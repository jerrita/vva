# vva core 拆分方案

本文是待实施的设计，基于当前 `vva 1.16.0`；API 名称为草图。本轮只修改本文。

## 1. 目标与边界

只保留两个 Cargo package：根目录 `vva` 应用与 `crates/vva-core` 库，依赖方向为 `vva → vva-core`。
`provider`、`tools`、`storage` 都是 core 内部模块，不再拆独立 crate。

| 内容 | vva-core | 根 package vva |
|---|---|---|
| 会话与执行 | Session / Turn / Message / ContentPart、强类型供应商元数据、AgentEvent、manager / task / handle / view、Agent Loop | 选择模型、创建哪种会话、自动标题等产品策略 |
| 模型调用 | Responses / Anthropic / Gemini 原生实现，HTTP / SSE / WebSocket、能力声明 | 配置寻址、模型预设、用户选择与热重载 |
| 工具与异步工作 | Tool / ToolRegistry；文件、编辑、Shell、联网、AskUser、子代理、消息投递、后台任务与输出采集 | 注册哪些工具、子代理定义清单、交互是否可用；Space 专用工具 |
| 存储与文件解析 | SessionStore、JSONL；skill / agent 文件解析与显式目录的发现函数 | 默认存储路径、配置文件与偏好、技能目录优先级和内置 agent 清单 |
| 上下文 | 执行应用提供的上下文处理，通用工具描述与后台完成回执格式 | coding system prompt、AGENTS.md、`@` 展开、技能调用与注入策略 |
| 交互与服务 | 标准事件、只读快照、程序化投递 / 回复 / 停止等公开 API | CLI、TUI、Web / SSE 投影、frontend、日志初始化与进程信号 |
| Space | 提供其需要的会话、工具、后台执行和持久化机制 | IM / QQ、router / focus、记忆 / curator、rollover、TTL、scheduler、渠道进度与出站消息 |

Space 已是一个常驻个人 agent 应用，作为 core 的第一个消费者继续留在 vva，只能使用 core 公开 API。
第二个同类应用出现后，再判断哪些渠道、记忆或调度实现值得共享；此次不为它们预设公共框架。

core 的领域模型、事件、运行时及 Tool / Provider / SessionStore 契约始终可用。
建议可选 feature 为 `response`、`anthropic`、`gemini`、`filesystem-tools`、`shell-tools`、`web-tools`、`jsonl`；默认关闭，vva 显式开启现有功能。
子代理、后台登记和通用投递不依赖文件 / Shell feature；`shell-tools` 复用这些机制。原生联网工具联动声明对应 provider feature。
feature 只裁剪实现与依赖，不裁剪历史中的供应商元数据类型，不把“未编译此能力”变成运行时换协议或换工具。
终端、剪贴板、Axum、rust-embed、frontend 与 `build.rs` 只属于根应用；构建 core 不执行 npm。

## 2. 必须切断的耦合点

