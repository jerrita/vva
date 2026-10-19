# Agent Space 设计 (Always-on IM Agent)

状态：P0–P3 已实现（`send_file` 已做，AskUser 不挂载；Telegram 未做）；以下各节中与实现不同处已标注。参考实现：`../agent`（Python，QQ/Telegram + 小模型 router）与 `../fairy-old`（分层 router、knowledge maintenance、scheduler）。

## 1. 目标

常驻、经 IM 触达的个人 agent。与同类工具（Hermes 等）的差异：

1. **cache 稳定复用**：session 只追加、前缀稳定，Anthropic `cache_control` 与 Responses `previous_response_id` 持续命中；
2. **session 管理透明**：agent 的每次对话都是普通 vva JSONL session，TUI / Web 可见、可接管；路由决策可追溯；
3. **自动知识沉淀**：curator 将对话整理为可审阅、有版本的记忆与 skill。

成本控制的关键是 **小模型 router**：仅凭元数据选择 session，不读历史。

## 2. 总体架构

```
IM ──► Channel ──► Inbound{binding, user, text, files, reply_to}
                       │
                       ▼
                    Router ── L0 确定性锚点 / L1 小模型 / L2 显式报错
                       │
        ┌──────────────┼────────────────┐
        ▼              ▼                ▼
     oneshot        existing / new   guide（Deliver 入队）
        │              │
        └─ 出站渲染（订阅 SessionView）─► Channel

  生命周期：空闲 TTL → curator 沉淀 → 删除
  上下文：回合结束达到预留线 → rollover
  Scheduler：cron → Deliver
```

新增 `src/agent_space/`（`channel/`、`binding.rs`、`router.rs`、`curator.rs`、`rollover.rs`、`scheduler.rs`），入口 `vva agent`。复用 `SessionManager`、`Deliver`、skills、JSONL，不改动其语义。所有 session 位于固定工作区 `~/.vva/space/`。

## 3. 概念

- **session**：当前聊天范围内未被接替的 Topic 会话为 router 候选；
- **focus**：每个 binding 一个默认续写指针；
- **running**：session 是否正在生成；
- **binding**：`channel:chat_id`（QQ 为 `qq:<msg_type>:<chat_id>`）；
- **待附件缓冲**：binding 级，收到无文字文件时暂存。
- **会话范围**：QQ 群聊仅使用 `origin.binding()` 等于当前群 binding 的会话；个人聊天共享排除群来源的个人会话池。路由候选、`/sessions` 与会话命令统一使用该范围。范围外或已失效的 focus 在读取时清除，跨范围引用显式报错。
- **home bindings**：后台通知的接收集合，与 focus、session 来源独立。`/sethome` 将当前聊天加入，`/unsethome` 将当前聊天移出，支持多个 home；命令无参数、幂等，沿用渠道白名单准入。`bindings.json` 每项增加 `home` 布尔字段，旧记录缺省为 `false`，重启后保留。

## 4. Router

### 4.1 分层

| 层级 | 触发 | 成本 |
|---|---|---|
| L0 | reply 命中 `message_index`；`/switch`；零 session 时直接创建 | 0 token |
| L1 | 其余 | 一次小模型调用（`settings.small_model`），或 `settings.agent.router.decisions` 声明的 OpenRouter Decisions / OpenCode System One API（`decisions.rs`：`action` / `session` choice 题 + `guide` noul 题，回填为同一 `RouteDecision`；关键题置信度低于 `min_confidence` 时续接当前 focus） |
| L2 | 小模型失败 / 非法输出 | 显式报错，附引用回复或 `/new <消息>` 指引 |

### 4.2 L1 输入输出

候选表只含 `id`、`title`（小模型生成的会话标题，首轮完成前为占位标题）、`running`、`focus`，按 id 排序（前缀稳定）；另含用户文字、被 reply 消息片段（`reply_to_text`）、待附件文件名与类型。不含历史正文。

