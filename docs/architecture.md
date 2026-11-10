# 核心架构

## 1. 问题：最小公分母（LCD）

通用 coding harness 为抹平供应商差异，把请求退化为无状态的 `/v1/chat/completions`，丢失各家原生能力：

| 供应商 | 丢失的能力 |
|---|---|
| OpenAI Responses | 服务端链式 `response_id`，多轮需重复上传历史 |
| Anthropic | Prompt Caching（`cache_control`）；Thinking 块 `signature`，思考链无法原样回传而触发校验错误 |
| Gemini | 原生 Google Search Grounding 的字符偏移引用 |
| 跨供应商 | 切换模型时上下文格式割裂，无法接力 |

vva 在统一 Session 抽象之上保留各供应商原生特性。设计约束（统一存储、零兜底、强类型元数据）见 [AGENTS.md §2](../AGENTS.md#2-设计约束)。

## 2. 分层

Cargo workspace 含根应用 `vva`（2.0.0）和 `crates/vva-core`（0.1.0），`default-members` 同时包含两包，依赖方向为 `vva → vva-core`。
core 的 `response`、`anthropic`、`gemini`、`filesystem-tools`、`shell-tools`、`web-tools`、`jsonl` feature 默认关闭；根应用显式开启全部能力。
`web-tools` 联动 Anthropic 独立搜索与 Gemini 原生联网实现。关闭实现不裁剪历史元数据，也不更换协议或工具。
`build.rs`、终端 / 剪贴板、Axum、静态资源和 frontend 仅属于应用；单独构建 core 不执行 npm。
应用直接引用 `vva_core`，根包不再转发 core 模块。core JSONL v3 只保存通用会话；应用侧 coding 配置与 Space 记录独立存储，见 [storage](storage.md)。Space 通过公开会话 API 工作；应用投影组合快照、应用记录和展示规则。

```
+--------------------------------------------------------------+
|  应用 vva：main.rs · app/ · ui/ · web/ · space/                 |
+------------------------------+-------------------------------+
                               v
+--------------------------------------------------------------+
|  core：harness/ (SessionManager · SessionTask · ExecutionPlan)  |
|           tools/                                              |
+---------------+----------------------------+-----------------+
                v                            v
+-----------------------------+  +-----------------------------+
|  供应商适配层  provider/       |  |  存储层  storage/ (JSONL)    |
|  Responses·Anthropic·Gemini |  |  Session→Turn→Message→Part  |
+-----------------------------+  +-----------------------------+
          领域模型 model/ 贯穿各层，应用 config/ 经 app/ 装配执行计划
```

| 层 | 职责 | 详述 |
|---|---|---|
| 配置 `config/` | 分层加载、JSONC / 环境变量展开、`{provider}/{model}` 寻址。`resolve_model` 只做解析并返回 `ResolvedModel`，Provider 由 `create_provider()` 在发起调用时单独构造，统计价格、列模型清单等只读路径不创建适配器 | [config-reference](config-reference.md) |
| 领域模型 `model/` | 四层实体、多模态内容、引用注脚、轮次级强类型元数据 | [domain_model](domain_model.md) |
| 供应商适配 `provider/` | 实现 `Provider` trait 并声明 `ProviderCapabilities`；协议转换后经 SSE 输出标准 `AgentEvent` | [providers](providers.md) |
| 调度 `harness/` `tools/` | 应用 `VvaSessionSetup` 经 `AgentAssembly` 提供 Provider、工具注册表、补全参数与输入 / 工具后上下文；`SessionTask` 是会话唯一所有者，顺序处理提交、配置变更与取消，驱动 Agent Loop 并落盘；`SessionManager` 只管理明确装配的会话与句柄，应用 facade `VvaSessionManager` 负责模型偏好和配置重载 | [background_tasks](background_tasks.md) |
| 存储 `storage/` | `JsonlSessionStore` 行级追加写，崩溃容灾，流式读取 | [storage](storage.md) |
| 交互 | CLI 单次执行与会话检查；TUI 与 Web 订阅 `SessionView`；agent space 驱动 IM | [ui](ui.md) · [web_sync](web_sync.md) · [agent](agent.md) |

## 3. 数据流

1. 交互层向 `ManagedSession` 提交输入，`SessionTask` 调用宿主 `SessionSetup::prepare_input` 处理输入，再取得 `ExecutionPlan`（coding 的 `@` / skills / AGENTS.md 策略由 `VvaSessionSetup` 实现）；
2. Provider 以原生协议请求，流式增量转为 `AgentEvent`；
3. 事件驱动 `SessionTask` 更新草稿，并以 core `SessionView` 快照经 `watch` 发布（33ms 节流）；应用投影关联会话配置和 Space 记录，提供状态文案与 TUI 终态计时，再交给 TUI/Web/IM；
4. 工具调用由 `ToolRegistry` 执行，结果配对后进入下一次请求；
5. 每个完成的 Turn 追加写入 JSONL，供应商强类型元数据随 Turn 保存，下一次请求按原生协议回传。

后续新增供应商的步骤见 [AGENTS.md §4](../AGENTS.md#4-开发指南)。

## 4. 外部消费与生命周期

[`examples/headless.rs`](../crates/vva-core/examples/headless.rs) 演示只使用 core 的会话执行：自定义 `Tool`、`ToolRegistry`、`FixedSetup`、JSONL 存储，投递一次输入并等待包含后台续跑的回执。示例仅开启 `response,jsonl`，不需要 Node、终端、应用配置或 `~/.vva`。使用：

```sh
cargo build -p vva-core --examples --features response,jsonl
OPENAI_API_KEY=... OPENAI_MODEL=... VVA_SESSION_DIR=/absolute/path/to/sessions \
  cargo run -p vva-core --example headless --features response,jsonl -- 'Uppercase hello using the tool.'
```

凭据、模型和存储目录必须显式提供。示例会联网调用 Responses；编译验收不要求凭据，也不发起请求。

core 的公开入口包括 `harness::{AgentAssembly, SessionSetup, FixedSetup, ExecutionPlan, SessionServices, SessionManager, ManagedSession, SessionView, DeliveryReceipt}`，以及领域模型、Provider / Tool / SessionStore 契约与应用实际使用的适配器和工具装配。Task、Command、内部状态表、SSE 解码和输出采集实现不公开。

- `SessionManager::close(id)` 中止该任务的当前执行，取消交互和投递等待，等待标题任务、后台回执等待任务及后台 monitor 退出，再移出登记表；保留已持久化历史。
- `SessionManager::shutdown()` 关闭登记入口并清空句柄表，对所有会话执行上述清理；单项失败仍清理其余项，汇总错误返回。关闭后创建或打开新会话明确报错。
- 中止当前轮次的 `abort_current()` 保留会话任务和后台结果处理，与关闭任务的生命周期不同。后台 Shell monitor 的取消会释放输出管道，并通过 `kill_on_drop` 终止其持有的子进程。
- core 不注册进程信号、不退出进程；宿主负责调用生命周期 API。应用 facade 转交 shutdown，CLI 单次执行、TUI 退出及 Space 停止时回收其会话。
