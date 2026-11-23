# Web 会话同步协议

## 1. 问题与边界

core Harness 经 `watch<Arc<SessionView>>` 发布进程内只读视图，流式文本按 33ms 节流。若把每次发布都序列化为完整 SSE 快照，会重复传输历史、图片、工具结果和累计草稿：带宽约为 `发布频率 × (历史大小 + 累计草稿大小)`，并随会话增长。

Web 层在 `src/web/sync.rs` 计算增量，前端在 `frontend/src/sessionSync.ts` 重建展示快照。领域模型、持久化与供应商协议不受影响，进程内视图继续服务 TUI。

- 应用投影把快照与按 SessionId 关联的应用记录合并，生成 Web 协议。编辑协议、web 开关与 Space 展示字段保持扁平形状。
- 统计由应用共用的 `app/stats.rs` 计算。
- Web 轮次投影包含消息、usage、elapsed_ms、worked_ms 与 steering / compaction 标记（见[领域模型 §1.2](domain_model.md#12-turn)）；`Turn.meta` 留在服务端。

## 2. 帧与版本

`GET /api/sessions/{id}/events` 使用命名 SSE 事件：

- `snapshot`：每条连接的首帧，含当前 revision、session、draft、state、persisted、notices。
- `delta`：含 base_revision、revision、state、persisted，以及可选的 session、turns、draft、notices。

| 增量字段 | 应用规则 |
|---|---|
| `session` | 整体替换 turns 以外的会话信息；省略的 `title` / `current_variant` 随之清空 |
| `turns` | 保留前 `from` 轮，用 `items` 替换其余尾部；支持追加、回退与修改已有轮次 |
| `draft` | 先截取到 `len`，再应用 `updates`：`set` 替换指定部件，`text` / `reasoning` 追加字符串后缀 |
| `notices` | 整体替换通知列表 |
| `pending_inputs` | 整体替换待发消息列表；首帧带完整队列，消费时与新增的用户轮次同帧更新 |
| 未出现的可选字段 | 保持原值 |

- 前端以一帧为单位原子更新历史、草稿和状态，保留未变化的对象引用。
- 版本校验要求 `base_revision` 等于本地 revision，且 `revision > base_revision`。解析、版本或部件类型错误时关闭订阅并展示错误。
- 设置命令的 HTTP 响应只确认命令完成，界面状态由 SSE 驱动。
- `/compact` 分发到 `POST /api/sessions/{id}/compact`，不作为聊天消息提交。接口返回 202 表示已接受；运行状态、摘要与错误经 SSE 同步。

## 3. 运行状态与浏览器通知

- 前端以 SSE `state` 从非空变为空识别运行结束。初次打开空闲会话不提示，重连后的重复空闲快照不重复提示。
- 运行开始时记录 Notice ID，结束时用这次运行新增的 turn Notice 区分错误与主动停止；历史错误不影响后续运行。
- 标签页标题显示当前会话名称，未命名时显示会话 ID；运行中前缀 `●`。
- 当前会话在标签页隐藏或窗口失焦时结束，标题保留 `✓` 或 `✕`；重新查看标签页、切换会话或开始新运行后清除。
- 首次发送消息时通过用户操作请求 Notification API 授权，每次页面加载最多请求一次。
- 通知要求浏览器支持、安全上下文（HTTPS 或 localhost）且已授权。
- 未在查看的当前会话和已订阅的后台会话结束时发送系统通知，错误结束使用失败标题。点击通知聚焦窗口并打开对应会话。
- 通知只在页面保持打开且收到运行结束状态时发送。

## 4. Shell 输出增量

首帧的 `shell_outputs` 按工具 `call_id` 提供完整展示快照。增量帧的同名字段是操作列表，只含变化或删除的调用：

| 操作 | 应用规则 |
|---|---|
| `set` | 用 `output` 整体设置该调用的输出 |
| `append` | 校验调用的 `base_revision`，按 UTF-16 单元丢弃头部 `drop`，再追加 `text`，同时更新 revision、start、total_bytes、finished、error |
| `remove` | 删除该调用 |

- 后端以连接上次发送的保留区间为基线。尾部区间重叠且保留文本未变时发送 `append`；CR 改写或保留区间已无重叠时发送 `set`。
- 未变化的输出以 Arc 身份跳过，不序列化。
- 前端保留未变化调用的对象引用；版本或区间错误按协议错误处理。

采集、缓冲、持久化与展示见 [background_tasks §2](background_tasks.md#2-shell-实时输出)。

## 5. 背压与重连

- 每条连接只保存上一份已发送视图，并读取 `watch` 中的最新视图，没有无界事件队列。
- 首次读取使用 `borrow_and_update` 建立首帧基线并标记版本已读。
- 慢连接允许 `watch` 合并中间发布；增量总是比较该连接上次已发送的版本与当前版本。
- 网络中断后 EventSource 自动重连。新连接从当前快照建立基线，不依赖旧连接的版本或事件回放；重连会再次传输完整展示快照。

## 6. 传输成本

- 历史 Arc 未变化时跳过历史比较与序列化。
- 稳定连接中的文本 / 思考增量只传输新增字符串，带宽与历史及累计文本大小无关。
- 结构改变时发送变化的部件或历史尾部；轮次提交时发送该轮的最终内容。
- 本协议只处理网络传输。进程内的草稿复制、前缀比较与前端渲染由视图机制承担。