输出强类型 `RouteDecision`：`action: oneshot | existing | new`、`session_id`、`running_intent: none | guide_existing`、`reason`。使用供应商原生结构化输出，非法值全部显式报错。

规则优先级：oneshot（无需历史、无长期价值）→ `guide_existing`（对运行中 session 补充/修正）→ existing（承接、追问、无法判断）→ new（明确无关的独立任务）。难以判断消息类型或会话归属时续接当前 focus；Decisions 的 `action` 或 `session` 置信度不足时，由 Router 执行这一默认规则，focus 正在运行则以 `guide_existing` 投递。无 focus 时显式提示 `/switch` 设置后重发，或 `/new <消息>` 新建。决策原因与 usage 照常记录在 `router.jsonl`。调用失败、缺失字段、非法取值保持显式报错。带媒体的消息不得走 oneshot；明确选择 running session 却非 `guide_existing` 时拒绝。

### 4.3 reply 目标已被清理

`message_index` 仅保存 `消息 id → session id`。目标 session 不存在时：warning 用户，新建 session，以 channel 入站自带的 `reply_to_text` 作为首条上下文，并处理当前消息。

索引命中现存的范围外会话时，Runtime 显式拒绝续接，提示发送新消息或 `/new <消息>`；该校验也适用于 home 聊天收到其他来源会话的通知后进行引用。

### 4.4 oneshot

新建 `kind=oneshot` session（跟随主 agent 模型），不进入 router 候选；回复后立即进入清理流程（不经 curator 沉淀，直接删除）。

### 4.5 guide_existing

经 `Deliver` 入队：运行中于轮间注入，空闲则触发新轮次；决策到执行间 session 已空闲则降级为普通 existing；残留插话在最终回复后继续触发轮次；session 被 stop/删除时清空。

### 4.6 可追溯

每次决策追加一行 `router.jsonl`（输入摘要、决策、reason、小模型 usage）；session header 记录 `routed_by`。

## 5. Session 生命周期

- 空闲超过 TTL → curator 增量沉淀 → 删除；沉淀失败则保留 session 并告警，下个周期重试；运行中或有排队消息的 session 不清理；
- 候选表上限：按 LRU 取前 N 个，超出者走同一清理流程；
- 重启后状态从 header 恢复。

## 6. 上下文管理：rollover

不原地改写历史。回合结束后若上下文达到 `limit.context − 预留`，由**主模型在同一 session 末尾追加压缩 prompt**（请求参数与上一轮完全一致，复用 cache 前缀，按 cache 读价计费），摘要作为追加轮次留在旧 session。随后：

1. 新建 session，`continued_from` 指向旧 session（不借用 subagent 的 `parent`），首条 `<system-reminder>` 为摘要，除 system 前缀外不复制任何对话轮次（交接无冗余，摘要须自足）；
2. binding focus 与 `message_index` 后续消息切到新 session；
3. 旧 session 写 `superseded_by`，router 排除，之后走常规 TTL 清理。

约束：

- 压缩请求不改变 system、工具、模型、推理档位、`tool_choice`；prompt 要求不调用工具，若仍调用则视为失败（`tool_choice` 与 thinking 变更是否使缓存失效需实测）；
- 失败：显式通知用户，session 不变，下个回合结束重试；
- 预留 ≥ 压缩 prompt + 摘要 `max_tokens`；
- 轮内溢出（单轮工具输出撑满窗口）不在此机制范围，遇到时显式报错。

摘要分节：目标、偏好、关键事实、约束、决策、待办、未决问题；不确定信息标注"未确认"。

## 7. Cache 稳定性纪律

