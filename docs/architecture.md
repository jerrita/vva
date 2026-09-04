# 核心架构与设计原则 (Core Architecture & Design Principles)

## 1. 背景与痛点：拒绝“最小公分母”（Lowest Common Denominator, LCD）

现有通用 AI Coding Harness（如各类聚合网关与传统客户端）在多模型支持上面临共同的结构性缺陷：**“最小公分母”退化**。
为了抹平不同供应商之间的协议差异，通常将所有请求强行退化为无状态的 `/v1/chat/completions`：

1. **OpenAI Responses API 优势丧失**：
   - 丢弃了服务端的链式状态追踪 `response_id`，导致多轮对话必须反复全量上传数万 Token 的历史上下文，引发海量重复带宽消耗与计费激增。
2. **Anthropic 核心优势被剔除**：
   - 丢弃了官方的 Prompt Caching (`cache_control`) 标记，无法享受 90% 的输入成本节约与首字延迟降低；
   - 丢弃了思考块（Thinking）关联的原生防篡改数字签名 (`signature`)，导致多轮思考链无法在后续往返中原样回传，直接触发上游校验错误。
3. **Google Gemini 原生能力降级**：
   - 将 Gemini 强大的原生 Google Search Grounding 退化为普通的外部检索插件，彻底丢失了精准的字符偏移量网页引用（Citation Token Offset）。
4. **供应商切换时的断层**：
   - 切换模型时由于上下文格式割裂，往往需要清空会话或重新初始化，无法实现跨模型无缝接力。

**vva (Vendor-Versatile Agent Harness)** 的使命是：**在维持统一 Session 交互抽象的同时，最大化激发各大供应商官方协议的原生优势**。

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

### 2.1 统一存储与“零兜底”铁律 (Unified Storage & Zero Fallback Rule)
> **铁律**：**存储是统一且纯净的，不存在任何降级或兜底行为。** 每个供应商必须有纯正、独特的原生协议适配转换，以及完整的专属强类型 Meta 存储。

1. **统一领域存储 (Unified Domain Storage)**：
   - 持久化层只保存领域模型实体（`Session`, `Turn`, `Message`, `ContentPart`）与各供应商的强类型元数据，与私有请求/响应协议彻底解耦。
2. **适配器层零兜底 (Zero Fallback in Provider Adapters)**：
   - 每个 Provider 严格且仅实现官方原生协议规范。
   - **严禁跨协议伪造退化**：严禁在 Responses API 适配器中退化检查 Chat Completions 的 `choices` 字段，严禁任何形式的伪造推导；
   - **严禁非标字段容错链**：严禁出现类似 `"output_text" | "text"`、`"web_search_call" | "web_search"` 或 `.or_else(|| json["reasoning_effort"])` 等多重猜测链。官方文档未定义的字段，绝对严禁在代码中臆造。
3. **UI / Harness 调度层零兜底**：
   - UI 纯粹由标准事件流（`AgentEvent`）驱动渲染，严禁在 `AgentEvent::Finished` 中编写强行从会话历史覆盖聊天记录的“兜底同步”补丁。如果界面未展现，必须直接排查适配器转换根因。
4. **单测 Mock 真实性铁律**：
   - 单元测试中的 Mock 报文必须 100% 严谨对照厂商官方标准规范构造，**严禁为了迎合手写错误的测试数据而在生产代码中妥协添加容错分支**。

### 2.2 强类型供应商元数据 (Strict Typed Meta)
拒绝弱类型的 `HashMap<String, serde_json::Value>`。系统针对三大供应商定义了清晰的编译期强类型元数据：
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
   - 处理各家独特的双向协议转换，通过泛型 SSE 流解码输出标准 `AgentEvent`。
4. **执行与调度层 (`src/harness/`, `src/tools/`)**：
   - `AgentEngine` 驱动多轮次 Agent Loop，自动处理模型推理、工具调用（Bash、文件读写等）、结果回传以及历史持久化。
5. **存储层 (`src/storage/`)**：
   - `JsonlSessionStore` 提供行级追加写的持久化方案，支持崩溃容灾与极速历史拉取。
6. **交互层 (`src/ui/`, `src/main.rs`)**：
   - CLI 支持单次会话执行与检查；
   - 现代化 Ratatui TUI 基于单例 `SessionManager` 实现非阻塞的后台运行与实时渲染。
