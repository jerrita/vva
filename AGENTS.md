# AGENTS.md - vva 项目核心架构与开发指南

本文档记录 **vva (Vendor-Versatile Agent Harness)** 的核心设计原则、领域模型约定与扩展指南，作为后续 Agent 和开发者协作的基础锚定。

---

## 1. 项目定位与核心价值观

### 痛点与使命
现有通用 coding harness 存在共同弊端：**“最小公分母”（Lowest Common Denominator, LCD）问题**。
- 将所有模型强行退化为无状态的 `/v1/chat/completions`；
- 丢弃 OpenAI Responses API 的服务端链式 `response_id`，造成海量重复上下文上传；
- 丢弃 Anthropic 的 Prompt Caching (`cache_control`) 与 Thinking 块签名 (`signature`)；
- 将 Gemini 原生 Google Search Grounding 降级为外部搜索，丢失高质量网页引用（Citation token offset）；
- 跨供应商切换时上下文格式割裂。

**vva 的使命**：构建一个在保持统一 Session 抽象的同时，**最大化激发各供应商原生优势**的现代化 Coding Harness。

---

## 2. 核心架构锚点

### 2.1 强类型供应商元数据 (Strict Typed Meta)
> **铁律**：严禁退化为松散的 `HashMap<String, serde_json::Value>`。必须充分发挥 Rust 编译期类型安全与模式匹配优势。

目前**只支持且深度支持**三大核心供应商：
1. **OpenAI Responses API (`ResponseTurnMeta`, `ResponseSessionMeta`, `ResponseMessageMeta`)**：
   - 核心字段：`response_id`、`previous_response_id`、`status`、`system_fingerprint`、`service_tier`、`reasoning_effort`；
   - 行为：每次请求自动从 Session 提取最近有效的 `response_id` 作为 `previous_response_id` 发回，仅需上传增量消息。
2. **Anthropic Messages API (`AnthropicTurnMeta`, `AnthropicMessageMeta`)**：
   - 核心字段：`cache_creation_input_tokens`、`cache_read_input_tokens`、`stop_reason`、`ephemeral_cache`；
   - 行为：在系统指令及关键轮次自动注入 `cache_control: {"type": "ephemeral"}`；在多轮往返中严格原样保留 `thinking` 块关联的防篡改 `signature`。
3. **Google Gemini API (`GeminiTurnMeta`, `GeminiMessageMeta`)**：
   - 核心字段：`finish_reason`、`safety_ratings`、`code_execution_result`、`thought_signature`；
   - 行为：原生启用 `tools: [{ "google_search": {} }]`，并将返回的 `groundingMetadata` 投影为统一注脚。

### 2.2 四层级联领域模型与 Session 状态绑定
$$\text{Session} \longrightarrow \text{Turn} \longrightarrow \text{Message} \longrightarrow \text{ContentPart}$$

- **`Session` (与 SessionHeader)**：
  - 会话根实体，除工作空间路径 (`workspace_root`) 外，**强绑定持久化当前选定模型与推理强度**：
    - `current_model: String`（如 `"a/gpt-5.6-terra"`）；
    - `current_variant: Option<String>`（如 `Some("high")`，映射至厂商 reasoning effort）；
  - 断点续跑或会话切换时自动无缝继承，支持 CLI 动态热切换。
- **`Turn`**：交互轮次与计费/状态锚点，包含 `parent_turn_id`（支持分支树）、角色 (`Role`)、消息流 (`Vec<Message>`)、Token 消耗核算 (`TokenUsage`) 及专属元数据 (`TurnMeta`)。
- **`Message`**：标准协议消息，角色支持 `System`, `User`, `Assistant`, `Tool`。
- **`ContentPart` (多模态一等公民)**：
  - `Text`: 文本内容，内嵌引用注脚 `annotations: Vec<Annotation>`；
  - `Reasoning`: 深度思考过程，携带可选的数字签名 `signature`；
  - `Image` / `Document` (PDF) / `Audio` / `Video`: 强类型多模态支持（`Url` / `Base64` / `Path`）；
  - `ToolCall` / `ToolResult`: Coding 工具调用与返回封装；
  - `WebSearchCall`: 结构化记录检索意图、状态与数据源。

### 2.3 统一配置系统 (JSON / JSONC + 环境变量展开)
配置文件（如 `config.example.jsonc` / `~/.vva/config.jsonc`）：
- **同时支持 JSON 与 JSONC**：词法剥离 `//` 与 `/* */` 注释，保留字符串内部内容；
- **环境变量插值**：自动将 `"${ENV_VAR}"` 展开为对应系统环境变量；
- **模型规格寻址 (`{provider_id}/{model_id}`)**：
  - 支持在具体模型节点覆写协议 `type`（如在 `response` 网关节点下为特定模型声明 `anthropic` 协议）；
  - 支持声明与校验推理档位 `variants`（如 `["minimal", "low", "medium", "high", "xhigh", "max"]`）；
  - 自动规范化 URL 端点（自动补全 `/v1` 或 `/v1beta`）。

