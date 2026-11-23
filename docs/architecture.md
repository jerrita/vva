# 核心架构

## 1. 问题：最小公分母

通用 coding harness 为抹平供应商差异，把请求统一为无状态的 `/v1/chat/completions`，丢失各家原生能力：

| 供应商 | 丢失的能力 |
|---|---|
| OpenAI Responses | 服务端链式 `response_id`，多轮需重复上传历史 |
| Anthropic | Prompt Caching（`cache_control`）；Thinking 块的 `signature` 无法回传，思考链触发校验错误 |
| Gemini | 原生 Google Search Grounding 的字符偏移引用 |
| 跨供应商 | 切换模型时上下文格式割裂，无法接力 |

vva 在统一的 Session 抽象上保留各供应商的原生特性。设计约束见 [AGENTS.md §2](../AGENTS.md#2-设计约束)。

## 2. 分层

Cargo workspace 有两个包：应用 `vva` 与库 `crates/vva-core`，依赖方向为 `vva → vva-core`，`default-members` 同时包含两包。

```
应用 vva：main.rs · app/ · ui/ · web/ · space/     config/ 经 app/ 装配执行计划
   │ 依赖
   ▼
vva-core
├── harness/   SessionManager · SessionTask · ExecutionPlan
├── tools/     ToolRegistry 与各工具实现
├── provider/  Responses · Anthropic · Gemini
├── storage/   JSONL：Session → Turn → Message → Part
└── model/     领域模型，贯穿各层
```

**职责边界**

| 职责 | vva-core | 应用 vva |
|---|---|---|
| 会话与执行 | 四层实体、供应商元数据、`AgentEvent`、会话任务与句柄、只读快照、Agent Loop | 选择模型、创建哪种会话、自动标题等产品策略 |
| 模型调用 | 三家协议的原生实现，HTTP / SSE / WebSocket，能力声明 | 配置寻址、模型预设、用户选择与热重载 |
| 工具与后台工作 | `Tool` / `ToolRegistry`；文件、编辑、Shell、联网、AskUser、子代理、消息投递、后台任务与输出采集 | 注册哪些工具、子代理定义清单、交互是否可用、Space 专用工具 |
| 存储与文件解析 | `SessionStore`、JSONL、skill / agent 文件解析与目录扫描 | 默认存储路径、配置与偏好、技能目录优先级、内置 agent 清单 |
| 上下文 | 执行宿主提供的上下文处理；通用工具描述与后台完成回执格式 | coding system prompt、AGENTS.md、`@` 展开、技能注入策略 |
| 交互与服务 | 标准事件、只读快照、投递 / 回复 / 停止等公开 API | CLI、TUI、Web / SSE、frontend、日志与进程信号 |
| Agent Space | 所需的会话、工具、后台执行与持久化机制 | IM / QQ、router / focus、记忆 / curator、rollover、TTL、scheduler |

**core 的 feature**

- `response`、`anthropic`、`gemini`、`filesystem-tools`、`shell-tools`、`web-tools`、`jsonl`，默认全部关闭，应用开启全部。
- `web-tools` 联动 Anthropic 独立搜索与 Gemini 原生联网实现。
- 关闭 feature 只裁剪实现与依赖：历史元数据类型保留，协议与工具不变。
- 终端 / 剪贴板、Axum、静态资源、frontend 与 `build.rs` 只属于应用，单独构建 core 不执行 npm。
- 应用直接依赖 `vva_core`，不转发 core 模块。

**存储与展示的分界**

- core 的 JSONL v3 只保存通用会话；应用的 coding 配置与 Space 记录独立存储，见 [storage](storage.md)。
- Agent Space 通过公开会话 API 工作。应用的投影组合快照、应用记录与展示规则。

**各层职责**

- **配置 `config/`**（应用）：
  - 分层加载、JSONC 与环境变量展开、`{provider}/{model}` 寻址。
  - `resolve_model` 只做解析，返回 `ResolvedModel`。
  - Provider 由 `create_provider()` 在发起调用时构造；统计价格、列出模型清单等只读路径不创建适配器。
  - 见 [config-reference](config-reference.md)。
- **领域模型 `model/`**：四层实体、多模态内容、引用注脚、轮次级强类型元数据。见 [domain_model](domain_model.md)。
- **供应商适配 `provider/`**：实现 `Provider` trait，声明 `ProviderCapabilities`；协议事件经 SSE 转为标准 `AgentEvent`。见 [providers](providers.md)。
- **调度 `harness/`**：
  - 应用的 `VvaSessionSetup` 经 `AgentAssembly` 提供 Provider、工具注册表、补全参数与输入 / 工具后上下文。
  - `SessionTask` 是会话的唯一所有者，顺序处理提交、配置变更与取消，驱动 Agent Loop 并落盘。
  - `SessionManager` 管理已装配的会话与句柄；应用 facade `VvaSessionManager` 负责模型偏好与配置重载。
  - 见 [background_tasks](background_tasks.md)。
- **工具 `tools/`**：`ToolRegistry` 执行工具调用，结果配对后进入下一次请求。
- **存储 `storage/`**：`JsonlSessionStore` 行级追加写，崩溃容灾，流式读取。见 [storage](storage.md)。
- **交互**：CLI 负责单次执行与会话检查；TUI 与 Web 订阅 `SessionView`；Agent Space 驱动 IM。见 [ui](ui.md)、[web_sync](web_sync.md)、[agent](agent.md)。

## 3. 数据流

1. 交互层向 `ManagedSession` 提交输入。
2. `SessionTask` 调用宿主的 `SessionSetup::prepare_input` 处理输入，再取得 `ExecutionPlan`。coding 的 `@`、skills、AGENTS.md 策略由应用的 `VvaSessionSetup` 实现。
3. Provider 以原生协议请求，流式增量转为 `AgentEvent`。
4. 事件驱动 `SessionTask` 更新草稿，以 core `SessionView` 快照经 `watch` 发布，间隔 33ms。应用投影关联会话配置与 Space 记录，补充状态文案与 TUI 终态计时，再交给 TUI / Web / IM。
5. 工具调用由 `ToolRegistry` 执行，结果配对后进入下一次请求。
6. 每个完成的 Turn 追加写入 JSONL。供应商元数据随 Turn 保存，下一次请求按原生协议回传。

新增供应商的步骤见 [AGENTS.md §4](../AGENTS.md#4-开发指南)。

## 4. 外部消费与生命周期

[`examples/headless.rs`](../crates/vva-core/examples/headless.rs) 演示只用 core 的会话执行：自定义 `Tool`、`ToolRegistry`、`FixedSetup` 与 JSONL 存储，投递一次输入并等待包含后台续跑的回执。示例只开启 `response,jsonl`，无需 Node、终端、应用配置或 `~/.vva`。

```sh
cargo build -p vva-core --examples --features response,jsonl
OPENAI_API_KEY=... OPENAI_MODEL=... VVA_SESSION_DIR=/absolute/path/to/sessions \
  cargo run -p vva-core --example headless --features response,jsonl -- 'Uppercase hello using the tool.'
```

凭据、模型与存储目录由这三个环境变量提供，示例会联网调用 Responses。

**公开入口**

- `harness::{AgentAssembly, SessionSetup, FixedSetup, ExecutionPlan, SessionServices, SessionManager, ManagedSession, SessionView, DeliveryReceipt}`
- 领域模型，`Provider` / `Tool` / `SessionStore` 契约
- 应用在用的供应商适配器与工具装配

Task、Command、内部状态表、SSE 解码与输出采集的实现不公开。

**生命周期**

- `SessionManager::close(id)`：中止该任务的当前执行，取消交互与投递等待，等待标题任务、后台回执等待任务与后台 monitor 退出，再移出登记表。已持久化的历史保留。
- `SessionManager::shutdown()`：关闭登记入口并清空句柄表，对所有会话执行上述清理。单项失败时继续清理其余项，汇总错误返回。关闭后创建或打开会话报错。
- `abort_current()`：只中止当前轮次，会话任务与后台结果处理继续运行。后台 Shell monitor 被取消时释放输出管道，并经 `kill_on_drop` 终止它持有的子进程。
- core 不注册进程信号，也不退出进程，生命周期由宿主调用。应用 facade 转交 shutdown，在 CLI 单次执行结束、TUI 退出与 Space 停止时回收会话。