- 同 session 内 system 与前缀字节级不变，只追加 turn；
- 技能清单、记忆索引、后台通知、定时触发一律以 `<system-reminder>` user turn 追加；
- 记忆索引仅在 session 创建时快照：agent space 会话（排除 curator）首次落盘时追加 `<system-reminder type="memory">` 轮次，内容为当时 `memory/MEMORY.md` 全文并注明记忆库绝对路径，细节由模型按需 Read；`MEMORY.md` 不存在则不注入。L1 全文注入、不截断；体量从设计上有界：`MEMORY.md` 只列领域，领域索引与主题按需 Read，增长的是领域数而非条目数；
- rollover 接替会话复制的 system 前缀中，旧记忆快照以**当前** `MEMORY.md` 原位重生成（旧前缀没有则不加，当前无索引则丢弃旧快照）；
- 记忆更新通知：curator 结果为 `Updated` 时，向现存 topic / oneshot 会话（排除 curator、已 superseded、来源会话）追加 `<system-reminder type="memory-updated">`（只含 curator 摘要，提示 Read `MEMORY.md`）。该路径为 `ManagedSession::note`：只追加不起轮次，空闲直接落盘，运行中入队到轮次间隙或本轮收尾追加；追加在历史末尾，不改动已有前缀；
- system prompt 的 `Date` 在 session 创建时冻结（或改由 reminder 提供），避免跨日全体 miss；
- router 调用前缀固定，session 表与用户消息置尾；
- 状态页展示各 session cache 命中率与 router 小模型成本，数据来自 `usage` 与成本配置。

## 8. Curator（知识沉淀）

- 模型可通过 `settings.agent.curator_model` 独立配置，启动时校验；省略时每次整理取当前 agent 模式模型。独立配置后不受 `/model` 切换影响；
- 由系统在删除会话前触发的后台 session（空闲 TTL 到期的周期扫描，或 `/delete`；`/clear` 与 rollover 本身不触发，被接替的旧会话按 TTL 照常整理）（`kind=curator`），非聊天模型派生的 subagent；
- 只处理游标之后的增量 turns，重复触发返回 `unchanged`；结果三态 `updated | unchanged | failed`；
- 产出写入 `~/.vva/space/memory/`（三层：`MEMORY.md` 只列领域 → `<领域>/INDEX.md` 列主题 → `<领域>/<topic>.md` 内容；纳入 git，每次整理一个 commit，提交信息含来源 session 与范围）；反复出现的工作流晋升为 skill 写入工作区 `.vva/skills`（`~/.vva/space/.vva/skills`，仅 agent space 会话可发现，不污染全局技能）；
- 完成后向用户推送摘要；git 冲突时不覆盖，告警并跳过；

## 9. Channel

