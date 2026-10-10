# 存储引擎与持久化规范 (Storage Engine & Persistence Specification)

## 1. 架构定位与设计哲学

代码位置：[`src/storage/mod.rs`](../src/storage/mod.rs) 与 [`src/storage/jsonl.rs`](../src/storage/jsonl.rs)

在 vva 中，持久化引擎满足以下设计要求：
1. **统一领域存储**：磁盘持久化保存领域模型实体与强类型元数据；
2. **写入抗崩溃**：对话过程中每轮交互即时落盘，在断电或进程中断时保留会话数据；
3. **轮次追加写**：每轮交互只向文件尾部追加一行，长会话不因新增轮次触发全量序列化；
4. **流式增量加载与 Git Diff 友好**：便于版本管理与流式解析。

---

## 2. JSONL Append-Only 存储格式

vva 默认采用 **JSONL (JSON Lines) 追加写存储引擎**（[`JsonlSessionStore`](../src/storage/jsonl.rs)）。

### 2.1 文件路径与命名
- **根存储目录**：`~/.vva/sessions/`（或测试指定的独立目录）
- **项目目录**：`~/.vva/sessions/{project_key}/`，`project_key` 由工作空间绝对路径编码（路径分隔符替换为 `-`）
- **会话文件**：`~/.vva/sessions/{project_key}/{session_id}.jsonl`，`session_id` 为 `sess_` + 5 位小写十六进制
- **Scratch Pad**：`~/.vva/sessions/{project_key}/scratch/`，每个项目一份；System Prompt 的 Environment Context 写入该目录的绝对路径。读写须使用该绝对路径。相对路径（含 `scratch/...`）解析到工作空间根目录而非 Scratch Pad；不提供 `scratch` 别名（工作空间内可以合法存在名为 scratch 的目录）。

### 2.2 行级记录布局 (Line Layout)
每个 `.jsonl` 文件严格遵循以下两段式结构：

```
Line 1:      {"type":"header","id":"sess_a1b2c","title":"Refactor","workspace_root":"/path","created_at":"...","current_model":"openai/gpt-5-terra","current_variant":"high","web_search":true,"team":true}
Line 2:      {"type":"turn","id":"turn_001","role":"user","messages":[{"parts":[...]}],"meta":{}}
Line 3:      {"type":"turn","id":"turn_002","role":"assistant","messages":[{"parts":[...]}],"usage":{...},"meta":{"response":{"response_id":"resp_123",...}}}
Line 4..N:   {"type":"turn",...}
```

- **第 1 行 (`JsonlRecord::Header`)**：
  - 存储全局会话头信息 [`SessionHeader`](../src/model/session.rs)；
  - 绑定当前会话的 `current_model`、`current_variant`、`web_search`、`team`（缺省 `true`；关闭时不挂载 `Agent` / `SendMessage` / `WaitSession` 工具，`TaskStop` 不再接受 `sess_` id）、`editor` 与 `workspace_root`（新会话取模型 `annotation.editor`，未声明时取 `settings.editor`，缺省 `hashline`；JSONL 省略该字段时按早期 `replace` 读取）；
  - agent space 可选字段：`kind`（`topic` 缺省不写出 / `oneshot` / `curator`）、`origin`（`{channel, chat_id}`）、`routed_by`、`continued_from`（rollover 接替会话所承接的旧会话）、`superseded_by`、`last_activity`；缺省时全部省略，旧文件无需迁移；
  - 会话创建或发生模型/标题/网络搜索配置变更时调用 `save_header`，按“首行 Header + 全部已有轮次”写入同目录临时文件，`fsync` 后 `rename` 原子替换；重写中途失败时原文件与历史保持完整。其余时间文件只被追加。
- **第 2..N 行 (`JsonlRecord::Turn`)**：
  - 每完成一轮交互（用户提问、模型响应或工具执行返回），直接向文件尾部执行标准 `O_APPEND` 追加写；
  - 独立记录该轮的全部 `messages`、`usage` 及专属 `meta`。

---

## 3. SessionStore Trait 接口定义

代码位置：[`src/storage/mod.rs`](../src/storage/mod.rs)

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 保存或更新会话 Header（重写首行并回写全部已有轮次）
    async fn save_header(&self, session: &Session) -> Result<(), StoreError>;

    /// 向指定会话尾部追加单轮 Turn（核心 Append-Only 写入路径）
    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError>;

    /// 读取并重构完整的 Session 实体（包含所有历史 Turns）
    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError>;

    /// 遍历列出当前存储目录下的所有会话元信息摘要（轻量级，无需完全反序列化消息体）
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError>;

    /// 删除指定会话文件
    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError>;

    /// 确保项目目录与 scratch 文件夹存在，返回 scratch 路径
    async fn ensure_scratch(&self, workspace: &Path) -> Result<PathBuf, StoreError>;
}
```

---

## 4. 容灾与零拷贝流式性能优势

1. **抗进程崩溃**：
   - 使用 `OpenOptions::new().append(true)` 打开文件，单轮写入附带换行符并刷新缓冲区。进程终止时保留已完成轮次。
2. **轻量会话列表检索 (`list_sessions`)**：
   - 提取会话列表或在 TUI `/sessions` 弹窗展示时，扫描各项目子目录中的 `.jsonl` 文件，**仅反序列化首行**（获取 `SessionHeader`）；`turn_count` 按剩余字节流中的换行符计数得出，不解析消息体、不分配行字符串。
3. **零数据兜底保证**：
   - 解析记录时使用枚举分发（`JsonlRecord`），检测到损坏行返回强类型 `StoreError`，不吞没错误或替换为空数据。
