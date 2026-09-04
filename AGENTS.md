# AGENTS.md - vva 项目核心架构与开发指南

本文档记录 **vva (Vendor-Versatile Agent Harness)** 的核心设计原则、设计文档索引与开发指南，作为 Agent 与开发者协作规范。

---

## 1. 项目定位与核心原则

### 痛点与目标
通用 coding harness 存在“最小公分母”（Lowest Common Denominator, LCD）问题：
- 将模型退化为无状态的 `/v1/chat/completions`；
- 丢弃 OpenAI Responses API 服务端链式 `response_id`，增加重复上下文上传；
- 丢弃 Anthropic Prompt Caching (`cache_control`) 与 Thinking 签名 (`signature`)；
- 将 Gemini 原生 Google Search Grounding 降级为外部搜索，丢失网页引用（Citation token offset）；
- 切换供应商时上下文格式割裂。

**vva 的目标**：在保持统一 Session 抽象的同时，支持各供应商原生特性的 Coding Harness。

---

## 2. 核心架构与设计约束

### 2.1 统一存储与“零兜底”约束 (Unified Storage & Zero Fallback Rule)
> **核心原则**：统一领域存储，不进行兜底降级。各供应商实现原生协议适配转换与专属强类型 Meta 存储。

1. **统一领域存储 (Unified Domain Storage)**：
   - 遵循四层级联实体结构：$$\text{Session} \longrightarrow \text{Turn} \longrightarrow \text{Message} \longrightarrow \text{ContentPart}$$
   - 底层持久化（JSONL）保存领域模型实体与强类型元数据，与厂商私有请求/响应报文解耦。
2. **零兜底与零降级约束 (Zero Fallback & Zero LCD Degradation)**：
   - **适配器层零兜底**：Provider 仅实现厂商官方原生协议规范。
     - **禁止协议退化**：Responses API 适配器不检查 Chat Completions 的 `choices` 字段，不进行数据伪造与猜测；
     - **禁止非标字段别名猜测**：不编写 `"output_text" | "text"`、`"web_search_call" | "web_search"` 或 `.or_else(|| json["reasoning_effort"])` 等猜测链。厂商官方规范未定义的字段，不在代码中臆造；
   - **UI / Harness 调度层零兜底**：UI 由标准事件流（`AgentEvent`）驱动渲染，不编写从会话历史覆盖聊天记录的同步补丁。界面未展示时排查适配器转换逻辑；
   - **单测 Mock 真实性约束**：单元测试 Mock 报文对照官方标准文档构造，不在生产代码中为错误测试数据添加容错分支。

### 2.2 强类型供应商元数据 (Strict Typed Meta)
> **类型系统约束**：使用强类型结构体，不使用松散的 `HashMap<String, serde_json::Value>`。

为核心供应商定义专属强类型元数据（见 `src/model/meta.rs`）：
- **OpenAI Responses API**：`ResponseTurnMeta`, `ResponseSessionMeta`, `ResponseMessageMeta`
- **Anthropic Messages API**：`AnthropicTurnMeta`, `AnthropicSessionMeta`, `AnthropicMessageMeta`
- **Google Gemini API**：`GeminiTurnMeta`, `GeminiSessionMeta`, `GeminiMessageMeta`

---

## 3. 设计文档索引 (Docs Reference)

技术设计方案参见 `docs/` 目录：

| 模块 | 设计文档 | 核心内容摘要 |
|---|---|---|
| **架构全局** | [docs/architecture.md](docs/architecture.md) | 痛点分析、LCD 陷阱防范、整体数据流与分层调用拓扑 |
| **领域模型** | [docs/domain_model.md](docs/domain_model.md) | 四层级联实体架构、多模态一等公民、引用注脚系统与强类型元数据 |
| **持久化存储** | [docs/storage.md](docs/storage.md) | JSONL 追加写存储引擎、`Header` 与 `Turn` 记录规范、抗崩溃容灾 |
| **供应商适配** | [docs/providers.md](docs/providers.md) | Responses 链式继承、Anthropic 缓存与签名、Gemini 搜索注脚与通用 SSE 解码 |
| **配置路由** | [docs/configuration.md](docs/configuration.md) | JSON/JSONC 词法注释剥离、`${VAR}` 环境变量展开、模型规格寻址与端点规范化 |
| **终端交互** | [docs/ui.md](docs/ui.md) | Ratatui TUI 架构、`SessionManager` 单例并发、`InputBuffer` 多行编辑与模态框状态解耦 |

---

## 4. 最新代码目录结构