- `Channel` trait：`run(inbound_tx)` + `send(OutboundMessage)`；出站渲染器订阅 `SessionView`，负责分段（超限时在末段明确标注截断，不静默截断）、markdown 失败降级纯文本；
- **命令排版**：斜杠命令统一使用 Markdown；帮助、会话、模型、技能与状态以带标题的表格展示，定时任务逐项使用两列表格；操作反馈与错误使用少量 emoji，附必要的后续操作提示。动态文本压平空白并转义 Markdown 标记，描述预览截断时明确显示省略号。
- 首发 QQ（gateway identify / resume / heartbeat 按官方规范，参考 `../agent` 的 `ref/qq` 已验证实现），其后 Telegram；
- 白名单外用户静默丢弃并记日志；
- 群内开放 `/new`、`/switch`、`/sessions`、`/stop`、`/status`、`/rename`、`/clear`、`/sethome`、`/unsethome`、`/help`；其余个人管理命令提示在私聊使用，帮助只列当前可用命令。群内不挂载 `Schedule` 工具。
- **通知目标**：自动整理更新 / 失败、后台删除完成、定时任务结果 / 进度 / 失败 / 错过 / rollover 提示统一发送到全部 home bindings。每次发送查询最新集合，单个目标失败仍继续发送其他目标；无 home 时执行照常继续，跳过推送并记录日志。普通对话回答、命令反馈、对话进度与附件反馈仍发往发起聊天。`/status` 展示当前聊天是否为 home。
- home 可配置为群：该群将接收全空间后台通知。home 标记不授予跨范围会话访问能力。长期记忆注入与文件 / 工具工作区仍共享，本次会话范围调整不建立记忆或文件访问隔离。
- 文件：无文字 → 进入待附件缓冲，不触发模型，回复"已收到"；有文字（含 caption）→ 一次路由，文件随之进入目标 session；落盘 `files/`，历史只记绝对路径文本；多附件保持顺序；缓冲超时（`pending_files_ttl_secs`）明确提示并清理缓冲（文件保留在磁盘）；
- `SendFile` 工具已实现（`send_file.rs`）：渠道绑定、经 `SessionManager::with_tool_factory` 按会话 header 运行期注入（仅带 origin 的顶层会话，subagent 不挂载）；参数 `path`（工作区相对 / 绝对 / `~`）与可选 `caption`，文件缺失或非普通文件显式报错，渠道错误（如 QQ guild / dm 不支持上传）原样作为工具错误由模型转述；
- AskUser 不挂载：agent space 会话（`SessionHeader::agent_managed()`）组装工具时不注册 `AskUserTool`，工具定义一并缺席；channel 级工具白名单未做；
- 命令（`command.rs`，`/help` 列出）：`/new <消息>`（强制新建 topic，必须带文字）、`/switch <前缀>`（歧义列候选并拒绝，只设置 focus，后续消息仍经 router）、`/sessions`、`/stop`（中止 focus）、`/status`（`/stat`；会话、模型、上下文占比、cache 命中率、待附件数）、`/rename <标题>`（覆盖标题并取消尚未回写的自动标题）、`/model [list|[--global] <规格> [档位]]`（查看 / 列出 / 切换：有 focus 改 focus 会话，无 focus 改 **agent 模式模型**，`--global` 两者都改；agent 模式模型决定新会话的模型（curator 未配置独立模型时，每次整理取当前值），落盘于 `~/.vva/space/model.json` 并优先于配置文件，会话与模式切换都须目标模型声明 `limit.context`；agent 会话的切换不写 `state.json` 的「上次选用模型」；会话切换会使其缓存失效）、`/skills`（列出 agent space 可发现技能）、`/schedule [list|pause|resume|delete|run <id>]`（管理定时任务，创建走自然语言 → Schedule 工具）、`/delete [前缀]`（缺省 focus；**先 curator 整理再删除**，后台执行并回报，整理失败则保留会话）、`/clear [前缀]`（**直接删除，跳过整理**）；命令名大小写不敏感并容忍 `/cmd@bot`；**任何 `/` 开头的未知命令都显式回复帮助，不会交给模型**；
- 出站只发最终文本（`deliver_wait` 的结果），不做流式增量；失败 / 中止显式回复错误文本；
- 入站循环顺序完成路由与建会话，轮次在独立任务执行，故运行中的 session 仍可被 reply / guide 插话；oneshot 不抢占 focus；已不存在或范围外的 focus 在下次读取时清理；rollover 后 `replace_focus` + `MessageIndex::repoint` 并提示用户，home 标记保留；
- `message_index` 键带 channel 前缀（`qq:<id>`）。

## 10. Scheduler

实现于 `scheduler.rs`（任务表、状态、计时）、`schedule_tool.rs`（`Schedule` 工具）与 `runtime/schedule.rs`（执行与通知）。参考 `../fairy-old`（`../agent` 无此功能）。

- **定义**：`<workspace>/schedule.json`，由 `Schedule` 工具与 `/schedule` 命令写入（原子写），人手修改按 mtime 热重载；自己的写入不触发重载，解析失败保留旧任务表并告警（同一 mtime 不重复报错）。
- **状态**：`schedule_state.json`，每任务 `enabled`、`last_run`、`last_error`、连续 `failures`、interval 的 `next`，以及全局 `last_tick`。
- **任务**：`id`、触发三选一、`prompt`、`session`（可选）、`bindings`（可选，二者至少其一）：

  ```json
  { "tasks": [
    { "id": "morning", "cron": "0 9 * * 1-5", "prompt": "汇总今日待办", "bindings": ["qq:c2c:<openid>", "qq:group:<gid>"] },
    { "id": "remind",  "at": "2026-10-11T09:00", "prompt": "提醒开会", "bindings": ["qq:c2c:<openid>"] },
    { "id": "check",   "every": "30m", "prompt": "检查新邮件，没有就保持静默", "session": "sess_abcde" }
  ] }
  ```