| 具体位置 | 必须改的边界 |
|---|---|
| [`model/session.rs`](../../src/model/session.rs) | `origin`、`kind=curator`、`routed_by`、TTL、`last_activity`、沉淀标记迁到应用的 `SpaceSessionRecord`，按 SessionId 关联；删除 `agent_managed()` / `supports_curation()`。父子和续接关系可留 core；编辑协议 / web 开关的选择归应用会话配置。 |
| [`harness/agent.rs`](../../src/harness/agent.rs) | `AgentEngine::new` 不再读 AppConfig 或固定注册默认工具，不判断 IM 身份，不过滤 curator 名称；执行应用提供的 provider / options / registry。原生与客户端 web 工具的选择、捆绑权限由应用装配明确决定。 |
| [`agents/mod.rs`](../../src/agents/mod.rs)、[`discover.rs`](../../src/discover.rs)、[`skills/mod.rs`](../../src/skills/mod.rs) | 去掉 `builtin_agents → space::curator` 依赖；解析与显式路径扫描进 core，`~/.vva` / `.agents` 搜索次序和内置清单留应用。保留 SKILL.md 未知 frontmatter / 非字符串 metadata 告警忽略的既有例外。 |
| [`harness/session/task.rs`](../../src/harness/session/task.rs)、[`harness/mention.rs`](../../src/harness/mention.rs)、[`system.rs`](../../src/system.rs) | task 不拼 coding prompt，不发现 AGENTS.md / skills，不解释 `@` 或按 Read 工具名加载目录指令；通过 §3 的装配与上下文入口处理。通用后台 reminder 格式移入 core，退出 coding system 模块。 |
| [`harness/session/curation.rs`](../../src/harness/session/curation.rs)、[`harness/session/handle.rs`](../../src/harness/session/handle.rs) | `SetSessionCuration` 工具、`SetCuration` 命令和 Space 活动时间维护移到应用；Space 用公开投递与完成回执更新自己的记录，不直接操纵 SessionTask / Command。 |
| [`harness/session/manager.rs`](../../src/harness/session/manager.rs) | 去掉 AppConfig / AppState、配置路径、偏好写入、默认模型选择、配置重载与自动创建项目目录；应用 facade 负责这些工作，core 只管理明确提供的会话与装配。 |
| [`config/resolver.rs`](../../src/config/resolver.rs)、[`config/model.rs`](../../src/config/model.rs) | ProviderType、InputCapabilities 等执行必需类型进 core；AppConfig、预设、合并和文件加载留应用。provider / Read / 联网工具不能再反向引用 AppConfig。 |
| [`tools/shell.rs`](../../src/tools/shell.rs)、[`tools/web/mod.rs`](../../src/tools/web/mod.rs)、[`storage/mod.rs`](../../src/storage/mod.rs) | Shell 接收明确 env，不自行读 `.vva/env`；联网工具接收已解析凭据 / provider；scratch / tool_outputs 由应用准备后传入，移出 SessionStore 必需方法。 |
| [`provider/http.rs`](../../src/provider/http.rs)、[`provider/websocket.rs`](../../src/provider/websocket.rs)、[`tools/web/zhipu/mcp.rs`](../../src/tools/web/zhipu/mcp.rs) | vva 的 OpenRouter 归属头和 MCP clientInfo 改由宿主显式传入，库版本不冒充宿主版本。 |
| [`harness/session/view.rs`](../../src/harness/session/view.rs)、[`model/background.rs`](../../src/model/background.rs)、[`web/api.rs`](../../src/web/api.rs) | 状态文案、TUI 终态展示计时归应用投影；Web 引用的 `ui/history` 统计移到应用共用模块。core 保留运行事实与快照，不携带侧栏规则。 |

当前 harness 没有直接调用 ratatui / Axum；主要问题是同包构建依赖和内置应用策略。
保留现有执行引擎，不为拆分另写 Agent Loop，也不将整个 config / system / space 目录搬入 core。

## 3. core 对外 API 草图

统一入口为 `AgentAssembly`：应用提供初始会话、提示词和每会话装配对象；新建与冷加载均明确提供装配。
以下省略已有便利方法与完整字段，不表示现有源码已提供这些 API。

```rust,ignore
pub struct AgentAssembly {
    pub session: Session, // 含应用构造的初始 system / 上下文轮次
    pub setup: Arc<dyn SessionSetup>,
}
pub struct ExecutionPlan {
    pub provider: Arc<dyn Provider>,
    pub options: CompletionOptions,
    pub inputs: InputCapabilities,
    pub tools: ToolRegistry,
    // 明确的交互模式、能力通知等已有执行信息省略
}
pub struct PreparedInput {
    pub before: Vec<Message>, // 用户上下文，按序追加
    pub message: Message,
    pub after: Vec<Message>,
}
#[async_trait]
pub trait SessionSetup: Send + Sync {
    async fn plan(&self, session: &Session, services: &SessionServices)
        -> Result<ExecutionPlan, SetupError>;
    async fn prepare_input(&self, session: &Session, input: Message)
        -> Result<PreparedInput, SetupError>;
    async fn after_tools(&self, session: &Session, batch: &[ToolExecution])
        -> Result<Vec<Message>, SetupError>;
}
impl SessionManager {
    pub fn new(store: Arc<dyn SessionStore>) -> Self;
    pub async fn create(&self, assembly: AgentAssembly) -> Result<SessionHandle, HarnessError>;
    pub async fn open(&self, id: &str, setup: Arc<dyn SessionSetup>)
        -> Result<SessionHandle, HarnessError>;
    // live / close / shutdown 等生命周期操作省略
}
```

