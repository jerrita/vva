# 存储

[`src/storage/mod.rs`](../src/storage/mod.rs) 定义 `SessionStore`，[`src/storage/jsonl.rs`](../src/storage/jsonl.rs) 提供 `JsonlSessionStore`。

设计要求：

1. **统一领域存储**：落盘领域实体与强类型元数据，与厂商报文解耦；
2. **抗崩溃**：每轮即时落盘，进程中断保留已完成轮次；
3. **追加写**：每轮只向文件尾追加一行，长会话不触发全量序列化；
4. **流式读取，Git diff 友好**。

## 1. 路径

| 对象 | 路径 |
|---|---|
| 项目目录 | `~/.vva/sessions/{project_key}/`，`project_key` 由工作区绝对路径编码（分隔符替换为 `-`） |
| 会话文件 | `{project_key}/{session_id}.jsonl`，`session_id` 为 `sess_` + 5 位小写十六进制 |
| Scratch Pad | `{project_key}/scratch/`，每项目一份。System Prompt 的 Environment Context 写入其绝对路径，读写使用该绝对路径；相对路径（含 `scratch/...`）解析到工作区根，工作区内可存在名为 scratch 的目录 |
| 大体积工具输出 | `{project_key}/tool_outputs/` |

## 2. 记录布局

```
Line 1:    {"type":"header","version":2,"id":"sess_a1b2c","workspace_root":"/path","current_model":"openai/gpt-5-terra","editor":"hashline",...}
Line 2:    {"type":"turn","id":"turn_001","role":"user","messages":[{"parts":[...]}],"meta":{}}
Line 3:    {"type":"turn","id":"turn_002","role":"assistant","messages":[...],"usage":{...},"meta":{"response":{"response_id":"resp_123",...}}}
Line 4..N: {"type":"turn",...}
```

- **Header（`JsonlRecord::Header`）**：[`SessionHeader`](../src/model/session.rs)，字段含义见 [domain_model §1.1](domain_model.md#11-session会话根实体)。版本不符的会话不解析：列表跳过并告警，加载报错；工具协议随版本整体变化，不做旧格式转换。可选字段缺省时全部省略，旧文件无需迁移。
- **Turn（`JsonlRecord::Turn`）**：每完成一轮（用户提问、模型响应、工具返回）以 `O_APPEND` 追加一行，含该轮 `messages`、`usage`、`meta`。
- **Header 重写**：创建会话或模型 / 标题 / 网络搜索配置 / 沉淀标记变更时调用 `save_header`，将「新首行 + 全部已有轮次」写入同目录临时文件，`fsync` 后 `rename` 原子替换，中途失败时原文件完整。其余时间文件只被追加。

## 3. SessionStore

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn save_header(&self, session: &Session) -> Result<(), StoreError>;
    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError>;
    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError>;
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError>;
    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError>;
    async fn ensure_scratch(&self, workspace: &Path) -> Result<PathBuf, StoreError>;
}
```

## 4. 容灾与读取

- **抗崩溃**：追加写单轮带换行并刷新缓冲，进程终止时保留已完成轮次；
- **轻量列表**：`list_sessions` 扫描各项目 `.jsonl`，仅反序列化首行；`turn_count` 按剩余字节中的换行符计数，不解析消息体；
- **损坏行显式报错**：按 `JsonlRecord` 枚举分发，损坏行返回强类型 `StoreError`，不吞没、不替换为空数据。
