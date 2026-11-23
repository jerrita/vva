# Agent Space 设计

Agent Space 是常驻的个人 agent，经 IM 触达，入口 `vva agent`。配置见 [config-reference §2.2](config-reference.md#22-settingsagent)，源码位于 `src/space/`。

## 1. 目标与总体架构

1. **cache 稳定复用**：会话只追加、前缀稳定，Anthropic `cache_control` 与 Responses `previous_response_id` 持续命中。
2. **会话管理透明**：每次对话都是普通的 vva 会话。TUI / Web 可见，可按 `kind` / `origin` 过滤并接管；路由决策可追溯。
3. **自动记忆整理**：curator 把对话整理为可审阅、有版本的记忆与 skill。

成本控制的关键是小模型 router：只凭当前 focus 的元数据判断是否新建会话，不读历史。

```
IM ──► Channel ──► Inbound{binding, user, text, files, reply_to}
                       │
                       ▼
                    Router ── L0 确定性锚点 / L1 小模型 / L2 报错
                       │
               ┌───────┴────────────┐
               ▼                    ▼
         existing / new      guide（Steer 注入）
               │                    │
               └─ 出站渲染（订阅 SessionView）─► Channel

  生命周期：空闲 TTL → 按标记整理 → 删除
  上下文：回合结束达到预留线 → rollover
  定时任务：cron → Deliver
```

Space 复用 core 的公开会话 API：装配、快照订阅、Steer / Deliver、完成回执与 note，不直接操作 `SessionTask` / `Command`。

- 所有会话位于固定工作区 `~/.vva/space/`。
- 应用的投影关联独立的会话记录，供 TUI / Web 展示和接管。

## 2. 概念

**binding**：`channel:chat_id`，QQ 为 `qq:<msg_type>:<chat_id>`。

**focus**：每个 binding 有一个默认续写的指针。

- focus 所指的会话被删除（`/clear`、`/delete`、TTL 清理），或读取时已失效（不存在、已被接替、在范围外）时，改指自身会话范围内最近活跃的非定时会话；尚无活动时取创建时间。无候选则清空。
- 定时任务创建的会话以 `routed_by=scheduled` 识别，可用 `/switch` 切换。
- TTL 清理与读取时修复引起的 focus 变更推送到 home，列出 binding、原会话与新 focus。

**running**：会话是否正在生成。

**待附件缓冲**：binding 级，收到无文字的文件时暂存。

**会话范围**：

- QQ 群聊只使用 `origin.binding()` 等于当前群 binding 的会话；个人聊天共享一个排除群来源的个人会话池。
- 路由候选（当前范围内未被接替的 topic 会话）、`/sessions` 与会话命令统一使用该范围。
- 范围外或已失效的 focus 在读取时按 focus 的补位规则改指；跨范围引用报错。

**home bindings**：

- 后台通知的接收集合，独立于 focus 与会话来源。
- `/sethome` 把当前聊天加入，`/unsethome` 移出。支持多个，幂等，受渠道白名单约束。
- `bindings.json` 的每项带 `home` 布尔字段，默认 `false`，重启保留。
- home 可以是群，该群接收全空间的后台通知。
- home 标记不授予跨范围的会话访问；记忆注入与文件 / 工具工作区在各聊天间共享。

## 3. Router

### 3.1 分层

| 层级 | 触发 | 成本 |
|---|---|---|
| L0 | reply 命中 `message_index`；`/switch`；零会话时直接创建 | 0 token |
| L1 | 其余 | 一次小模型调用（`settings.small_model`），或 `settings.agent.router.decisions` 声明的决策 API |
| L2 | 小模型失败或输出非法 | 报错，附引用回复或 `/new <消息>` 的指引 |

决策 API 支持 OpenRouter Decisions 与 OpenCode System One（`decisions.rs`）：`action` / `focus` / `ttl` 三道 choice 题，回填为同一个 `RouteDecision`。

### 3.2 L1 输入输出

**输入**：

- 当前 focus 的 `id`、`title`（首轮完成前为占位标题）、`running`、`focus`；无 focus 时为 null。
- 用户文字、被 reply 消息的片段、待附件的文件名与类型。
- 不含其他会话或历史正文。

**输出**：强类型 `RouteDecision`，经供应商原生结构化输出产生。

| 字段 | 取值 |
|---|---|
| `action` | `existing` / `new` |
| `focus` | bool |
| `ttl` | `3h` / `12h` / `null` |
| `reason` | string |

决策规则：

- 默认 `existing`，续接当前 focus。focus 运行中时由 Router 生成 guide 路由，空闲时开始新轮次；此时 `focus=false`、`ttl=null`。
- 与 focus 无关的独立话题或任务选 `new`。新建时独立判断是否切换 focus 与空闲 TTL：
  - 预期持续讨论的话题切换 focus，一次性任务保留原 focus。
  - 一次性、短期价值的任务选 3h，正常会话选 12h。
- Decisions 的 `action` / `focus` / `ttl` 置信度低于 `min_confidence` 时续接当前 focus。无 focus 时提示 `/switch` 设置后重发，或 `/new <消息>` 新建。
- 调用失败、缺失字段、非法取值都报错。
- 每次决策向 `router.jsonl` 追加一行，记录输入摘要、决策、reason 与小模型 usage；会话记录 `routed_by`。

### 3.3 路由结果

**new**：

- 新建 `kind=topic` 会话，跟随主 agent 模型；按决策设置 TTL，并切换或保留 focus。
- 响应首行为 `> decision: new, focus, sess_…`，保留原 focus 时省略 `focus`。
- 零会话、`/new`、引用目标已清理三种情况直接创建并切换 focus，TTL 为 12h。

**existing / guide**：

- 经 Steer 投递。运行中在下一次模型调用前注入，即完整工具批次结束后；最后一次模型响应期间到达的也继续在本轮处理。空闲则触发新轮次。
- reply 直达遵循相同规则。
- 会话被 stop 或删除时清空程序化投递队列；失败或中止向投递者返回错误。

**reply 目标已清理**：

- `message_index` 只保存 `消息 id → 会话 id`，键带 channel 前缀 `qq:<id>`。
- 目标不存在时向用户发 warning，新建会话，以入站自带的 `reply_to_text` 作为首条上下文，再处理当前消息。
- 命中范围外的会话时拒绝续接，提示发送新消息或 `/new <消息>`。home 聊天引用其他来源会话的通知时同样适用。

## 4. 会话生命周期

### 4.1 TTL 清理

- 会话的 TTL 保存在 `ttl_secs`，未指定时用 `settings.agent.ttl_secs`；rollover 接替的会话继承该值。
- 空闲超过 TTL 后，按[整理标记](#43-curator记忆整理)决定是否整理，然后删除。
- 整理失败时会话保留并告警，按指数退避重试。撤销标记后，下次扫描直接清理，跳过整理失败的退避。
- 运行中或有排队消息的会话不清理。
- 候选表按 LRU 取前 N 个，超出的走同一清理流程。
- 重启后状态从记录恢复。

### 4.2 rollover

不原地改写历史。回合结束后若上下文达到 `limit.context` 减去预留，复用 [`/compact`](usage.md#压缩上下文) 的请求，由主模型生成摘要；请求参数与上一轮一致，以复用 cache 前缀。压缩记录与摘要作为追加轮次留在旧会话。随后：

1. 新建会话，`continued_from` 指向旧会话，`parent` 为空。首条 `<system-reminder>` 为摘要；除 system 前缀外不复制任何对话轮次，摘要须自足。接替的会话首轮写入缓存，第二轮起命中前缀。
2. binding 的 focus 与 `message_index` 切到新会话（`replace_focus` + `MessageIndex::repoint`），提示用户。home 标记保留。
3. 旧会话写入 `superseded_by`，router 不再把它列为候选，之后走常规 TTL 清理。

约束：

- 压缩请求不改变 system、工具、模型、推理档位与 `tool_choice`。prompt 要求不调用工具；返回工具调用则视为失败，且不执行。
- 失败时通知用户，会话不变，下个回合结束重试。
- 预留不小于压缩 prompt 加摘要的 `max_tokens`。
- 轮内溢出（单轮工具输出撑满窗口）不在本机制范围内，直接报错。

摘要分节：目标、偏好、关键事实、约束、决策、待办、未决问题。不确定的信息标注「未确认」。

### 4.3 Curator（记忆整理）

**整理资格**：

- Agent Space 的顶层 topic 会话通过 `SetSessionCuration({"enabled": true})` 请求整理，`false` 撤销。
- 标记保存在 `SpaceSessionRecord.curation_requested`。
- 该工具属于 Space 装配，绑定当前 SessionId，串行更新应用记录，落盘成功后返回。重复设置幂等，不延长 TTL。

**主动标记**：

- 模型发现稳定偏好、已确认的决策与约束、可复用的经验时调用工具。
- 临时进度、一次性查询和普通任务执行无需标记。
- 标记覆盖本会话的全部未处理内容及后续对话。curator 决定具体写入内容，允许无需更新。

**触发**：

- 系统在删除已标记的会话前触发后台会话（`kind=curator`），时机为 TTL 周期扫描或 `/delete`。未标记的会话直接删除。
- `Curator::curate` 统一检查资格与标记，跳过时返回 `Unchanged`，不创建整理输入，也不启动模型。curator 自身与子代理会话不参与。
- `/clear` 与 rollover 不触发整理。rollover 的旧会话保留标记，按 TTL 清理；新会话默认未标记。
- 整理成功后保留标记；整理期间新增的内容留给下次增量处理。

**模型**：`settings.agent.curator_model`，启动时校验，不受 `/model` 影响；省略时取当前 agent 模式模型。

**增量**：只处理游标之后的 turns，重复触发返回 `unchanged`。结果三态：`updated` / `unchanged` / `failed`。

**产出**：

- 写入 `~/.vva/space/memory/`，分三层：`MEMORY.md` 只列领域，`<领域>/INDEX.md` 列主题，`<领域>/<topic>.md` 存内容。
- 纳入 git，每次整理一个 commit，提交信息含来源会话与范围。
- 反复出现的工作流晋升为 skill，写入 `~/.vva/space/.vva/skills`。`discover_skills` 能发现它，只有 Agent Space 会话可用。
- 完成后推送摘要。git 冲突时不覆盖，告警并跳过。

**删除同步清理**：来源会话被删除（TTL、`/delete`、`/clear`）时，`Curator::forget` 清除其游标与残留的整理输入，并删除为它创建的 curator 会话（标题 `[curator] <session_id>`）。curator 会话清理失败只告警，由 TTL 清理。

## 5. Cache 稳定性纪律

- 同一会话内 system 与前缀字节级不变，只追加 turn。
- 技能清单、记忆索引、后台通知、定时触发都以 `<system-reminder>` user turn 追加。
- system prompt 的 `Date` 在会话创建时写入并冻结，避免跨日全部 miss；rollover 逐字复制 system 前缀。
- router 调用的前缀固定，会话表与用户消息置尾。
- **记忆索引**只在会话创建时快照：
  - Agent Space 会话（排除 curator）首次落盘时追加 `<system-reminder type="memory">`，内容为当时 `memory/MEMORY.md` 的全文并注明记忆库的绝对路径；细节由模型按需 Read。
  - `MEMORY.md` 不存在则不注入。
  - 全文注入，不截断。体量有界：`MEMORY.md` 只列领域，增长的是领域数而非条目数。
- **rollover 接替的会话**：system 前缀中的旧记忆快照以当前 `MEMORY.md` 原位重生成。旧前缀没有则不加，当前无索引则丢弃旧快照。
- **记忆更新通知**：curator 结果为 `Updated` 时，向现存的 topic 会话（排除 curator、已被接替的、来源会话）追加 `<system-reminder type="memory-updated">`，内含 curator 摘要并提示 Read `MEMORY.md`。
  - 经 `ManagedSession::note` 追加，不起轮次。
  - 空闲时直接落盘；运行中入队，在轮次间隙或本轮收尾写入。
  - 已有前缀不变。
- **技能变动提示**：
  - 每条新消息提交前重新发现技能，与历史中最近一次 `skills` / `skills-changed` 提醒携带的指纹索引比对。索引形如 `index="name:hash,..."`，hash 覆盖 description 与正文。
  - 有新增、更新或移除时，在用户消息之前注入 `<system-reminder type="skills-changed">`，并以新索引作为之后的基线。
  - 不带索引的历史清单整体刷新一次。
  - 子代理与 curator 会话不注入。`/skills`（IM 与 TUI）每次现扫目录。
- 状态页展示各会话的 cache 命中率与 router 小模型成本，数据来自 `usage` 与成本配置。

## 6. Channel

- **`Channel` trait**：`run(inbound_tx)` 与 `send(OutboundMessage)`，可通过 `progress_stream` 提供[进度汇报](#8-进度汇报)能力。当前实现为 QQ，gateway 的 identify / resume / heartbeat 按官方规范。
- **准入**：白名单外的用户静默丢弃并记日志。
- **入站循环**：顺序完成路由与建会话，轮次在独立任务执行；运行中的会话也可被 reply / guide 插话。各类 chat 的 binding 相互隔离；断线 resume 不重复、不丢消息。
- **最终回答**：轮次结束后整段发送。
  - 渲染器负责分段，超限时在末段标注截断。
  - markdown 发送失败时降级为纯文本。
  - 失败或中止时回复错误文本。
- **命令排版**：
  - 斜杠命令的输出统一为 Markdown。
  - 帮助、会话、模型、技能与状态用带标题的表格展示；定时任务逐项两列表格。
  - 反馈与错误使用少量 emoji 并附后续操作提示。
  - 动态文本压平空白并转义 Markdown 标记，截断处显示省略号。
- **通知目标**：
  - 自动整理的更新 / 失败、后台删除完成、focus 自动补位、定时任务的结果 / 进度 / 失败 / 错过、rollover 提示，统一发往全部 home bindings。
  - 每次发送查询最新的 home 集合；单个目标失败不影响其他目标。
  - 无 home 时只跳过推送并记日志。
  - 普通回答、命令反馈、对话进度与附件反馈发往发起的聊天。
- **文件**：
  - 无文字：进入待附件缓冲，不触发模型，回复「已收到」。
  - 有文字（含 caption）：一次路由，文件随之进入目标会话。
  - 文件落盘到 `files/`，历史只记绝对路径文本，多附件保序。
  - 缓冲超时（`pending_files_ttl_secs`）后提示并清理缓冲，文件保留在磁盘。
- **工具**：
  - `SendFile`（`send_file.rs`）：渠道绑定，经 `SessionManager::with_tool_factory` 按会话 header 在运行期注入，只注入带 origin 的顶层会话。参数为 `path`（工作区相对 / 绝对 / `~`）与可选的 `caption`。文件缺失或不是普通文件时报错；渠道错误作为工具错误原样返回，由模型转述。
  - `Schedule`：见 §7，只挂在带个人 IM 来源的顶层会话，群内不挂载。
  - `AskUser`：Space 装配关闭交互，不挂载。core 不判断 IM 身份。

### 6.1 命令

命令解析在 `command.rs`，`/help` 列出。命令名大小写不敏感，容忍 `/cmd@bot`。`/` 开头的未知命令回复帮助，不交给模型。

| 命令 | 作用 |
|---|---|
| `/new <消息>` | 强制新建 topic 会话，必须带文字 |
| `/switch <前缀>` | 设置 focus；有歧义时列出候选并拒绝。后续消息经 router |
| `/sessions` | 列出会话、使用的模型、空闲 TTL 与剩余时间 |
| `/stop` | 中止 focus |
| `/status`（`/stat`） | 会话、模型、上下文占比、cache 命中率、待附件数、当前聊天是否为 home |
| `/rename <标题>` | 覆盖标题，并取消尚未回写的自动标题 |
| `/model [list\|[--global] <规格> [档位]]` | 切换模型，规则见下 |
| `/model --variant <档位>` | 切换推理档位；`none` 清除档位 |
| `/skills` | 列出 Agent Space 可发现的技能 |
| `/schedule [list\|pause\|resume\|delete\|run <id>]` | 管理定时任务，仅私聊 |
| `/delete [前缀]` | 整理后删除，默认为 focus；后台执行并回报，整理失败则会话保留 |
| `/compact` | 压缩默认会话的上下文，见[使用指南](usage.md#压缩上下文) |
| `/clear [前缀]` | 直接删除，跳过整理 |
| `/sethome` / `/unsethome` | 加入 / 移出 home |

`/sessions` 的剩余时间按最后活动时间计算，尚无活动时取创建时间。到期显示 0s，实际清理遵循 [TTL 清理](#41-ttl-清理)。

`/model` 的规则：

- 有 focus 时改 focus 会话，无 focus 时改 agent 模式模型；`--global` 两者都改。
- 切换要求目标模型声明 `limit.context`。
- 会话切换使缓存失效。
- agent 会话的切换不写 `state.json`。

agent 模式模型决定新会话的模型，curator 未配置独立模型时每次整理取当前值。它保存在 `~/.vva/space/model.json`，优先于配置文件。

`/clear`、`/delete` 删除 focus 会话后，所有指向它的 binding 按 [focus 的补位规则](#2-概念)改指；删除其他会话时保留现有 focus。

群内开放 `/new`、`/switch`、`/sessions`、`/stop`、`/compact`、`/status`、`/rename`、`/clear`、`/sethome`、`/unsethome`、`/help`，其余命令提示在私聊使用。帮助只列当前可用的命令。

## 7. Scheduler

三个模块：`scheduler.rs`（任务表、状态、计时）、`schedule_tool.rs`（`Schedule` 工具）、`runtime/schedule.rs`（执行与通知）。

**定义**：`<workspace>/schedule.json`，由 `Schedule` 工具与 `/schedule` 命令写入，原子写。人手修改按 mtime 热重载，自身写入不触发重载。解析失败时保留旧任务表并告警，同一 mtime 不重复报错。

**状态**：`schedule_state.json`。每个任务记录 `enabled`、`last_run`、`last_error`、连续失败的 `failures`、interval 的 `next`；另有全局 `last_tick`。

**任务**：字段为 `id`、触发方式三选一、`prompt`、`session`（可选）、`bindings`（可选）。`session` 与 `bindings` 至少其一。

```json
{ "tasks": [
  { "id": "morning", "cron": "0 9 * * 1-5", "prompt": "汇总今日待办", "bindings": ["qq:c2c:<openid>", "qq:group:<gid>"] },
  { "id": "remind",  "at": "2026-10-11T09:00", "prompt": "提醒开会", "bindings": ["qq:c2c:<openid>"] },
  { "id": "check",   "every": "30m", "prompt": "检查新邮件，没有就保持静默", "session": "sess_abcde" }
] }
```

**触发**：

- `cron`：5 字段，本地时区。支持 `*`、数字、列表、范围、步长；日与周都受限时取并集；周日为 0 或 7。到点时该任务还在执行则跳过该次触发。
- `at`：一次性本地时刻 `YYYY-MM-DDTHH:MM`，执行后自动停用；已过的时刻拒绝创建。
- `every`：`30m` / `2h` / `1d`，分钟精度，从上次完成起算，任务之间不重叠。

**执行目标**：

- 指定 `session`：在该会话内执行并保留上下文；会话被 TTL 清理后任务永久失败。
- 未指定 `session`：在 `bindings[0]` 新建普通 topic 会话执行，无历史，prompt 须自足。该会话进入路由候选，支持后续追问；后台创建，不切换 focus，TTL 为 12h。
- 指定已有会话时使用其 TTL。周期任务用后一种方式。
- 当前 channel 实例唯一（QQ），`bindings` 须同属该渠道。

**通知**：一次触发只执行一次。最终回复与执行进度发往全部 home bindings，并登记 `message_index`。引用按当前聊天的范围校验，目标已清理则按 §3.3 处理。`bindings` 只作执行来源，不决定通知目标。

**静默**：触发 reminder 告知模型「无需通知用户时只回复 `[SILENT]`」。Runtime 对精确匹配的回复不发消息，历史保留。

**失败**：

- 单次失败通知 home，任务继续；连续失败达 `task_max_failures` 时自动停用。
- 永久性错误（目标会话不存在、binding 非法）与一次性任务的失败立即停用，并提示 `/schedule resume`。
- 成功后清零计数。
- 单次执行超过 `task_timeout_secs` 时中止并计为失败。

**错过的任务**：每分钟记录 `last_tick`。启动时对账，回溯上限 7 天。cron 错过、interval 的 `next` 已过期、once 已过未执行，都记 `tracing::warn`，并汇总后向 home 提示一次，不补跑。interval 重排到之后；once 可用 `/schedule run <id>` 补跑。

**管理**：

- `Schedule` 工具：`create` / `list` / `pause` / `resume` / `delete`，参数当场校验。`list` 与创建失败时附当前本地时间，供模型换算 `at`。
- `/schedule run <id>`：立即执行一次，已停用的需先 resume。
- 创建只经自然语言 → 工具，没有 `/schedule add`。

## 8. 进度汇报

`progress.rs`。轮次（含定时任务）超过 `progress_interval_secs` 未结束时，每隔该时长向聊天推送一段摘要，直到轮次结束。

摘要由已完成的轮次确定性提炼，不调用模型：

- 新建 / 修改的文件，按各编辑协议提取路径；
- Shell 的 `intent`，不展示命令原文；
- 读取 / 检索与其他工具的调用次数，工具报错次数；
- 最近几条助手文字。

本段无新动作时发「仍在处理」。

同一会话只有一个汇报者。汇报消息计入 `message_index`，用户 reply 它会直达该会话。最终回复在轮次结束时发送。

QQ C2C 中由用户触发的轮次使用官方 `stream_messages`：

- 首次汇报创建一条进度消息，后续摘要在同一条消息内追加，保留已下发正文的前缀。
- 每帧使用同一个 `msg_seq`，递增 `index`，并携带服务端返回的流式消息 id。
- 轮次成功、失败或中止时追加结束状态，以 `input_state=10` 收尾。
- 未到汇报时间就结束的轮次不创建进度消息。
- 流式发送失败后停止追加；已创建的流尝试收尾，错误记入日志。

群聊、频道与定时任务逐条发送摘要。

## 9. 状态文件

工作区 `~/.vva/space/` 下：`bindings.json`、`message_index.jsonl`、`router.jsonl`、`schedule.json`、`schedule_state.json`、`model.json`、`files/`、`memory/`。

会话的应用记录与会话存储并列，路径与迁移见 [storage](storage.md)。

`SpaceSessionRecord` 以 SessionId 关联会话，保存：

- `origin{channel, chat_id}`
- `kind`（Topic / Curator）
- `routed_by`
- `ttl_secs`
- `last_activity`
- `curation_requested`

完成回执（含后台续跑的完成或中止）到达后，Space 更新活动时间；尚无活动时 TTL 从创建时间起算。coding 编辑协议与 web 开关保存在同一应用记录的 `coding` 配置。加载时遇到缺失的关联记录直接报错，不推断为普通会话，也不自动补建。父子与续接关系由 core 保存。
