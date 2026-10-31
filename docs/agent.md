# Agent Space 设计

常驻、经 IM 触达的个人 agent，入口 `vva agent`。配置字段见 [config-reference §2.2](config-reference.md#22-settingsagent-常驻-im-agent)，源码位于 `src/space/`。

## 1. 目标与总体架构

1. **cache 稳定复用**：session 只追加、前缀稳定，Anthropic `cache_control` 与 Responses `previous_response_id` 持续命中；
2. **session 管理透明**：每次对话都是普通 vva JSONL session，TUI / Web 可见、可接管，路由决策可追溯；
3. **自动知识沉淀**：curator 将对话整理为可审阅、有版本的记忆与 skill。

成本控制的关键是**小模型 router**：仅凭当前 focus 的元数据判断是否新建，不读历史。

```
IM ──► Channel ──► Inbound{binding, user, text, files, reply_to}
                       │
                       ▼
                    Router ── L0 确定性锚点 / L1 小模型 / L2 显式报错
                       │
               ┌───────┴────────────┐
               ▼                    ▼
         existing / new      guide（Steer 注入）
               │                    │
               └─ 出站渲染（订阅 SessionView）─► Channel

  生命周期：空闲 TTL → 按标记沉淀 → 删除
  上下文：回合结束达到预留线 → rollover
  Scheduler：cron → Deliver
```

复用 `SessionManager`、Steer / `Deliver`、skills、JSONL。所有 session 位于固定工作区 `~/.vva/space/`；TUI / Web 暂不接入 agent space 逻辑，新增 header 字段均有缺省，旧界面照常读取。

## 2. 概念

- **binding**：`channel:chat_id`（QQ 为 `qq:<msg_type>:<chat_id>`）；
- **focus**：每个 binding 一个默认续写指针；
- **running**：session 是否正在生成；
- **待附件缓冲**：binding 级，收到无文字文件时暂存；
- **会话范围**：QQ 群聊仅使用 `origin.binding()` 等于当前群 binding 的会话；个人聊天共享排除群来源的个人会话池。路由候选（当前范围内未被接替的 Topic 会话）、`/sessions` 与会话命令统一使用该范围。范围外或已失效的 focus 读取时清除，跨范围引用显式报错；
- **home bindings**：后台通知的接收集合，独立于 focus 与 session 来源。`/sethome` 加入当前聊天，`/unsethome` 移出，支持多个、幂等、沿用渠道白名单准入；`bindings.json` 每项带 `home` 布尔字段（缺省 `false`），重启保留。home 可为群，该群将接收全空间后台通知；home 标记不授予跨范围会话访问能力，记忆注入与文件 / 工具工作区仍共享。

## 3. Router

### 3.1 分层

| 层级 | 触发 | 成本 |
|---|---|---|
| L0 | reply 命中 `message_index`；`/switch`；零 session 时直接创建 | 0 token |
| L1 | 其余 | 一次小模型调用（`settings.small_model`），或 `settings.agent.router.decisions` 声明的 OpenRouter Decisions / OpenCode System One API（`decisions.rs`：`action` / `focus` / `ttl` choice 题，回填为同一 `RouteDecision`） |
| L2 | 小模型失败 / 非法输出 | 显式报错，附引用回复或 `/new <消息>` 指引 |

### 3.2 L1 输入输出

模型输入只含当前 focus 的 `id`、`title`（首轮完成前为占位标题）、`running`、`focus`；无 focus 时为 null。另含用户文字、被 reply 消息片段与待附件文件名、类型，不含其他会话或历史正文。

输出强类型 `RouteDecision`：`action: existing | new`、`focus: bool`、`ttl: 3h | 12h | null`、`reason`，使用供应商原生结构化输出。

- 默认 `existing`，续接当前 focus；其运行中时由 Router 生成 guide 路由，空闲时开始新轮次。此时决策 `focus=false`、`ttl=null`；
- 明确与 focus 无关的独立话题或任务选择 `new`。新建时独立判断是否切换 focus 与空闲 TTL：预期持续讨论的话题切换 focus，oneshot 一次性任务保留原 focus；一次性、短期价值任务选择 3h，正常会话选择 12h；
- Decisions 的 `action` / `focus` / `ttl` 置信度低于 `min_confidence` 时续接当前 focus；无 focus 时提示 `/switch` 设置后重发，或 `/new <消息>` 新建；
- 调用失败、缺失字段、非法取值保持显式报错；
- 每次决策追加一行 `router.jsonl`（输入摘要、决策、reason、小模型 usage），session header 记录 `routed_by`。

### 3.3 路由结果

- **new**：新建 `kind=topic` session（跟随主 agent 模型），按决策设置 TTL，并选择切换或保留 focus。响应首行为 `> decision: new, focus, sess_…`，保留原 focus 时省略 `focus`。零会话、`/new` 与引用目标已清理时直接创建并切换 focus，TTL 为 12h；
- **existing / guide**：经 Steer 投递，运行中在下次模型调用前注入（完整工具批次结束后），最后一次模型响应期间到达也继续在本轮处理；空闲则触发新轮次。reply 直达遵循相同规则；session 被 stop / 删除时清空程序化投递队列，失败或中止向投递者返回错误；
- **reply 目标已清理**：`message_index` 仅保存 `消息 id → session id`（键带 channel 前缀 `qq:<id>`）。目标不存在时 warning 用户，新建 session，以入站自带的 `reply_to_text` 作为首条上下文并处理当前消息；命中范围外会话时显式拒绝续接，提示发送新消息或 `/new <消息>`（home 聊天引用其他来源会话的通知时同样适用）。

## 4. Session 生命周期

### 4.1 TTL 清理

会话 TTL 持久化于 header 的 `ttl_secs`，缺省使用 `settings.agent.ttl_secs`；rollover 接替会话继承该值。空闲超过 TTL → 按[沉淀标记](#43-curator知识沉淀)决定是否整理 → 删除。沉淀失败则保留 session 并告警，按指数退避重试；撤销标记后下次扫描直接清理，跳过沉淀失败退避。运行中或有排队消息的 session 不清理；候选表按 LRU 取前 N 个，超出者走同一清理流程；重启后状态从 header 恢复。

### 4.2 rollover

不原地改写历史。回合结束后若上下文达到 `limit.context − 预留`，由**主模型在同一 session 末尾追加压缩 prompt**（请求参数与上一轮一致，复用 cache 前缀），摘要作为追加轮次留在旧 session。随后：

1. 新建 session，`continued_from` 指向旧 session（不借用 subagent 的 `parent`），首条 `<system-reminder>` 为摘要，除 system 前缀外不复制任何对话轮次（摘要须自足）；
2. binding focus 与 `message_index` 切到新 session（`replace_focus` + `MessageIndex::repoint`），提示用户，home 标记保留；
3. 旧 session 写 `superseded_by`，router 排除，之后走常规 TTL 清理。

约束：

- 压缩请求不改变 system、工具、模型、推理档位、`tool_choice`；prompt 要求不调用工具，仍调用则视为失败（`tool_choice` 与 thinking 变更是否使缓存失效需实测）；
- 失败：显式通知用户，session 不变，下个回合结束重试；
- 预留 ≥ 压缩 prompt + 摘要 `max_tokens`；
- 轮内溢出（单轮工具输出撑满窗口）不在此机制范围，显式报错。

摘要分节：目标、偏好、关键事实、约束、决策、待办、未决问题；不确定信息标注「未确认」。

### 4.3 Curator（知识沉淀）

- **沉淀资格**：Agent Space 顶层 topic 会话通过 `SetSessionCuration({"enabled": true})` 请求整理，`false` 撤销；标记持久化于 header 的 `curation_requested`，新旧会话缺省均为 `false`。工具绑定当前会话，在运行中经会话命令通道更新 header，落盘成功后返回；重复设置幂等，不延长 TTL；
- **主动标记**：模型发现稳定偏好、已确认的决策与约束、可复用经验时调用工具；临时进度、一次性查询和普通任务执行无需标记。标记覆盖本会话全部未处理内容及后续对话，curator 决定具体写入内容，允许无需更新；
- 由系统在删除已标记会话前触发后台 session（`kind=curator`；TTL 周期扫描或 `/delete`），未标记会话直接删除。`Curator::curate` 统一检查资格与标记，跳过时返回 `Unchanged`，不创建整理输入或启动模型；curator 自身与子代理会话不参与；
- `/clear` 与 rollover 本身不触发沉淀。rollover 旧会话保留标记，按 TTL 清理；新会话默认未标记。整理成功保留标记，整理期间新增内容仍可增量处理；
- 模型取 `settings.agent.curator_model`（启动时校验，不受 `/model` 影响），省略时取当前 agent 模式模型；
- 只处理游标之后的增量 turns，重复触发返回 `unchanged`；结果三态 `updated | unchanged | failed`；
- 产出写入 `~/.vva/space/memory/`：`MEMORY.md` 只列领域 → `<领域>/INDEX.md` 列主题 → `<领域>/<topic>.md` 内容；纳入 git，每次整理一个 commit，提交信息含来源 session 与范围。反复出现的工作流晋升为 skill，写入 `~/.vva/space/.vva/skills`（仅 agent space 会话可发现）；
- 完成后推送摘要；git 冲突时不覆盖，告警并跳过；
- **删除同步清理**：来源会话被删除（TTL、`/delete`、`/clear`）时，`Curator::forget` 清除其游标与残留整理输入，并删除为它创建的 curator 会话（标题 `[curator] <session_id>`）；curator 会话清理失败只告警，由 TTL 兜底。

## 5. Cache 稳定性纪律

- 同 session 内 system 与前缀字节级不变，只追加 turn；
- 技能清单、记忆索引、后台通知、定时触发一律以 `<system-reminder>` user turn 追加；
- system prompt 的 `Date` 在 session 创建时冻结（或改由 reminder 提供），避免跨日全体 miss；
- router 调用前缀固定，session 表与用户消息置尾；
- **记忆索引**仅在 session 创建时快照：agent space 会话（排除 curator）首次落盘时追加 `<system-reminder type="memory">`，内容为当时 `memory/MEMORY.md` 全文并注明记忆库绝对路径，细节由模型按需 Read；`MEMORY.md` 不存在则不注入。全文注入、不截断，体量有界（`MEMORY.md` 只列领域，增长的是领域数而非条目数）；
- rollover 接替会话的 system 前缀中，旧记忆快照以**当前** `MEMORY.md` 原位重生成（旧前缀没有则不加，当前无索引则丢弃旧快照）；
- **记忆更新通知**：curator 结果为 `Updated` 时，向现存 topic 会话（排除 curator、已 superseded、来源会话）追加 `<system-reminder type="memory-updated">`（含 curator 摘要，提示 Read `MEMORY.md`）。经 `ManagedSession::note`：只追加不起轮次，空闲直接落盘，运行中入队到轮次间隙或本轮收尾，不改动已有前缀；
- **技能变动提示**：每条新消息提交前重新发现技能，与历史中最近一次 `skills` / `skills-changed` 提醒携带的指纹索引（`index="name:hash,..."`，hash 覆盖 description 与正文）比对；有新增 / 更新 / 移除时，在用户消息之前注入 `<system-reminder type="skills-changed">`，并以新索引为之后的基线。旧版不带索引的清单整体刷新一次；子代理 / curator 会话不注入。`/skills`（IM 与 TUI）每次现扫目录；
- 状态页展示各 session cache 命中率与 router 小模型成本，数据来自 `usage` 与成本配置。

## 6. Channel

- `Channel` trait：`run(inbound_tx)` + `send(OutboundMessage)`，可通过 `progress_stream` 提供[进度汇报](#8-进度汇报)能力。首发 QQ（gateway identify / resume / heartbeat 按官方规范），其后 Telegram；
- 白名单外用户静默丢弃并记日志；
- 入站循环顺序完成路由与建会话，轮次在独立任务执行，运行中的 session 仍可被 reply / guide 插话；
- 最终回答在轮次结束后整段发送；渲染器负责分段（超限时在末段明确标注截断）与 markdown 失败降级纯文本；失败 / 中止显式回复错误文本；
- **命令排版**：斜杠命令输出统一 Markdown；帮助、会话、模型、技能与状态以带标题的表格展示，定时任务逐项两列表格；反馈与错误使用少量 emoji 并附后续操作提示；动态文本压平空白并转义 Markdown 标记，截断处明确显示省略号；
- **通知目标**：自动整理更新 / 失败、后台删除完成、定时任务结果 / 进度 / 失败 / 错过、rollover 提示统一发往全部 home bindings，每次发送查询最新集合，单个目标失败不影响其他目标；无 home 时执行照常继续，跳过推送并记录日志。普通回答、命令反馈、对话进度与附件反馈发往发起聊天；
- **文件**：无文字 → 进入待附件缓冲，不触发模型，回复「已收到」；有文字（含 caption）→ 一次路由，文件随之进入目标 session；落盘 `files/`，历史只记绝对路径文本，多附件保序；缓冲超时（`pending_files_ttl_secs`）明确提示并清理缓冲（文件保留在磁盘）；
- **工具**：
  - `SendFile`（`send_file.rs`）：渠道绑定，经 `SessionManager::with_tool_factory` 按会话 header 运行期注入（仅带 origin 的顶层会话）；参数 `path`（工作区相对 / 绝对 / `~`）与可选 `caption`；文件缺失或非普通文件显式报错，渠道错误原样作为工具错误由模型转述；
  - `Schedule`：见 §7，仅挂在带个人 IM 来源的顶层会话，群内不挂载；
  - `AskUser` 不挂载：`SessionHeader::agent_managed()` 会话组装工具时不注册，工具定义一并缺席。

### 6.1 命令

`command.rs`，`/help` 列出；命令名大小写不敏感并容忍 `/cmd@bot`；**任何 `/` 开头的未知命令显式回复帮助，不交给模型**。

| 命令 | 作用 |
|---|---|
| `/new <消息>` | 强制新建 topic，必须带文字 |
| `/switch <前缀>` | 设置 focus（歧义列候选并拒绝），后续消息仍经 router |
| `/sessions` | 列出会话、空闲 TTL 与剩余时间；剩余时间按最后活动时间（尚无活动时取创建时间）计算，到期显示 0s，实际清理遵循 [TTL 清理规则](#41-ttl-清理) |
| `/stop` | 中止 focus |
| `/status`（`/stat`） | 会话、模型、上下文占比、cache 命中率、待附件数、当前聊天是否为 home |
| `/rename <标题>` | 覆盖标题并取消尚未回写的自动标题 |
| `/model [list\|[--global] <规格> [档位]]` | 有 focus 改 focus 会话，无 focus 改 **agent 模式模型**，`--global` 两者都改；切换须目标模型声明 `limit.context`；会话切换使缓存失效；agent 会话的切换不写 `state.json` |
| `/skills` | 列出 agent space 可发现技能 |
| `/schedule [list\|pause\|resume\|delete\|run <id>]` | 管理定时任务（私聊） |
| `/delete [前缀]` | 缺省 focus；按沉淀标记决定是否整理后删除，后台执行并回报，整理失败则保留会话 |
| `/clear [前缀]` | 直接删除，跳过整理 |
| `/sethome` `/unsethome` | 加入 / 移出 home |

agent 模式模型决定新会话的模型（curator 未配置独立模型时每次整理取当前值），落盘于 `~/.vva/space/model.json` 并优先于配置文件。

`/clear`、`/delete` 成功删除 focus 会话后，所有指向它的 binding 自动改指当前聊天范围内最近活跃的会话（尚无活动时取创建时间）；范围内无剩余会话则清空 focus。删除其他会话保留现有 focus。

群内开放 `/new`、`/switch`、`/sessions`、`/stop`、`/status`、`/rename`、`/clear`、`/sethome`、`/unsethome`、`/help`；其余个人管理命令提示在私聊使用，帮助只列当前可用命令。

## 7. Scheduler

`scheduler.rs`（任务表、状态、计时）、`schedule_tool.rs`（`Schedule` 工具）、`runtime/schedule.rs`（执行与通知）。

- **定义**：`<workspace>/schedule.json`，由 `Schedule` 工具与 `/schedule` 命令写入（原子写），人手修改按 mtime 热重载；自身写入不触发重载，解析失败保留旧任务表并告警（同一 mtime 不重复报错）。
- **状态**：`schedule_state.json`，每任务 `enabled`、`last_run`、`last_error`、连续 `failures`、interval 的 `next`，及全局 `last_tick`。
- **任务**：`id`、触发三选一、`prompt`、`session`（可选）、`bindings`（可选，二者至少其一）：

  ```json
  { "tasks": [
    { "id": "morning", "cron": "0 9 * * 1-5", "prompt": "汇总今日待办", "bindings": ["qq:c2c:<openid>", "qq:group:<gid>"] },
    { "id": "remind",  "at": "2026-10-11T09:00", "prompt": "提醒开会", "bindings": ["qq:c2c:<openid>"] },
    { "id": "check",   "every": "30m", "prompt": "检查新邮件，没有就保持静默", "session": "sess_abcde" }
  ] }
  ```
- **触发**：`cron` 为 5 字段（本地时区，`*`、数字、列表、范围、步长；日与周都受限取并集；周日 0 或 7）；`at` 为一次性本地时刻（`YYYY-MM-DDTHH:MM`），执行后自动停用，已过时刻拒绝创建；`every` 为 `30m / 2h / 1d`（分钟精度），**从上次完成起算**，天然不重叠。cron 到点时同一任务仍在执行则跳过本次。
- **执行目标**：`session` 指定时在该会话内执行（保留上下文；会话被 TTL 清理后任务永久失败）；否则在 `bindings[0]` 新建普通 topic 会话执行（无历史，prompt 须自足），进入路由候选并支持后续追问，后台创建不切换 focus，TTL 为 12h；指定已有会话时沿用其 TTL。周期任务应走后者。当前 channel 实例唯一（QQ），`bindings` 须同属该渠道。
- **通知**：一次触发只执行一次，最终回复与执行进度发往全部 home bindings，并登记 `message_index`；引用按当前聊天范围校验（已清理则按 §3.3 处理）。`bindings` 仅作执行来源配置，不决定通知目标。
- **静默**：触发 reminder 告知模型「无需通知用户时只回复 `[SILENT]`」，Runtime 对精确匹配的回复不发消息（历史照常保留）。
- **失败**：单次失败通知 home 聊天但任务继续；连续失败达 `task_max_failures` 自动停用；永久性错误（目标会话不存在、binding 非法）与一次性任务失败立即停用，并提示 `/schedule resume`；成功清零计数。单次执行超过 `task_timeout_secs` 中止并计为失败。
- **错过的任务**：每分钟记录 `last_tick`；启动时对账（回溯上限 7 天）：cron 错过、interval 的 `next` 已过期、once 已过未执行，均 `tracing::warn` 并汇总向 home 聊天提示一次，**不补跑**（interval 重排到之后；once 提示可 `/schedule run <id>` 补跑）。
- **管理**：`Schedule` 工具（`create / list / pause / resume / delete`，参数当场校验，`list` 与创建失败附当前本地时间供模型换算 `at`）；`/schedule run` 立即执行一次，已停用的需先 resume。创建只走自然语言 → 工具，无 `/schedule add`。

## 8. 进度汇报

`progress.rs`：轮次（含定时任务）超过 `progress_interval_secs` 未结束时，每隔该时长向聊天推送一段摘要，直到轮次结束。摘要由已完成的轮次**确定性**提炼，不调用模型：新建 / 修改的文件（按各编辑协议提取路径）、Shell 的 `intent`（不展示命令原文）、读取 / 检索与其他工具调用次数、工具报错次数、最近几条助手文字；本段无新动作时发「仍在处理」。同一会话只有一个汇报者，汇报消息计入 `message_index`，用户 reply 它会直达该会话；最终回复照旧在轮次结束时发送。

QQ C2C 用户触发的轮次使用官方 `stream_messages`：首次汇报创建一条进度消息，后续摘要在同一条消息内追加，保留已下发的正文前缀；每帧沿用 `msg_seq`，递增 `index`，并携带服务端返回的流式消息 id。轮次成功、失败或中止时追加结束状态，以 `input_state=10` 收尾。未到汇报时间即结束的轮次不创建进度消息。流式发送失败后停止追加，已创建的流仍尝试收尾，错误记入日志。群聊、频道及定时任务沿用逐条摘要发送。

## 9. 状态文件

工作区 `~/.vva/space/` 下：`bindings.json`、`message_index.jsonl`、`router.jsonl`、`schedule.json`、`schedule_state.json`、`model.json`、`files/`、`memory/`。`SessionHeader` 的 agent space 字段见 [domain_model §1.1](domain_model.md#11-session会话根实体)。

## 10. 验收要点

- **入站**：白名单外丢弃；各 chat 类型 binding 隔离；断线 resume 不重复不丢；无文字文件只缓冲；多附件保序；超长回复分段并标注。
- **路由**：reply 命中直达且零模型调用；reply 目标已清理 → warning + 新 session 带引用；`/switch` 歧义拒绝；零 session 直接创建；续接始终跟随 focus；新建可独立选择 focus 与 TTL；响应首行提示决策与 session id；非法决策显式报错；每次决策落 `router.jsonl`。
- **guide**：经 Steer 在安全边界注入；并行工具结果完整配对；末次响应期间的插话继续在本轮处理；空闲时直接新起轮次；stop 清空队列。
- **生命周期**：TTL 到期按标记沉淀后删除；沉淀失败保留；运行中不清理；重启恢复。
- **rollover**：达到预留线回合结束触发；旧 session 内容仅追加；新 session `continued_from` 正确且 `parent` 为空；binding 切换；旧 session 被 router 排除；失败不改变 session；压缩调用不触发工具；第二轮起重新命中前缀。
- **cache**：system 不变；动态内容走 reminder；记忆索引仅创建时快照；日期冻结；Anthropic 签名与 `cache_control`、Responses 链路不中断。
- **curator**：游标增量、幂等；写入产生 commit；skill 可被 `discover_skills` 发现；冲突不覆盖；完成推送摘要。
- **透明性**：session 可在 TUI / Web 按 `kind` / `origin` 过滤并接管；任一 IM 消息可追溯到决策与 session；所有降级与失败对用户可见，无静默 fallback。
