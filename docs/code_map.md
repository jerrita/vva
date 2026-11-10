# 代码地图

按目录给出职责，文件级细节以源码为准。设计说明见 [README](README.md) 索引。

```
Cargo.toml                 # workspace；default-members = vva + vva-core
build.rs                   # 仅应用：frontend 构建
src/                       # vva 应用
├── lib.rs / main.rs        # 模块导出 / CLI 入口 (vva run, session list|inspect|migrate, web, agent, version, update)
├── find.rs                 # 工作区遍历与模糊匹配 (walk_workspace / fuzzy_match；TUI @ 补全与 Web go-to-file 共用)
├── app/                    # 应用会话 facade 与装配策略
│   ├── session.rs          # VvaSessionManager：模型偏好、配置重载、会话创建与子代理装配
│   ├── setup.rs            # VvaSessionSetup：工具选择、coding 上下文与能力通知
│   ├── records.rs · store.rs # 独立强类型应用记录与会话关联
│   ├── model.rs · view.rs · handle.rs # 应用会话/快照投影与公开 handle 包装
│   ├── stats.rs            # TUI/Web 共用统计（含原有测试）
│   ├── migration.rs        # 停写后 v2→v3、新目录与清单
│   ├── mention.rs          # @ 文件引用解析与展开
│   └── web.rs              # 外置联网模型与凭据解析
├── system.rs               # 系统环境采集与系统提示组装 (日期 / OS / arch / workspace / scratch)
├── discover.rs             # 应用默认的全局与项目发现目录
├── prompts/system.md       # 系统提示模板
├── config/                 # 配置
│   ├── mod.rs              # load_config 寻址与分层合并 (含 FALLBACK_MODEL_SPEC)
│   ├── model.rs            # AppConfig / ProviderConfig / ModelConfig / AgentSpaceConfig；glm-coding-plan 预设补全
│   ├── parser.rs           # strip_jsonc_comments / interpolate_env_vars
│   ├── env.rs              # env 文件 (dotenv 子集)
│   ├── resolver.rs         # resolve_model → ResolvedModel；create_provider 单独构造
│   └── state.rs            # ~/.vva/state.json 运行时偏好
├── agents/mod.rs           # 默认目录优先级与内置 general / explore；re-export core AgentDef / 解析器
├── skills/mod.rs           # 默认目录优先级与斜杠调用；re-export core Skill / 解析器
├── space/                  # 常驻 IM agent (设计见 docs/agent.md)
│   ├── mod.rs              # AgentSpace 启动校验与装配、tool_factory
│   ├── runtime/            # 主循环 / inbound / commands / schedule / mode_model
│   ├── router.rs · decisions.rs        # L0/L1/L2 路由；Decisions API 后端
│   ├── binding.rs                      # BindingStore / MessageIndex / PendingFiles
│   ├── activity.rs · curation_tool.rs # 完成回执维护活动时间 / 沉淀标记工具
│   ├── lifecycle.rs · curator.rs · rollover.rs  # TTL 清理 / 知识沉淀 / 上下文压缩续接
│   ├── scheduler.rs · schedule_tool.rs # 定时任务表与 Schedule 工具
│   ├── progress.rs · send_file.rs · command.rs · markdown.rs
│   └── channel/            # Channel trait；files.rs 附件落盘；qq/ 官方 OpenAPI + 网关
├── ui/                     # Ratatui TUI (设计见 docs/ui.md)
│   ├── mod.rs · events.rs  # 终端生命周期与主循环 / 输入事件分发
│   ├── app/                # App 状态机：session / editor / dispatch / pane / draft / selection / scroll
│   ├── command.rs          # 斜杠命令与补全、@ 文件候选
│   ├── input.rs · markdown.rs · text.rs · spans.rs · hyperlinks.rs
│   ├── modals/             # ActiveModal 与各弹窗状态机 (sessions / models / variants / config / skills / ask_user / message)
│   └── views/              # 纯渲染：chat/ (pipeline / tool / diff / patch / thought / citations / user_block / selection / view)、各弹窗视图、header / sidebar / input_area
└── web/                    # Axum 服务器
    ├── api.rs              # REST + SSE (会话 CRUD / 提交 / 中止 / 配置 / 事件流 / 模型清单)
    ├── sync.rs             # SSE 快照 / 增量投影 (见 docs/web_sync.md)
    └── server.rs           # 路由与静态资源 (rust_embed 内嵌 frontend/dist，SPA 回退)

frontend/                   # React + Vite + TypeScript；build.rs 构建期编译，产物由 rust_embed 打包；规范见 frontend/AGENTS.md
├── src/App.tsx             # 会话状态与 SSE 协调核心
├── src/api.ts · types.ts · sessionSync.ts · toolNames.ts
├── src/hooks/ · utils/ · styles/
└── src/components/         # Sidebar / Chat / Composer / Parts / trace / files / StatsPanel / modals ...
crates/vva-core/
├── Cargo.toml             # 默认无 feature 的库
├── tests/support/         # 随代码迁移的共享协议与运行时测试 fixture
└── src/
    ├── lib.rs
    ├── agents.rs          # AgentDef、YAML 解析与显式目录扫描
    ├── skills.rs          # Skill、SKILL.md 解析与显式目录扫描
    ├── discover.rs        # 通用字段校验
    ├── event.rs                # AgentEvent 统一事件流 (Provider 流式增量 + Harness 工具/Notice/Aborted)
    ├── model/                  # 领域模型
    │   ├── session.rs          # Session / SessionHeader / JsonlRecord
    │   ├── turn.rs             # Turn / TokenUsage
    │   ├── message.rs          # Message
    │   ├── content.rs          # ContentPart / Annotation / MediaSource
    │   ├── meta.rs             # Response / Anthropic / Gemini 轮次元数据
    │   ├── tool.rs             # ToolCall / ToolResult / ToolDefinition / ToolInput (Json | Freeform + Lark)
    │   └── background.rs       # 后台任务快照与状态
    ├── storage/                # SessionStore trait；jsonl.rs 为追加写实现
    ├── provider/               # 供应商适配 (Provider trait、capabilities、共享 HTTP Client)
    │   ├── input.rs            # ProviderType / InputCapabilities 执行类型与媒体校验
    │   ├── common.rs           # EventForwarder / require_* / RoleBlockMerger / normalize_endpoint
    │   ├── http.rs             # post_json / post_sse (SseEventHandler)
    │   ├── sse.rs              # SSE 解码器
    │   ├── websocket.rs        # JSON WebSocket 传输与控制帧处理
    │   ├── response/           # OpenAI Responses (request / parse / stream / websocket / search / image)
    │   ├── anthropic/          # Anthropic Messages (request / parse / stream)
    │   └── gemini/             # Gemini (request / parse 累积器 / stream)
    ├── tools/                  # 工具集
    │   ├── mod.rs / names.rs   # Tool trait 与 ToolRegistry (按编辑协议组装、白名单裁剪) / 工具名常量
    │   ├── filesystem/         # Read / Write / Edit / Glob / Grep 与共享基建
    │   ├── apply_patch.rs      # apply_patch (Codex V4A, freeform；文法 apply_patch.lark)
    │   ├── hashline/           # hashline_edit：snapshot / session / parse / apply / block
    │   ├── shell.rs            # Shell (前台超时 / 输出上限 / run_in_background)
    │   ├── background.rs       # TaskOutput / TaskStop 与后台任务注册表
    │   ├── agent.rs            # Agent 工具与 SessionBroker trait (派生 subagent)
    │   ├── session_ipc.rs      # SendMessage 跨会话消息
    │   ├── ask_user.rs         # AskUser
    │   ├── skill.rs            # Skill 工具 (按 name 加载 SKILL.md)
    │   └── web/                # web_search / web_fetch 外置 provider (brave / deepseek / google / tavily / zhipu / local)
    └── harness/                # 调度引擎
        ├── assembly.rs         # AgentAssembly / SessionSetup / ExecutionPlan / FixedSetup
        └── session/            # SessionManager 句柄表 · SessionTask 执行任务 · ManagedSession 句柄/Command · SessionView 快照
```
