# 代码地图

按目录给出职责，文件级细节以源码为准。设计说明见 [README](README.md) 索引。

```
src/
├── lib.rs / main.rs        # 模块导出 / CLI 入口 (vva run, session list|inspect, web, agent, version, update)
├── event.rs                # AgentEvent 统一事件流 (Provider 流式增量 + Harness 工具/Notice/Aborted)
├── find.rs                 # 工作区遍历与模糊匹配 (walk_workspace / fuzzy_match；TUI @ 补全与 Web go-to-file 共用)
├── system.rs               # 系统环境采集与系统提示组装 (日期 / OS / arch / workspace / scratch)
├── discover.rs             # agents 与 skills 共用的目录优先级与 name 校验
├── prompts/system.md       # 系统提示模板
├── config/                 # 配置
│   ├── mod.rs              # load_config 寻址与分层合并 (含 FALLBACK_MODEL_SPEC)
│   ├── model.rs            # AppConfig / ProviderConfig / ModelConfig / AgentSpaceConfig；glm-coding-plan 预设补全
│   ├── parser.rs           # strip_jsonc_comments / interpolate_env_vars
│   ├── env.rs              # env 文件 (dotenv 子集)
│   ├── resolver.rs         # resolve_model → ResolvedModel；create_provider 单独构造
│   └── state.rs            # ~/.vva/state.json 运行时偏好
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
│   ├── common.rs           # EventForwarder / require_* / RoleBlockMerger / normalize_endpoint
│   ├── http.rs             # post_json / post_sse (SseEventHandler)
│   ├── sse.rs              # SSE 解码器
│   ├── response/           # OpenAI Responses (request / parse / stream / search / image)
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
├── agents/mod.rs           # Subagent 定义：AgentDef 解析与 discover_agents (内置 general / explore)
├── skills/mod.rs           # Agent Skills：SkillFrontmatter 解析与 discover_skills
├── harness/                # 调度引擎
│   ├── agent.rs            # AgentEngine 组装与 capability_notices 能力协商
│   ├── mention.rs          # @ 文件引用解析与展开
│   ├── testing.rs          # (cfg(test)) StubBroker
│   └── session/            # SessionManager 句柄表 · SessionTask 执行任务 · ManagedSession 句柄/Command · SessionView 快照
├── space/                  # 常驻 IM agent (设计见 docs/agent.md)
│   ├── mod.rs              # AgentSpace 启动校验与装配、tool_factory
│   ├── runtime/            # 主循环 / inbound / commands / schedule / mode_model
│   ├── router.rs · decisions.rs        # L0/L1/L2 路由；Decisions API 后端
│   ├── binding.rs                      # BindingStore / MessageIndex / PendingFiles
│   ├── lifecycle.rs · curator.rs · rollover.rs  # TTL 清理 / 知识沉淀 / 上下文压缩续接
│   ├── scheduler.rs · schedule_tool.rs # 定时任务表与 Schedule 工具
│   ├── progress.rs · send_file.rs · command.rs · markdown.rs
│   └── channel/            # Channel trait；files.rs 附件落盘；qq/ 官方 OpenAPI + 网关
├── ui/                     # Ratatui TUI (设计见 docs/ui.md)
│   ├── mod.rs · events.rs  # 终端生命周期与主循环 / 输入事件分发
│   ├── app/                # App 状态机：session / editor / dispatch / pane / draft / selection / scroll
│   ├── command.rs          # 斜杠命令与补全、@ 文件候选
│   ├── input.rs · markdown.rs · text.rs · spans.rs · hyperlinks.rs · history.rs
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
```