### 2.4 以 Responses 规范为基准的 Web Search 与注脚
统一参考 OpenAI Responses 规范：
- 检索行为统一落地为 `ContentPart::WebSearchCall`（包含 `query`, `status`, `sources`）；
- 文本内联引用统一使用 `Annotation::UrlCitation`：
  ```rust
  Annotation::UrlCitation {
      url: String,
      title: String,
      start_index: usize, // 字符起始偏移
      end_index: usize,   // 字符结束偏移
  }
  ```
- Gemini 的 `groundingChunks` + `groundingSupports` 在适配器层直接计算字符偏移并投影为 `Annotation::UrlCitation`。

### 2.5 JSONL Append-Only 存储引擎
文件路径：`~/.vva/sessions/{session_id}.jsonl`
- **第 1 行**：`JsonlRecord::Header(SessionHeader)`（含 `current_model` 与 `current_variant`）
- **第 2..N 行**：`JsonlRecord::Turn(Turn)`
- **特性**：追加写（无需重写整个历史文件）、进程中断容灾、清晰易读的 Git diff 与零拷贝行级流式加载。

---

## 3. 代码目录结构

```
src/
├── lib.rs              # 核心模块导出
├── main.rs             # CLI 命令行入口 (vva run, vva session list/inspect)
├── config/             # 配置管理 (JSON/JSONC 解析、环境展开与路由)
│   ├── mod.rs          # load_config 自动寻址
│   ├── model.rs        # AppConfig, ProviderConfig, ModelConfig 强类型
│   ├── parser.rs       # strip_jsonc_comments 与 interpolate_env_vars
│   └── resolver.rs     # resolve_model 路由与 Provider 动态构造
├── model/              # 领域模型与强类型体系
│   ├── meta.rs         # Response, Anthropic, Gemini 专属元数据
│   ├── content.rs      # 多模态 (Image/PDF/Audio/Video)、搜索与 Annotation
│   ├── message.rs      # Message 与 Role
│   ├── turn.rs         # Turn 与 TokenUsage
│   ├── session.rs      # Session 与 JSONL 记录定义 (含 model 与 variant 绑定)
│   └── tool.rs         # ToolCall, ToolResult, ToolDefinition
├── storage/            # 存储抽象与实现
│   ├── mod.rs          # SessionStore Trait
│   └── jsonl.rs        # JsonlSessionStore 流式追加引擎
├── provider/           # 供应商适配器层
│   ├── mod.rs          # Provider Trait 与能力声明 (含 reasoning_effort)
│   ├── response.rs     # OpenAI Responses API (previous_response_id + 原生搜索)
│   ├── anthropic.rs    # Anthropic Messages API (Prompt Caching + Thinking 签名)
│   └── gemini.rs       # Google Gemini API (Grounding 投影 + 代码沙盒)
├── search/             # 统一搜索调度 (原生直通 / 外置降级)
│   └── mod.rs
├── tools/              # Coding 工具集
│   ├── mod.rs          # Tool Trait 与 ToolRegistry
│   ├── bash.rs         # Shell 执行
│   └── file.rs         # 文件读写与查看
├── harness/            # 调度引擎
│   ├── mod.rs
│   └── agent.rs        # 多轮 Agent 循环驱动 (AgentEngine)
└── ui/                 # 现代化 Ratatui 交互式终端用户界面
    ├── mod.rs          # TUI 启动与事件主循环驱动 (run_tui)
    ├── app.rs          # App 响应式状态管理、历史加载与侧边栏统计
    ├── command.rs      # 斜杠命令系统与 Claude Code 风格前缀自动补全
    ├── session_manager.rs # 全局 Session 单例与并发后台运行管理
    └── views/          # 模块化渲染组件 (header, chat, sidebar, input_area, sessions_modal, help_modal)
```

---

## 4. 后续开发与扩展指南

1. **新增模型/供应商适配**：
   - 必须先在 `src/model/meta.rs` 中定义强类型结构体，并在 `SessionMeta` / `TurnMeta` / `MessageMeta` 中显式引入字段；
   - 在 `src/config/model.rs` 与 `src/config/resolver.rs` 中注册对应的 ProviderType 路由；
   - 在 `src/provider/` 中实现 `Provider` trait，显式在 `capabilities()` 中声明支持的原生能力；
   - 编写单测验证供应商专属特性（如 cache、signature、response_id、reasoning_effort）的双向转换保真性。
2. **多模态扩展**：
   - 读取图片/音视频文件时统一使用 `MediaSource::Path` 或 `MediaSource::Base64`；
   - 适配器负责将通用模态转为目标 API 兼容的 Payload（如 Gemini 的 `inline_data`，Anthropic 的 `base64` image source）。
3. **保持测试覆盖**：
   - 每次修改核心模型、解析器、适配器或 UI 状态后，必须运行 `cargo test` 确保全量测试（目前 15/15）通过，并保持 0 warning。