```
src/
├── lib.rs                  # 核心模块导出
├── main.rs                 # CLI 命令行入口 (vva run, vva session list/inspect)
├── config/                 # 配置管理 (JSON/JSONC 解析、环境展开与路由)
│   ├── mod.rs              # load_config 自动寻址与分层合并
│   ├── model.rs            # AppConfig, ProviderConfig, ModelConfig 强类型定义
│   ├── parser.rs           # strip_jsonc_comments 与 interpolate_env_vars
│   └── resolver.rs         # resolve_model 寻址路由与 Provider 动态构造
├── model/                  # 领域模型与强类型体系
│   ├── mod.rs              # 模块导出与通用类型
│   ├── meta.rs             # Response, Anthropic, Gemini 专属强类型元数据
│   ├── content.rs          # 多模态 (Image/PDF/Audio/Video)、搜索与 Annotation
│   ├── message.rs          # Message 与 Role
│   ├── turn.rs             # Turn 与 TokenUsage
│   ├── session.rs          # Session 与 JSONL 记录定义 (含 model 与 variant 绑定)
│   └── tool.rs             # ToolCall, ToolResult, ToolDefinition
├── storage/                # 存储抽象与实现
│   ├── mod.rs              # SessionStore Trait
│   └── jsonl.rs            # JsonlSessionStore 行级追加写引擎
├── provider/               # 供应商适配器层 (原生协议直通)
│   ├── mod.rs              # Provider Trait 与能力声明
│   ├── response.rs         # OpenAI Responses API (previous_response_id + 原生搜索)
│   ├── anthropic.rs        # Anthropic Messages API (Prompt Caching + Thinking 签名)
│   ├── gemini.rs           # Google Gemini API (Grounding 投影 + 代码沙盒)
│   └── sse.rs              # 通用标准 Server-Sent Events (SSE) 流式事件解码器
├── search/                 # 统一搜索调度 (原生直通 / 外置降级)
│   └── mod.rs
├── tools/                  # Coding 工具集
│   ├── mod.rs              # Tool Trait 与 ToolRegistry
│   ├── bash.rs             # Shell 执行 (Unix)
│   ├── powershell.rs       # PowerShell 执行 (Windows)
│   └── file.rs             # 文件读写与查看
├── harness/                # 调度引擎
│   ├── mod.rs
│   └── agent.rs            # 多轮 Agent 循环驱动 (AgentEngine)
└── ui/                     # 现代化 Ratatui 交互式终端用户界面
    ├── mod.rs              # TUI 启动与事件主循环驱动 (run_tui)
    ├── app.rs              # App 响应式控制器、Session 切换调度与斜杠命令执行
    ├── command.rs          # 斜杠命令系统与 Claude Code 风格前缀自动补全
    ├── history.rs          # ChatEntry 对话流与 SidebarStats 纯函数视图投影
    ├── input.rs            # InputBuffer 多行文本编辑、光标计算与 CJK 对齐算法
    ├── modals/             # 独立的模态弹窗状态机与单测
    │   ├── mod.rs          # ActiveModal 枚举及子模块导出
    │   ├── sessions.rs     # SessionsModalState (按工程过滤与会话切换)
    │   ├── models.rs       # ModelsModalState (模型清单筛选)
    │   ├── variants.rs     # VariantsModalState (推理档位切换)
    │   └── config.rs       # ConfigModalState (设置项交互切换)
    ├── session_manager.rs  # 全局 Session 单例与并发后台运行管理
    └── views/              # 纯渲染层组件
        ├── mod.rs          # 视图模块导出
        ├── chat.rs         # 聊天流渲染
        ├── config_modal.rs # 设置弹窗视图
        ├── header.rs       # 顶部标题栏
        ├── help_modal.rs   # 帮助弹窗视图
        ├── input_area.rs   # 复合输入区视图
        ├── models_modal.rs # 模型选择弹窗视图
        ├── sessions_modal.rs # 会话选择弹窗视图
        ├── sidebar.rs      # 右侧统计看板
        └── variants_modal.rs # 推理档位弹窗视图
```

---

## 5. 后续开发与扩展指南

1. **新增模型/供应商适配**：
   - 在 `src/model/meta.rs` 中定义强类型结构体，并在 `SessionMeta` / `TurnMeta` / `MessageMeta` 中引入字段；
   - 在 `src/config/model.rs` 与 `src/config/resolver.rs` 中注册对应的 ProviderType 路由；
   - 在 `src/provider/` 中实现 `Provider` trait，在 `capabilities()` 中声明支持的原生能力；
   - 编写单测验证供应商专属特性（如 cache、signature、response_id、reasoning_effort）的双向转换保真性。
2. **多模态扩展**：
   - 读取图片/音视频文件时使用 `MediaSource::Path` 或 `MediaSource::Base64`；
   - 适配器将通用模态转为目标 API 兼容的 Payload（如 Gemini 的 `inline_data`，Anthropic 的 `base64` image source）。
3. **测试覆盖与规范约束**：
   - 修改核心模型、解析器、适配器或 UI 状态后，运行 `cargo test` 确保全量测试通过，并保持 `cargo clippy -- -D warnings` 零告警；
   - **单测 Mock 规范**：单测数据按照官方最新规范格式构造，不在生产适配器中增加未定义的兜底逻辑。
