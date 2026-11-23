# 存储

core 的 [`storage`](../crates/vva-core/src/storage/mod.rs) 定义通用 `SessionStore`，[`JsonlSessionStore`](../crates/vva-core/src/storage/jsonl.rs) 保存领域实体与供应商元数据。应用经 [`AppSessionStore`](../src/app/store.rs) 关联应用记录。两者没有共享的 JSON 扩展字段。

## 1. 路径

会话目录默认 `~/.vva/sessions`，由全局参数 `--sessions-dir DIR` 指定。应用记录目录固定为它的同级 `DIR-app`，默认 `~/.vva/sessions-app`。

| 对象 | 路径 |
|---|---|
| 会话文件 | `DIR/{project_key}/{session_id}.jsonl` |
| 应用记录 | `DIR-app/{session_id}.json` |
| 项目便签 | `DIR/{project_key}/scratch/` |
| 大体积工具输出 | `DIR/{project_key}/tool_outputs/` |
| 写入锁 | `DIR` 同级的 `.目录名.writer.lock` |

- `project_key` 由工作区绝对路径编码；`session_id` 为 `sess_` 加 5 位小写十六进制。
- 应用记录以同一个 SessionId 关联会话。
- 便签每项目一份，绝对路径传入 coding system prompt；工具的相对路径解析到工作区。大体积工具输出的绝对路径由应用准备后传给工具。
- 写入锁：运行中的进程共享持有到退出，迁移独占持有。

迁移来的会话在应用记录里保存资源路径，继续指向旧目录；历史媒体路径与工具输出引用都不重写。备份需同时包含 JSONL、应用记录及二者引用的资源目录。

## 2. 记录布局

```jsonl
{"type":"header","version":3,"id":"sess_a1b2c","workspace_root":"/path","created_at":"2026-11-10T00:00:00Z","current_model":"openai/gpt-5-terra"}
{"type":"turn","id":"turn_001","role":"user","messages":[{"parts":[{"type":"text","text":"hello"}]}],"meta":{}}
```

- **Header**：通用字段见[领域模型 §1.1](domain_model.md#11-session)。创建、换模、改名、续接标记变更时重写。
- **Turn**：完成的用户输入、模型响应、工具结果逐轮以 `O_APPEND` 追加，含 `messages`、`usage`、强类型 `meta` 与结构化后台回执。
- **应用记录**：[`SessionRecord`](../src/app/records.rs) 必含以下字段，Space 字段语义见 [agent §9](agent.md#9-状态文件)。
  - `id`
  - `coding`：`editor` 与 `web_search`
  - `application`：`coding`，或携带 `SpaceSessionRecord` 的 `space`
  - `resources`：scratch 与 tool_outputs 的绝对路径
- **写入边界**：Header 重写与应用记录写入各自经临时文件、`fsync`、`rename`。两个文件之间没有事务：一方写失败时返回错误，已写入的一方保留。应用记录在创建时写入，加载时只读。

加载时遇到记录缺失、身份不匹配或字段损坏，直接报错。

## 3. SessionStore

core 的必需契约只有五个会话操作，资源目录由宿主准备：

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

- 每轮追加后刷新缓冲，进程中断保留已完成的轮次。
- Header 重写失败时保留原文件。
- 列表与加载都校验版本和记录类型；列表解析轮次并计数。
- 未知版本、损坏行或结构不符的 Header 报错，该会话不加载。
- 运行时只接受 v3，遇到 v2 提示迁移命令。

## 5. v2 → v3 迁移

1. 停止所有使用源目录的 vva 进程（TUI、Web、Space），包括不支持写入锁的旧二进制。
2. 选一个源目录之外、尚不存在的新目录；它同级的 `-app` 目录也必须不存在。父目录必须存在。
3. 执行：

   ```sh
   vva session migrate --source ~/.vva/sessions --destination ~/.vva/sessions-v3 --stopped
   ```

   `--stopped` 声明旧版已停写。命令持有源、目标目录的写入锁，完整校验 v2 后在临时目录生成 v3 与应用记录，保留 SessionId、全部 turns、原生供应商 meta、父子与续接关系。遇到未知版本或损坏数据即中止，源目录不被修改。
4. 核对标准输出清单与 `NEW/migration-manifest.json`，其中逐会话列出源 / 目标文件、应用记录、轮次数与旧资源路径。记录目录与会话目录分两次激活；第二次失败时，错误给出已写入的应用目录。移除失败产物后，换一个未占用的目标重跑。
5. 用 `vva --sessions-dir ~/.vva/sessions-v3 run`（或 `web` / `agent`）切换。默认目录不变。

迁移后备份新的 JSONL 与 `sessions-v3-app`。旧目录的 scratch / tool_outputs 被迁移来的会话引用，整个旧目录保留。

回退时停止新运行时，用旧二进制和保留的 v2 源目录；迁移后在新目录产生的轮次只存在于新目录。