这里只新增一个策略 trait：`SessionSetup`，对应现有代码真实需要替换的装配、输入处理、工具后上下文三个位置。
`VvaSessionSetup` 实现模型解析、工具选择、技能刷新、`@` 和目录 AGENTS.md；core 提供 `FixedSetup`，固定执行计划、原样输入、不追加工具后上下文，供简单宿主使用。
setup 由各 SessionTask 持有；同一会话的 provider Arc 和 hashline 状态随会话保留，不能每次 plan 重建 WebSocket，也不能跨会话误用有状态连接。
`SessionServices` 只暴露交互、后台任务、输出和子会话 broker 的具体句柄；子代理工具由宿主提供子会话装配，不回调 vva 的定义发现函数。

执行顺序沿用：消费输入并处理上下文 → 生成计划 / 校验输入能力 → 输入落盘 → provider → assistant 落盘 → 工具与回执配对落盘 → after_tools 上下文落盘 → 下一次 provider。
初始提示只在新会话写入，恢复时保留已有历史。技能发现与输入处理在实际消费输入时发生；配置更新只在安全边界生效。
`AgentEvent` 驱动草稿与状态，`SessionView` 经 watch 发布；Web 的 snapshot / delta 和终端渲染仍在应用。
公开 handle 的 deliver / steer / 等待回执 / answer_user / abort / note 能力；回执绑定本次投递并等待其后台续跑完成，不能以 watch 的一次 Idle 或单次 Finished 代替。
后台完成可能早于启动回执，终态不能被 Running 覆盖；完成结果在安全轮隙落盘后再续跑，停止或失败后仍保存结果但不自动续跑。
close / shutdown 负责结束会话任务、交互等待与后台资源并返回清理错误；core 不注册 ctrl-c、不退出进程，任务不跨进程恢复。

保留现有 Provider / Tool / SessionStore / SessionBroker trait，各自用于替换模型、工具、存储与子会话编排；不增加 transport、memory、scheduler trait。
JSONL 仍完整保存原生 metadata，provider 不猜字段别名或退化协议；损坏、缺字段、未知工具及存储错误明确返回。
工具白名单与 AskUser 保留现有语义；是否注册交互工具由装配决定，不再通过 IM 身份推断。此次拆分不顺带建设新的审批平台。

## 4. 新应用如何只用 core 组装

下例为单会话的概念草图；模型选择、`options`、输入能力、工作路径及渠道对象来自宿主。业务工具实现现有 Tool，模型与存储也可替换为宿主的 Provider / SessionStore 实现。
仅需依赖 `vva-core` 并开启 `response`、`jsonl`；无需根 package、frontend 或 vva 配置文件。

```rust,ignore
use std::sync::Arc;
use vva_core::{harness::*, model::*, provider::*, storage::*, tools::*};

let store = Arc::new(jsonl::JsonlSessionStore::new(session_root)?);
let provider = Arc::new(response::ResponseProvider::new(api_key).with_base_url(endpoint));
let mut tools = ToolRegistry::new();
tools.register(Arc::new(MyBusinessTool::new(business_service)));
let setup = Arc::new(FixedSetup::new(ExecutionPlan {
    provider, options, inputs, tools,
}));
let mut initial = Session::new(workspace, None);
initial.set_model(model_spec, variant);
initial.append_turn(Turn::new(Role::System, vec![Message::text("你是个人助手。") ]));
let manager = Arc::new(SessionManager::new(store));
let session = manager.create(AgentAssembly { session: initial, setup }).await?;

// 渠道先鉴权、下载附件和解析消息，再交给 core。
let report = session.deliver_wait(inbound_message, InputMode::After).await?;
channel.send(report.messages).await?;
// 宿主持有 manager，按自身关闭策略调用 close / shutdown。
```

多渠道的 binding → SessionId、消息去重、收发重试属于新应用；入站循环与等待回复分开执行，运行中可继续投递 Steer。
无 UI 时可仅等待回执；要进度则订阅 SessionView 或消费有界 AgentEvent 流，事件消费者关闭不代表执行成功或取消。
长期记忆由宿主构造初始上下文或调用 note；定时任务到点后调用 deliver。Space 也按同样路径接入，不获得 task 内部命令或可变会话的特权入口。

