# 代码地图

按目录给出职责，文件级细节以源码为准。各目录下的 `tests.rs`、`*_tests.rs` 与 `test_support.rs` 是单测 fixture，未列出。设计文档见 [README](README.md)。

```
Cargo.toml                 # workspace；default-members = vva + vva-core
build.rs                   # 仅应用：构建 frontend
src/                       # 应用 vva
├── lib.rs / main.rs       # 应用模块导出 / CLI 入口（run、session list|inspect|migrate、web、agent、version、update）
├── find.rs                # 工作区遍历与模糊匹配（TUI @ 补全与 Web go-to-file 共用）
├── discover.rs            # agents 与 skills 的默认目录优先级
├── system.rs              # 环境采集与系统提示组装（日期 / OS / arch / workspace / scratch）
├── prompts/system.md      # 系统提示模板
├── config/
│   ├── mod.rs             # 配置寻址与分层合并；DEFAULT_MODEL_SPEC
│   ├── model.rs           # AppConfig / ProviderConfig / ModelConfig / AgentSpaceConfig；glm-coding-plan 预设
│   ├── parser.rs          # JSONC 去注释与环境变量插值
│   ├── env.rs             # env 文件（dotenv 子集）
│   ├── resolver.rs        # resolve_model → ResolvedModel；create_provider 单独构造
│   └── state.rs           # ~/.vva/state.json 运行时偏好
├── agents/mod.rs          # 内置 general / explore 与默认目录；直接使用 core AgentDef / 解析器
├── skills/mod.rs          # 技能发现与斜杠调用；直接使用 core Skill / 解析器
├── app/                   # 应用会话 facade 与装配策略
│   ├── session.rs         # VvaSessionManager：模型偏好、配置重载、会话创建与子代理装配
│   ├── setup.rs           # VvaSessionSetup：工具选择、coding 上下文与能力通知
│   ├── records.rs         # SessionRecord：独立强类型应用记录
│   ├── store.rs           # AppSessionStore：应用记录与会话关联
│   ├── model.rs           # 应用会话配置
│   ├── view.rs            # 应用快照投影
│   ├── handle.rs          # 公开 handle 包装
│   ├── stats.rs           # TUI / Web 共用统计
│   ├── migration.rs       # 停写后 v2 → v3、新目录与清单
│   ├── mention.rs         # @ 文件引用解析与展开
│   └── web.rs             # 外置联网模型与凭据解析
├── space/                 # Agent Space（设计见 docs/agent.md）
│   ├── mod.rs             # 启动校验与装配、tool_factory
│   ├── runtime/           # 主循环 / 入站 / 命令执行 / 定时任务 / 模式模型
│   ├── router.rs          # L0 / L1 / L2 路由
│   ├── decisions.rs       # Decisions API 后端
│   ├── binding.rs         # BindingStore / MessageIndex / PendingFiles
│   ├── activity.rs        # 完成回执维护活动时间
│   ├── curation_tool.rs   # 整理标记工具
│   ├── lifecycle.rs       # TTL 清理
│   ├── curator.rs         # 记忆整理
│   ├── rollover.rs        # 上下文压缩续接
│   ├── scheduler.rs       # 定时任务表
│   ├── schedule_tool.rs   # Schedule 工具
│   ├── progress.rs        # 进度汇报
│   ├── send_file.rs       # SendFile 工具
│   ├── command.rs         # IM 斜杠命令解析与帮助
│   ├── markdown.rs        # 命令输出的 Markdown 排版
│   └── channel/           # Channel trait 与附件落盘；qq/ 官方 OpenAPI 与网关
├── ui/                    # Ratatui TUI（设计见 docs/ui.md）
│   ├── mod.rs · events.rs # 终端生命周期与主循环 / 输入事件分发
│   ├── command.rs         # 斜杠命令与补全、@ 文件候选
│   ├── input.rs           # 输入缓冲
│   ├── markdown.rs · text.rs · spans.rs · hyperlinks.rs
│   ├── app/               # App 状态：session / editor / dispatch / pane / draft / selection / scroll / chat_focus / input_queue / toast
│   ├── modals/            # ActiveModal 与各弹窗状态机（sessions / models / variants / config / skills / ask_user / message）
│   └── views/             # 纯渲染：chat/（pipeline / tool / diff / patch / thought / citations / user_block / selection / view）、各弹窗视图、header / sidebar / input_area / toast
└── web/                   # Axum 服务器
    ├── api.rs             # REST + SSE（会话 CRUD / 提交 / 中止 / 配置 / 事件流 / 模型清单）
    ├── sync.rs            # SSE 快照 / 增量投影（见 docs/web_sync.md）
    └── server.rs          # 路由与静态资源（rust_embed 内嵌 frontend/dist，SPA 回退）

frontend/                  # React + Vite + TypeScript；构建期由 build.rs 编译，产物经 rust_embed 打包；规范见 frontend/AGENTS.md
├── src/App.tsx            # 会话状态与 SSE 协调
├── src/api.ts · types.ts · sessionSync.ts · toolNames.ts
├── src/hooks/ · utils/ · styles/
└── src/components/        # Sidebar / Chat / Composer / Parts / trace / files / StatsPanel / 弹窗

crates/vva-core/
├── Cargo.toml             # 默认无 feature 的库
├── examples/headless.rs   # response + jsonl：自定义工具、一次投递与 shutdown
├── tests/support/         # 共享协议与公开 API 测试 fixture
└── src/
    ├── lib.rs
    ├── agents.rs          # AgentDef、YAML 解析与目录扫描
    ├── skills.rs          # Skill、SKILL.md 解析与目录扫描
    ├── discover.rs        # name / description 等通用字段校验
    ├── event.rs           # AgentEvent 统一事件流（Provider 流式增量 + Harness 工具 / Notice / Aborted）
    ├── model/
    │   ├── session.rs     # Session / SessionHeader / SessionSummary / EditorKind
    │   ├── turn.rs        # Turn / TokenUsage
    │   ├── message.rs     # Message
    │   ├── content.rs     # ContentPart / Annotation / MediaSource
    │   ├── meta.rs        # Response / Anthropic / Gemini 轮次元数据
    │   ├── tool.rs        # ToolCall / ToolResult / ToolDefinition / ToolInput（Json | Freeform + Lark）
    │   ├── background.rs  # 后台任务快照与状态
    │   └── shell.rs       # ShellOutput 展示输出
    ├── storage/           # SessionStore trait；jsonl.rs 为追加写实现
    ├── provider/          # 供应商适配
    │   ├── mod.rs         # Provider trait 与 ProviderCapabilities
    │   ├── input.rs       # ProviderType / InputCapabilities：执行类型与媒体校验
    │   ├── common.rs      # EventForwarder / require_* / RoleBlockMerger / normalize_endpoint
    │   ├── http.rs        # post_json / post_sse
    │   ├── sse.rs         # SSE 解码器
    │   ├── websocket.rs   # JSON WebSocket 传输与控制帧
    │   ├── response/      # OpenAI Responses（request / parse / stream / websocket / search / image）
    │   ├── anthropic/     # Anthropic Messages（request / parse / stream / standalone）
    │   └── gemini/        # Gemini（request / parse / stream）
    ├── tools/
    │   ├── mod.rs · names.rs   # Tool trait 与 ToolRegistry（按编辑协议组装、白名单裁剪）/ 工具名常量
    │   ├── filesystem/        # Read / Write / Edit / Glob / Grep
    │   ├── apply_patch.rs     # apply_patch（Codex V4A，freeform；文法 apply_patch.lark）
    │   ├── hashline/          # hashline_edit：snapshot / session / parse / apply / block
    │   ├── shell.rs           # Shell（前台超时 / 输出上限 / run_in_background）
    │   ├── shell_output.rs    # Shell 输出采集
    │   ├── background.rs      # TaskOutput / TaskStop 与后台任务注册表
    │   ├── agent.rs           # Agent 工具与 SessionBroker trait（派生 subagent）
    │   ├── session_ipc.rs     # SendMessage 跨会话消息
    │   ├── ask_user.rs        # AskUser
    │   ├── skill.rs           # Skill 工具（按 name 加载 SKILL.md）
    │   └── web/               # web_search / web_fetch 客户端工具与外置 provider（brave / deepseek / google / tavily / zhipu / local）
    └── harness/               # 调度引擎
        ├── assembly.rs        # AgentAssembly / SessionSetup / ExecutionPlan / FixedSetup
        └── session/           # manager 句柄表 · task 执行任务 · handle 公开句柄 · view 快照 · input 待发消息 · compaction 上下文压缩；Task / Command 私有，manager 提供 close / shutdown
```
