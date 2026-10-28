# AGENTS.md - vva

**vva (Vendor-Versatile Agent Harness)**：保持统一 Session 抽象，同时支持各供应商原生特性的 Coding Harness。

## 1. 定位

通用 harness 的「最小公分母」（LCD）做法把模型退化为无状态的 chat completions，丢失 Responses 服务端 `response_id` 链、Anthropic `cache_control` 与 Thinking `signature`、Gemini 原生 Grounding 引用，且切换供应商时上下文割裂。vva 为每家实现原生协议适配，并在统一领域模型上存储。背景与分层见 [docs/architecture.md](docs/architecture.md)。

## 2. 设计约束

### 2.1 统一存储与零兜底

统一领域存储，不降级、不兜底。

1. **统一领域存储**：`Session → Turn → Message → ContentPart` 四层实体与强类型元数据落盘（JSONL），与厂商报文解耦。
2. **适配器零兜底**：Provider 仅实现厂商官方原生协议。
   - 禁止协议退化：Responses 适配器不检查 Chat Completions 的 `choices`，不伪造或猜测数据；
   - 禁止非标字段别名猜测：不写 `"output_text" | "text"`、`"web_search_call" | "web_search"`、`.or_else(|| json["reasoning_effort"])` 之类的猜测链，官方未定义的字段不臆造。
3. **UI / Harness 零兜底**：UI 由标准事件流（`AgentEvent`）驱动渲染，不写从会话历史覆盖聊天记录的同步补丁；界面未展示时排查适配器转换。
4. **Agent Space 路由**：判断不明确时续接当前 focus；Decisions 低置信度由 Router 执行该规则，focus 运行中按插话投递，无 focus 时显式提示；调用失败与非法报文仍显式报错。
5. **Mock 真实性**：单测 Mock 报文对照官方文档构造，不为错误测试数据在生产代码中加容错分支。
6. **例外：SKILL.md frontmatter**：生态技能包常携带规范外字段（如 `version`）或非字符串 `metadata` 值，解析时告警后忽略，不跳过技能加载；其余规范字段校验仍严格。

### 2.2 强类型供应商元数据

用强类型结构体，不用松散的 `HashMap<String, serde_json::Value>`。`ResponseTurnMeta` / `AnthropicTurnMeta` / `GeminiTurnMeta` 定义见 [docs/domain_model.md §3](docs/domain_model.md#3-强类型供应商元数据)，源码 `src/model/meta.rs`。

## 3. 文档

| 文档 | 内容 |
|---|---|
| [docs/README.md](docs/README.md) | 文档索引 |
| [docs/architecture.md](docs/architecture.md) | LCD 问题、分层、数据流 |
| [docs/domain_model.md](docs/domain_model.md) | 实体、ContentPart、引用注脚、强类型元数据 |
| [docs/storage.md](docs/storage.md) | JSONL 布局、SessionStore、容灾 |
| [docs/providers.md](docs/providers.md) | Responses / Anthropic / Gemini 适配、freeform 工具、结构化输出、SSE |
| [docs/config-reference.md](docs/config-reference.md) | 配置字段、加载与合并、模型寻址、env 文件 |
| [docs/ui.md](docs/ui.md) | TUI 架构 |
| [docs/web_sync.md](docs/web_sync.md) | Web SSE 快照 / 增量协议 |
| [docs/background_tasks.md](docs/background_tasks.md) | 后台任务展示、持久化与自动唤醒 |
| [docs/agent.md](docs/agent.md) | Agent Space：Router、生命周期、curator、rollover、Scheduler |
| [docs/code_map.md](docs/code_map.md) | 目录结构与模块职责 |
| [docs/usage.md](docs/usage.md) | 用户使用指南 |
| [frontend/AGENTS.md](frontend/AGENTS.md) | Web 前端设计规范与开发契约 |

文档约定：每个事实只在一处记录，其余处链接；设计文档描述现状，不记录计划与进度。

## 4. 开发指南

1. **新增供应商**：
   - `src/model/meta.rs` 定义强类型元数据并接入 `TurnMeta`；
   - `src/config/model.rs`、`src/config/resolver.rs` 注册 ProviderType 路由；
   - `src/provider/` 实现 `Provider` trait，`capabilities()` 声明原生能力；
   - 单测验证 cache、signature、response_id、reasoning_effort 等专属特性的双向转换保真。
2. **多模态**：图片 / 音视频用 `MediaSource::Path` 或 `MediaSource::Base64`，适配器转为目标 API 的 Payload（Gemini `inline_data`、Anthropic `base64` image source 等）。
3. **测试**：改动核心模型、解析器、适配器或 UI 状态后运行 `cargo test`，保持 `cargo clippy -- -D warnings` 零告警。
4. **发布**：推送 `v*` tag 触发 `.github/workflows/release.yml`。打 tag 前必须更新 `CHANGELOG.md`，添加与 tag 版本一致的非空 `## [版本号]` 条目，并将 `Cargo.toml` 的 `version`（含 `Cargo.lock`）同步为该版本，一并提交。