- **触发**：`cron` 为 5 字段（本地时区，`*`、数字、列表、范围、步长；日与周都受限取并集；周日 0 或 7）；`at` 为一次性本地时刻（`YYYY-MM-DDTHH:MM`），执行后自动停用，已过时刻拒绝创建；`every` 为 `30m / 2h / 1d`（分钟精度），**从上次完成起算**，天然不重叠。cron 到点时同一任务仍在执行则跳过本次（debug 日志）。
- **执行目标**：`session` 指定时在该会话内执行（保留上下文；会话被 TTL 清理后任务永久失败）；否则在 `bindings[0]` 新建 oneshot 会话执行（无历史，prompt 须自足）。周期任务应走后者。
- **通知**：一次触发只执行一次，最终回复与执行进度发往发送时配置的全部 home bindings，并登记 `message_index`。引用仍按当前聊天范围校验（已清理则按 §4.3 处理）。`bindings` 保留为执行来源配置，首项用于新建 oneshot，后续条目及 session 来源均不再决定通知目标。当前 channel 实例唯一（QQ），`bindings` 须同属该渠道。
- **静默**：触发 reminder 告知模型"无需通知用户时只回复 `[SILENT]`"，Runtime 对精确匹配的回复不发消息（会话历史照常保留）。
- **失败**：单次失败通知 home 聊天但任务继续；连续失败达 `task_max_failures`（默认 3）自动停用；永久性错误（目标会话不存在、binding 非法）与一次性任务失败立即停用，并提示 `/schedule resume`。成功清零计数。单次执行超过 `task_timeout_secs`（默认 600）中止并计为失败。
- **错过的任务**：每分钟记录 `last_tick`；启动时对账（回溯上限 7 天）：cron 错过、interval 的 `next` 已过期、once 已过未执行，均 `tracing::warn` 并汇总向 home 聊天提示一次，**不补跑**（interval 重排到之后；once 提示可 `/schedule run <id>` 补跑）。
- **管理**：`Schedule` 工具（`create / list / pause / resume / delete`，仅挂在带个人 IM 来源的顶层会话，参数当场校验，`list` 与创建失败附当前本地时间供模型换算 `at`）；`/schedule [list|pause|resume|delete|run <id 前缀>]`（私聊使用；`run` 立即执行一次，已停用的需先 resume）。创建只走自然语言 → 工具，不提供 `/schedule add`。

## 11. 数据模型

- `SessionHeader` 新增：`kind`（`topic | oneshot | curator`）、`origin`（`{channel, chat_id}`）、`routed_by`、`superseded_by`、`last_activity`（强类型字段）；
- 目录：`bindings.json`、`message_index.jsonl`、`router.jsonl`、`schedule.json`、`schedule_state.json`、`model.json`、`memory/`；
- 配置：`settings.agent_space`（channels、allow-list、router、TTL、预留、K）。

## 12. 预期行为（验收要点）

- **入站**：白名单外丢弃；各 chat 类型 binding 隔离；断线 resume 不重复不丢；无文字文件只缓冲；多附件保序；超长回复分段并标注。
- **路由**：reply 命中直达且零模型调用；reply 目标已清理 → warning + 新 session 带引用；`/switch` 歧义拒绝；零 session 直接创建；候选表只含四字段；非法决策全部显式报错；带媒体不得 oneshot；running 且非 guide 拒绝；每次决策落 `router.jsonl`。
- **oneshot / guide**：oneshot 不进候选并随后清理；guide 经 `Deliver` 不打断运行；竞态降级为 existing；残留插话不丢；stop 清空队列。
- **生命周期**：TTL 到期先沉淀后删除；沉淀失败保留；运行中不清理；重启恢复。
- **rollover**：达到预留线回合结束触发；旧 session 内容仅追加；新 session `continued_from` 正确且 `parent` 为空；binding 切换；旧 session 被 router 排除；失败不改变 session；压缩调用不触发工具；第二轮起重新命中前缀。
- **cache**：system 不变；动态内容走 reminder；记忆索引仅创建时快照；日期冻结；Anthropic 签名与 `cache_control`、Responses 链路不中断。
- **curator**：游标增量、幂等；写入产生 commit；skill 可被 `discover_skills` 发现；冲突不覆盖；完成推送摘要。
- **透明性**：session 可在 TUI/Web 按 `kind`/`origin` 过滤并接管；任一 IM 消息可追溯到决策与 session；所有降级与失败对用户可见，无静默 fallback。

