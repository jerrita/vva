# 存储引擎与持久化规范 (Storage Engine & Persistence Specification)

## 1. 架构定位与设计哲学

代码位置：[`src/storage/mod.rs`](../src/storage/mod.rs) 与 [`src/storage/jsonl.rs`](../src/storage/jsonl.rs)

在 vva 中，持久化引擎必须满足以下严苛要求：
1. **统一领域存储**：无论与哪家供应商交互，磁盘持久化只存储纯净的领域模型实体与强类型元数据；
2. **高频写入抗崩溃**：对话过程中每轮交互都应即时落盘，严禁因意外断电或进程中断丢失整个会话；
3. **零重写开销**：传统 JSON 方案每次新增消息都必须读取全量数组并全量序列化覆写整个大文件，在长会话中存在严重的 IO 放大与磁盘写竞争；
4. **流式增量加载与 Git Diff 友好**：便于版本管理与零拷贝解析。

---

## 2. JSONL Append-Only 存储格式

vva 默认采用 **JSONL (JSON Lines) 追加写存储引擎**（[`JsonlSessionStore`](../src/storage/jsonl.rs)）。

### 2.1 文件路径与命名
- **根存储目录**：`~/.vva/sessions/`（或测试指定的独立目录）
- **文件命名规范**：`~/.vva/sessions/{session_id}.jsonl`

### 2.2 行级记录布局 (Line Layout)
每个 `.jsonl` 文件严格遵循以下两段式结构：

```
Line 1:      {"type":"header","data":{"id":"sess_xxx","title":"Refactor","workspace_root":"/path","created_at":"...","updated_at":"...","current_model":"openai/gpt-5-terra","current_variant":"high","meta":{}}}
Line 2:      {"type":"turn","data":{"id":"turn_001","role":"user","messages":[...],"usage":null,"meta":{}}}
Line 3:      {"type":"turn","data":{"id":"turn_002","role":"assistant","messages":[...],"usage":{...},"meta":{"response":{"response_id":"resp_123",...}}}}
Line 4..N:   {"type":"turn","data":{...}}
```

- **第 1 行 (`JsonlRecord::Header`)**：
  - 存储全局会话头信息 [`SessionHeader`](../src/model/session.rs)；
  - 绑定当前会话的 `current_model`、`current_variant` 与 `workspace_root`；
  - 仅在会话创建或发生模型/标题配置变更时覆写更新第 1 行。
- **第 2..N 行 (`JsonlRecord::Turn`)**：
  - 每完成一轮交互（用户提问、模型响应或工具执行返回），直接向文件尾部执行标准 `O_APPEND` 追加写；
  - 独立记录该轮的全部 `messages`、`usage` 及专属 `meta`。

---

## 3. SessionStore Trait 接口定义

代码位置：[`src/storage/mod.rs`](../src/storage/mod.rs)

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 保存或更新会话 Header 及全量轮次
    async fn save_header(&self, session: &Session) -> Result<(), StoreError>;

    /// 向指定会话尾部追加单轮 Turn（核心 Append-Only 写入路径）
    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError>;

    /// 读取并重构完整的 Session 实体（包含所有历史 Turns）
    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError>;

    /// 遍历列出当前存储目录下的所有会话元信息摘要（轻量级，无需完全反序列化消息体）
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError>;

    /// 删除指定会话文件
    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError>;
}
```

---

## 4. 容灾与零拷贝流式性能优势

1. **抗进程崩溃**：
   - 使用系统的 `OpenOptions::new().append(true)` 打开文件，单轮写入附带换行符并刷新缓冲区。即便进程在下一轮开始前被 kill 或崩溃，所有已完成轮次 100% 完整保留。
2. **轻量会话列表检索 (`list_sessions`)**：
   - 提取会话列表或在 TUI `/sessions` 弹窗展示时，**仅需读取各 `.jsonl` 文件的首行**（获取 `SessionHeader`）以及快速扫描行数（统计 `turn_count`），避免将长达数兆字节的完整多模态对话反序列化进内存。
3. **零数据兜底保证**：
   - 解析每行记录时使用严格的枚举分发（`JsonlRecord`），若检测到未闭合或损坏的行立即返回强类型 `StoreError`，严禁默默吞下错误或以空数据代替。