## 5. 四步迁移计划

### 第一步：单包内建立装配入口

引入 AgentAssembly / SessionSetup；把默认工具、coding prompt、技能与 AGENTS.md / `@` 处理搬到应用实现。
去掉 curator 特判和 AppConfig 反向依赖；工具仍在原包，先改调用方向，不改 JSONL 格式。
manager 的偏好与配置加载进入应用 facade；通用任务回执格式退出 coding system 模块。
风险集中在重复注入、输入 / 工具结果顺序、provider 连接复用；沿用现有 harness / provider 测试核对这些行为。

### 第二步：建立 workspace 并整体提取 core

把 model / event / provider / tools / storage / 文件解析和已去产品策略的 harness 移入 `crates/vva-core`，原实现随迁移删除。
根包暂用 re-export 维持调用路径，补齐 feature 与可选依赖；`build.rs`、UI / Web 依赖始终留根包。
`default-members` 包含两个 package；core 无默认 feature 可编译，vva 显式开启原有功能。
此步暂保留 v2 DTO 字段，仅作为迁移中间状态，不宣布 API 稳定；重点验证 feature 组合和原生协议保真，不调整协议。

### 第三步：迁出应用元数据，让 Space 只用公开 API

引入应用侧 SpaceSessionRecord / coding 会话配置，移除 core 中的 IM 字段、沉淀命令和展示规则。
Space 的路由、活动时间、curator 与调度接入公开会话 API；关联记录缺失显式报错，不猜普通会话、不自动补建。
建议 JSONL 升为 v3；提供显式 v2 → v3 迁移，停写后写新目录，保留 SessionId、turns、原生 meta 与应用字段，失败不修改源数据。
会话数据与应用记录以完整迁移清单核对后再切换；旧 scratch / tool_outputs 文件及历史路径继续保留，不能只搬 JSONL 就删旧目录。
新运行时遇旧版或未知版本明确报错，不在加载时隐式转换。回退用旧二进制和保留的源目录；关联数据双写失败作为明确错误处理，不宣称跨文件事务。
风险是旧数据丢字段与 Space 元数据不一致；只为未覆盖的真实迁移 / 时序回归补必要测试。

### 第四步：外部消费与发布收口

用独立目录中的最小 headless 消费者验证：只依赖 core，可关闭文件 / Shell / 联网实现，可替换存储，不需要 Node、终端或 `~/.vva`。
覆盖两包测试、core 无默认 feature 及各可选实现组合；三家 provider、后台 / 取消、TUI / Web 同步的既有测试迁移后继续通过。
验证 close / shutdown 回收资源，删除临时 re-export；release 构建明确 `-p vva`，应用二进制与 frontend 打包路径保持在根包。
建议新库先用 `0.1.0`，根应用首次公开破坏发布用 `2.0.0`；package 版本与 JSONL v3 分开管理。
实际发布时同步 manifest / Cargo.lock，并在根 CHANGELOG 写与应用 tag 一致的非空条目，列明 Rust API、格式与迁移方式；本轮不改版本或发布。

每步都应可独立合并并执行以下检查；只搬移相关既有测试，不为 re-export、常量或默认值新增测试：

```sh
export PATH=$HOME/.cargo/bin:$PATH RUSTFLAGS=-Cdebuginfo=1
cargo test
cargo clippy --all-targets -- -D warnings
```

workspace 建立后另核对 `--workspace --all-features` 测试 / clippy，以及 `-p vva-core --no-default-features` 检查。
涉及 Web 同步的步骤继续执行[既有前端验证](../web_sync.md)；不修改仓库 `.cargo` 配置。

## 6. 待负责人拍板

- **旧会话处置**：推荐第三步随格式变更提供显式迁移；若不做，必须明确 v2 只能由旧版本读取，不能静默丢弃应用字段。
- **发布与兼容期**：确认 core 0.1 / 应用 2.0 的建议，以及临时 re-export 在首次 major 发布时移除；是否发布 crates.io 可在外部消费验收后决定。
- **应用记录落点**：推荐 vva 自己维护按 SessionId 关联的强类型记录；需确认与会话存储的目录配置、备份及失败清理流程，不向 core 加任意 JSON 扩展袋。
