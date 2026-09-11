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
- **OpenAI Responses API**：`ResponseTurnMeta`（`response_id` 链式绑定与 `ResponseOutputItem` 原生条目回传）
- **Anthropic Messages API**：`AnthropicTurnMeta`
- **Google Gemini API**：`GeminiTurnMeta`

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
| **终端交互** | [docs/ui.md](docs/ui.md) | Ratatui TUI 架构、会话任务与 `SessionView` 订阅、`InputBuffer` 多行编辑与模态框状态解耦 |

---

## 4. 最新代码目录结构

```
src/
├── lib.rs                  # 核心模块导出
├── event.rs                # AgentEvent 统一事件流词汇表 (Provider 流式增量 + Harness 工具/Notice/Aborted 事件)
├── system.rs               # 系统环境信息采集与系统提示组装 (日期 / OS / arch / workspace / scratch)
├── main.rs                 # CLI 命令行入口 (vva run [prompt], vva session list/inspect, vva web)
├── config/                 # 配置管理 (JSON/JSONC 解析、环境展开与路由)
│   ├── mod.rs              # load_config 自动寻址与分层合并 (含 FALLBACK_MODEL_SPEC)
│   ├── model.rs            # AppConfig, ProviderConfig (provider_type 为 Option，合并时仅显式声明才覆写), ModelConfig；glm-coding-plan 预设补全
│   ├── parser.rs           # strip_jsonc_comments 与 interpolate_env_vars
│   ├── resolver.rs         # resolve_model 只做配置解析并返回 ResolvedModel；Provider 由 ResolvedModel::create_provider 单独构造
│   └── state.rs            # 运行时偏好 (~/.vva/state.json)：上次选用的模型与推理档位
├── model/                  # 领域模型与强类型体系
│   ├── mod.rs              # 模块导出与通用类型
│   ├── meta.rs             # Response, Anthropic, Gemini 专属强类型轮次元数据 (含 ResponseOutputItem 原生条目链)
│   ├── content.rs          # 多模态 (Image)、搜索与 Annotation
│   ├── message.rs          # Role 与 Message (内容分组值对象，仅持有 parts)
│   ├── turn.rs             # Turn 与 TokenUsage
│   ├── session.rs          # Session 与 JSONL 记录定义 (SessionHeader / JsonlRecord)
│   └── tool.rs             # ToolCall, ToolResult, ToolDefinition
├── storage/                # 存储抽象与实现
│   ├── mod.rs              # SessionStore Trait
│   └── jsonl.rs            # JsonlSessionStore 按项目目录存储 (sessions/{project_key}/{sess_xxxxx}.jsonl + scratch/)
├── provider/               # 供应商适配器层 (原生协议直通)
│   ├── mod.rs              # Provider Trait (capabilities/complete/complete_stream)、能力声明与共享 HTTP Client (超时)
│   ├── common.rs           # 跨供应商共享辅助 (EventForwarder 背压转发 / require_* 严格读取 / RoleBlockMerger)
│   ├── http.rs             # 共享传输封装 (post_json 非流式 / post_sse 流式事件循环 SseEventHandler)
│   ├── response.rs         # OpenAI Responses API (previous_response_id + 原生搜索)
│   ├── anthropic.rs        # Anthropic Messages API (Prompt Caching + Thinking 签名，流式/非流式统一解析)
│   ├── gemini.rs           # Google Gemini API (Grounding 多 chunk 合并投影 + 代码沙盒 + 共享累积器)
│   └── sse.rs              # 通用标准 Server-Sent Events (SSE) 流式事件解码器 (仅 data: 行)
├── tools/                  # Coding 工具集
│   ├── mod.rs              # Tool Trait 与 ToolRegistry
│   ├── shell.rs            # Shell 执行 (超时/输出上限/退出码检查)
│   └── filesystem.rs       # Read、Edit、Glob、Grep
├── harness/                # 调度引擎与会话运行时
│   ├── mod.rs
│   ├── agent.rs            # AgentEngine 组装 (Provider + ToolRegistry + CompletionOptions) 与 capability_notices 能力协商
│   └── session.rs          # SessionManager 句柄表 + SessionTask 会话任务 (单一所有者顺序执行提交/配置/取消，按 revision 节流发布 SessionView 快照)
├── ui/                     # 现代化 Ratatui 交互式终端用户界面
    ├── app/                # App 响应式控制器、状态机与子系统调度
    │   ├── mod.rs          # App 结构体定义、生命周期与模块导出
    │   ├── session.rs      # 会话切换、SessionView 快照刷新与模态框状态调度
    │   ├── editor.rs       # 输入缓冲编辑、光标移动与斜杠命令补全
    │   ├── dispatch.rs     # 用户提交、斜杠命令路由与会话请求分发
    │   ├── pane.rs         # TextPane 可选中文本面板与 ChatCache 历史前缀缓存 (命中判定/选区/行缓存)
    │   ├── selection.rs    # TextCoord/TextSelection/SelectionTracker 与剪贴板
    │   ├── scroll.rs       # 视口滚动与坐标映射
    │   ├── tests.rs        # App 完整自动化单元测试集
    │   └── test_support.rs # 测试共享基建 (临时 JSONL 存储 + App 构造)
    ├── command.rs          # 斜杠命令系统与 Claude Code 风格前缀自动补全
    ├── history.rs          # SidebarStats 纯函数统计投影 (compute_stats)
    ├── input.rs            # InputBuffer 多行文本编辑、光标计算与 CJK 对齐算法
    ├── markdown.rs         # Markdown 解析与终端样式渲染 (pulldown-cmark)
    ├── text.rs             # 文本展示工具 (按显示宽度截断 truncate，CJK 双列)
    ├── modals/             # 独立的模态弹窗状态机与单测
    │   ├── mod.rs          # ActiveModal 枚举 (变体直接携带状态)、ModalAction、FilterableModal Trait 与按键归一化
    │   ├── sessions.rs     # SessionsModalState (按工程过滤与会话切换)
    │   ├── models.rs       # ModelsModalState (模型清单筛选)
    │   ├── variants.rs     # VariantsModalState (推理档位切换)
    │   └── config.rs       # ConfigModalState (设置项交互切换)
    └── views/              # 纯渲染层组件
        ├── mod.rs          # 视图模块导出
        ├── chat.rs         # 聊天流渲染 (已完成轮次 + 流式草稿同源渲染)
        ├── config_modal.rs # 设置弹窗视图
        ├── header.rs       # 顶部标题栏
        ├── help_modal.rs   # 帮助弹窗视图
        ├── input_area.rs   # 复合输入区视图
        ├── modal_shell.rs  # 模态弹窗公共外壳 (render_modal_shell 渲染骨架/居中布局/过滤行/滚动窗口/空态/提示行)
        ├── models_modal.rs # 模型选择弹窗视图
        ├── sessions_modal.rs # 会话选择弹窗视图
        ├── sidebar.rs      # 右侧统计看板
        └── variants_modal.rs # 推理档位弹窗视图
└── web/                    # Web 界面 (Axum 服务器)
    ├── mod.rs              # 模块导出 (run_server)
    ├── api.rs              # REST + SSE API (会话 CRUD/提交/中止/配置/事件流订阅/模型清单)
    └── server.rs           # 路由组装与静态资源服务 (rust_embed 内嵌前端 dist，SPA 回退)

frontend/                    # Web 前端源码 (React + Vite + TypeScript)；build.rs 构件期编译，产物 dist 由 rust_embed 打包进二进制
```

---

## 5. 后续开发与扩展指南

1. **新增模型/供应商适配**：
   - 在 `src/model/meta.rs` 中定义强类型结构体，并在 `TurnMeta` 中引入字段；
   - 在 `src/config/model.rs` 与 `src/config/resolver.rs` 中注册对应的 ProviderType 路由；
   - 在 `src/provider/` 中实现 `Provider` trait，在 `capabilities()` 中声明支持的原生能力；
   - 编写单测验证供应商专属特性（如 cache、signature、response_id、reasoning_effort）的双向转换保真性。
2. **多模态扩展**：
   - 读取图片/音视频文件时使用 `MediaSource::Path` 或 `MediaSource::Base64`；
   - 适配器将通用模态转为目标 API 兼容的 Payload（如 Gemini 的 `inline_data`，Anthropic 的 `base64` image source）。
3. **测试覆盖与规范约束**：
   - 修改核心模型、解析器、适配器或 UI 状态后，运行 `cargo test` 确保全量测试通过，并保持 `cargo clippy -- -D warnings` 零告警；
   - **单测 Mock 规范**：单测数据按照官方最新规范格式构造，不在生产适配器中增加未定义的兜底逻辑。
