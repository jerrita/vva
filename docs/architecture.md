# 核心架构与设计原则 (Core Architecture & Design Principles)

## 1. 背景与痛点：“最小公分母”（Lowest Common Denominator, LCD）问题

通用 AI Coding Harness 在多模型支持上面临设计缺陷：“最小公分母”退化。
为了抹平不同供应商之间的协议差异，将请求退化为无状态的 `/v1/chat/completions`：

1. **OpenAI Responses API 优势丧失**：
   - 丢弃服务端链式状态追踪 `response_id`，多轮对话需重复上传历史上下文，增加网络传输与 Token 消耗。
2. **Anthropic 核心优势被剔除**：
   - 丢弃 Prompt Caching (`cache_control`) 标记，增加输入 Token 成本与首字延迟；
   - 丢弃思考块（Thinking）关联的防篡改数字签名 (`signature`)，导致多轮思考链在后续往返中无法原样回传，触发上游校验错误。
3. **Google Gemini 原生能力降级**：
   - 将 Gemini 原生 Google Search Grounding 替换为外部检索，丢失字符偏移量网页引用（Citation Token Offset）。
4. **供应商切换时的断层**：
   - 切换模型时由于上下文格式割裂，需要清空会话或重新初始化，无法跨模型接力。

**vva (Vendor-Versatile Agent Harness)** 的目标：**在维持统一 Session 交互抽象的同时，支持各大供应商官方协议原生特性**。

---

## 2. 核心架构设计原则

```
+-------------------------------------------------------------------------+
|                        CLI / TUI Interaction Layer                      |
|             (main.rs / ui/mod.rs / ui/app.rs / ui/input.rs)             |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|                        Harness & Session Layer                          |
|         (ui/session_manager.rs / harness/agent.rs / tools/mod.rs)       |
+-------------------+--------------------------------+--------------------+
                    |                                |
                    v                                v
+---------------------------------------+  +------------------------------+
|       Vendor Provider Adapters        |  |     Unified Domain Store     |
|   (Responses / Anthropic / Gemini)    |  | (storage/jsonl.rs -> .jsonl) |
| - Native Protocols & SSE Streaming    |  | - Session -> Turn -> Message |
| - Zero Fallback & Typed Metadata      |  | - Strict Typed TurnMeta/Meta |
+---------------------------------------+  +------------------------------+
```

### 2.1 统一存储与“零兜底”约束 (Unified Storage & Zero Fallback Rule)
> **设计原则**：**存储保持统一，不进行降级或兜底。** 各供应商实现原生协议适配转换与专属强类型 Meta 存储。

1. **统一领域存储 (Unified Domain Storage)**：
   - 持久化层保存领域模型实体（`Session`, `Turn`, `Message`, `ContentPart`）与各供应商强类型元数据，与私有请求/响应协议解耦。
2. **适配器层零兜底 (Zero Fallback in Provider Adapters)**：
   - 每个 Provider 实现官方原生协议规范。
   - **禁止跨协议伪造退化**：Responses API 适配器不检查 Chat Completions 的 `choices` 字段，不进行数据伪造与推导；
   - **禁止非标字段猜测链**：不编写类似 `"output_text" | "text"`、`"web_search_call" | "web_search"` 或 `.or_else(|| json["reasoning_effort"])` 等猜测链。官方文档未定义的字段，不在代码中编写猜测逻辑。
3. **UI / Harness 调度层零兜底**：
   - UI 由标准事件流（`AgentEvent`）驱动渲染，不在 `AgentEvent::Finished` 中从会话历史覆盖聊天记录。界面未展现内容时排查适配器转换逻辑。
4. **单测 Mock 真实性约束**：
   - 单元测试 Mock 报文对照厂商官方标准规范构造，不在生产代码中为错误测试数据添加容错分支。

### 2.2 强类型供应商元数据 (Strict Typed Meta)
系统为三大供应商定义编译期强类型元数据，不使用弱类型的 `HashMap<String, serde_json::Value>`：
- **OpenAI Responses API**：`ResponseTurnMeta`（`response_id`, `previous_response_id`, `status`, `service_tier`, `system_fingerprint`, `reasoning_effort`）
- **Anthropic Messages API**：`AnthropicTurnMeta`（`cache_creation_input_tokens`, `cache_read_input_tokens`, `stop_reason`）、`AnthropicMessageMeta`（`ephemeral_cache`）
- **Google Gemini API**：`GeminiTurnMeta`（`finish_reason`, `safety_ratings`, `code_execution_result`）、`GeminiMessageMeta`（`thought_signature`）

---

## 3. 系统分层与调用拓扑

1. **配置层 (`src/config/`)**：
   - 负责读取并规范化用户配置文件（JSON/JSONC 注释剥离、环境变量插值展开）；
   - 根据模型标识符（如 `prov/model-id`）解析目标 Provider 配置、有效端点类型及推理强度档位。
2. **领域模型层 (`src/model/`)**：
   - 统一四层级联领域模型，保证业务逻辑与供应商通信格式解耦；
   - 包含多模态载荷、工具调用与引用注脚。
3. **供应商适配层 (`src/provider/`)**：
   - 实现 `Provider` trait，声明原生能力集（`ProviderCapabilities`）；
   - 处理各家协议转换，通过泛型 SSE 流解码输出标准 `AgentEvent`。
4. **执行与调度层 (`src/harness/`, `src/tools/`)**：
   - `AgentEngine` 驱动多轮次 Agent Loop，处理模型推理、工具调用（Bash、文件读写等）、结果回传以及历史持久化。
5. **存储层 (`src/storage/`)**：
   - `JsonlSessionStore` 提供行级追加写的持久化方案，支持崩溃容灾与流式历史读取。
6. **交互层 (`src/ui/`, `src/main.rs`)**：
   - CLI 支持单次会话执行与检查；
   - Ratatui TUI 基于单例 `SessionManager` 实现非阻塞后台运行与实时渲染。
