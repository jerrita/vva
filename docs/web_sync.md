# Web 会话同步协议

## 问题与边界

Harness 通过 `watch<Arc<SessionView>>` 发布进程内只读视图，流式文本按 33ms 节流。
将每次发布直接序列化为完整 SSE 快照，会重复传输历史、图片、工具结果和累计草稿。
持续输出时带宽约为 `发布频率 × (历史大小 + 累计草稿大小)`，并随会话增长。

进程内视图继续服务 TUI。Web 层在 `src/web/sync.rs` 投影传输数据，
前端在 `frontend/src/sessionSync.ts` 重建展示快照。领域模型、持久化与供应商协议保持原样。
Web 轮次投影包含消息、usage、elapsed_ms、worked_ms；Turn.meta 留在服务端。

## 帧与版本

`GET /api/sessions/{id}/events` 使用命名 SSE 事件：

- `snapshot`：每条连接首帧，包含当前 revision、session、draft、state、persisted、notices。
- `delta`：包含 base_revision、revision、state、persisted，以及可选的 session、turns、draft、notices。

增量字段语义：

| 字段 | 应用规则 |
|---|---|
| session | 整体替换除 turns 外的会话信息；省略的 title/current_variant 随之清空 |
| turns | 保留前 from 轮，用 items 替换剩余尾部；支持追加、回退、已有轮次修改 |
| draft | 截取到 len，再应用 updates；set 替换指定部件，text/reasoning 追加字符串后缀 |
| notices | 整体替换通知列表 |
| 可选字段缺席 | 保持原值 |

前端以一帧为单位原子更新历史、草稿和状态，保留未变化的对象引用。
版本必须满足 `base_revision == 本地 revision` 且 `revision > base_revision`。
解析、版本或部件类型错误关闭订阅并展示错误，避免在失配基线上继续追加。
设置命令的 HTTP 响应只确认命令完成，界面状态由 SSE 统一驱动。

## Shell 输出增量

首帧的 `shell_outputs` 按工具 `call_id` 提供完整展示快照。增量帧的同名字段为操作列表，仅包含变化或删除的调用：

| 操作 | 应用规则 |
|---|---|
| set | 用 output 整体设置该调用的输出 |
| append | 校验调用的 base_revision，按 UTF-16 单元丢弃头部 drop，再追加 text，同时更新 revision、start、total_bytes、finished、error |
| remove | 删除该调用 |

后端以连接上次发送的保留区间为基线计算增量。尾部区间有重叠且保留文本未变时发送 append；CR 改写或保留区间已无重叠时发送 set。未变化的输出以 Arc 身份跳过，不序列化其他调用的输出。前端保留未变化调用的对象引用；版本或区间错误按既有协议错误处理。

采集、缓冲、持久化和两端展示行为见 [Shell 实时输出](background_tasks.md#shell-实时输出)。

## 背压与重连

每条连接只持有上一份已发送视图与 watch 中的最新视图。
首次读取使用 borrow_and_update，同步建立首帧基线并标记版本已读。
慢连接允许 watch 合并中间发布，增量始终比较该连接上次已发送版本和当前版本，
无需无界事件队列，也无需连续消费所有 revision。

网络中断后 EventSource 自动重连；新连接从当前快照建立基线，不依赖旧连接的版本或事件回放。
重连会再次传输完整展示快照。

## 成本与验证

- 历史 Arc 未变化时跳过历史比较与序列化。
- 稳定连接中的文本/思考增量只传输新增字符串，带宽与历史及累计文本大小解耦。
- 结构改变时发送变化部件或历史尾部；轮次提交会发送该轮的最终内容。
- 此改动聚焦网络传输；进程内草稿复制、前缀比较及前端渲染成本仍由现有视图机制承担。

验证命令：

```sh
cargo test
cargo test web::sync::tests::synthetic_stream_bandwidth -- --nocapture
cargo clippy -- -D warnings
cd frontend && npm run build
node --experimental-strip-types --test sessionSync.test.mjs
```

合成带宽测试比较 34KB 历史下连续 30 帧的完整快照和增量 JSON 字节数，
独立测试验证百万字符历史及累计草稿不增加普通文本增量帧大小。
测试数据使用合成内容，不读取用户会话。
