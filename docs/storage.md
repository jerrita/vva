# 存储

[`vva-core::storage`](../crates/vva-core/src/storage/mod.rs) 定义通用 `SessionStore`，[`JsonlSessionStore`](../crates/vva-core/src/storage/jsonl.rs) 保存领域实体和原生供应商元数据。应用通过 [`AppSessionStore`](../src/app/store.rs) 关联独立记录；两者不共享任意 JSON 扩展字段。

## 1. 路径

默认会话目录为 `~/.vva/sessions`，可通过全局 `--sessions-dir DIR` 明确指定；对应应用目录固定为其同级的 `DIR-app`（默认 `~/.vva/sessions-app`）。

| 对象 | 路径 |
|---|---|
| 会话文件 | `DIR/{project_key}/{session_id}.jsonl`，`project_key` 由工作区绝对路径编码，`session_id` 为 `sess_` + 5 位小写十六进制 |
| 应用记录 | `DIR-app/{session_id}.json`，以同一个 SessionId 关联 |
| 新会话 Scratch Pad | `DIR/{project_key}/scratch/`，每项目一份，绝对路径传入 coding system prompt；相对工具路径仍解析到工作区 |
| 新会话大工具输出 | `DIR/{project_key}/tool_outputs/`，绝对路径由应用准备并传入工具 |
| 写入锁 | `DIR` 同级的 `.目录名.writer.lock`，运行进程共享持有到退出，迁移独占持有 |

迁移会话的资源路径记录在应用记录中，继续指向旧目录。历史媒体路径、工具输出引用不重写；备份必须同时包含 JSONL、应用记录及其引用的资源目录。

## 2. 记录布局

```jsonl
{"type":"header","version":3,"id":"sess_a1b2c","workspace_root":"/path","created_at":"2026-11-10T00:00:00Z","current_model":"openai/gpt-5-terra"}
{"type":"turn","id":"turn_001","role":"user","messages":[{"parts":[{"type":"text","text":"hello"}]}],"meta":{}}
```

- **Header**：通用字段见 [领域模型](domain_model.md#11-session会话根实体)。创建、换模、改名、续接标记变更时重写；不含 Space、编辑协议或 web 开关。
- **Turn**：完成的用户输入、模型响应、工具结果逐轮 `O_APPEND` 追加，保留 `messages`、`usage`、强类型 `meta` 与结构化后台回执。
- **应用记录**：[`SessionRecord`](../src/app/records.rs) 必须包含 `id`、`coding`（`editor` / `web_search`）、显式 `application`（`coding` 或携带 `SpaceSessionRecord` 的 `space`）、`resources`（scratch / tool_outputs 绝对路径）。Space 字段语义见 [agent](agent.md#9-状态文件)。缺失记录、身份不匹配、字段损坏直接报错，不视作普通会话、不加载时补建。
- **写入边界**：JSONL header 重写和应用记录写入各自使用临时文件、`fsync`、`rename`。跨文件不构成事务；其中一方写入失败明确返回错误，保留已成功写入的数据供检查。创建时明确建立应用记录，加载只读取关联记录。

## 3. SessionStore

core 的必需契约仅含五个会话操作，资源目录由宿主准备：

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn save_header(&self, session: &Session) -> Result<(), StoreError>;
    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError>;
    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError>;
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError>;
    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError>;
}
```

## 4. 容灾与读取

每轮追加并刷新缓冲，进程中断保留已完成轮次。header 重写失败保留原文件。列表与加载均校验版本和记录类型，列表解析轮次并计数；未知版本、损坏行或不符合当前结构的 header 立即报错，不跳过会话。运行时只接受 v3；遇 v2 提示显式迁移命令，不进行隐式转换。

## 5. v2 → v3 迁移

1. 停止所有使用源目录的 vva（TUI、Web、Space），包括不支持写入锁的旧二进制；迁移期间保持停写。
2. 选择源目录之外、尚不存在的新目录；其同级 `-app` 目录也必须不存在。父目录必须存在。
3. 执行：

   ```sh
   vva session migrate --source ~/.vva/sessions --destination ~/.vva/sessions-v3 --stopped
   ```

   `--stopped` 确认调用者已停止旧版写入。命令还持有源、目标目录的写入锁。它完整校验 v2 后在临时目录生成 v3 和应用记录，保留 SessionId、全部 turns、原生 provider meta、父子及续接关系；未知版本或损坏数据中止，不修改源目录。
4. 核对标准输出清单和 `NEW/migration-manifest.json`：逐会话包含源/目标文件、应用记录、轮次数及旧资源路径。记录目录与会话目录分两次激活；若第二次失败，错误会指出已写入的应用目录，不宣称跨目录原子提交。检查并移除失败产物后重新迁移到未占用的目标。
5. 使用 `vva --sessions-dir ~/.vva/sessions-v3 run`（或 `web` / `agent`）切换。不会自动改变默认目录。备份新 JSONL 和 `sessions-v3-app`，保留旧 scratch/tool_outputs；不可仅搬 JSONL 后删除整个旧目录。

回退时停止新运行时，使用旧二进制和保留的 v2 源目录；新目录中的后续轮次不会自动回写到 v2。