## 13. 配置与默认值

全部位于 `settings.agent`，可配置；router 缺省复用 `settings.small_model`，可选 `router.decisions`。

| 字段 | 默认 | 说明 |
|---|---|---|
| `model` | `settings.default` | topic 与 oneshot session 共用的主 agent 模型 |
| `ttl_secs` | 18000 (5h) | 普通 session 空闲清理 |
| `oneshot_ttl_secs` | 0 | oneshot 回复完即可清理（不做记忆沉淀） |
| `rollover_reserve_tokens` | 32000 | 上下文预留，覆盖压缩 prompt 与摘要 |
| `pending_files_ttl_secs` | 600 | 待附件缓冲等待时间 |
| `task_timeout_secs` | 600 | 定时任务单次执行超时 |
| `progress_interval_secs` | 180 | 长轮次进度汇报间隔，0 关闭（见 §进度汇报） |
| `task_max_failures` | 3 | 定时任务连续失败自动停用的次数 |
| `channels.qq` | — | `app_id` / `secret` / `allowed_users` |



## 进度汇报

`progress.rs`：轮次（含定时任务）超过 `progress_interval_secs` 未结束时，每隔该时长向聊天推送一段摘要，直到轮次结束。摘要由会话已完成的轮次**确定性**提炼，不额外调用模型：新建 / 修改的文件（Write / Edit，含 hashline 与 V4A 补丁路径）、Shell 的 `intent`（不展示命令原文）、读取/检索与其他工具调用次数、工具报错次数、最近几条助手文字；本段无新动作时仍发「仍在处理」以示存活。同一会话只有一个汇报者（排队的后续投递不重复），汇报消息计入 `message_index`，用户 reply 它会直达该会话。最终回复照旧在轮次结束时发送。

## 技能变动与删除清理

- **技能变动提示**：每条新消息提交前重新发现技能，与会话历史中最近一次 `skills` / `skills-changed` 提醒携带的指纹索引（`index="name:hash,..."`，hash 覆盖 description 与正文）比对；有新增 / 更新 / 移除时，在用户消息之前注入 `<system-reminder type="skills-changed">`，并以新索引作为之后的基线（同一变动不重复提示）。旧版不带索引的清单整体刷新一次。子代理 / curator 会话不注入。`/skills`（IM 与 TUI）每次现扫目录，TUI 同步刷新斜杠调用缓存。
- **删除同步清理**：来源会话被删除（TTL、`/delete`、`/clear`）时，`Curator::forget` 一并清除其游标、残留的整理输入，并删除为它创建的 curator 会话（标题 `[curator] <session_id>`）；curator 会话清理失败只告警，由 TTL 兜底。

## 范围约束

- 入口为 `vva agent`；TUI / Web 暂不接入 agent space 逻辑（session 仍是普通 JSONL，新增 header 字段均有缺省，旧界面可照常读取）。

## 14. 分期

- **P0**：QQ channel、binding、header 新字段、L0 路由与命令、`vva agent`；
- **P1**：L1 router、oneshot、`router.jsonl`、`message_index`、guide；
- **P2**：TTL 清理、curator、记忆目录、日期冻结；
- **P3**：rollover、scheduler；
- **P4**：Telegram、Web agent space 视图。
