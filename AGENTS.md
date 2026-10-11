# AGENTS.md - vva 项目核心架构与开发指南

本文档记录 **vva (Vendor-Versatile Agent Harness)** 的核心设计原则、设计文档索引与开发指南，作为 Agent 与开发者协作规范。

---

## 1. 项目定位与核心原则

### 痛点与目标
通用 coding harness 存在“最小公分母”（Lowest Common Denominator, LCD）问题：
- 将模型退化为无状态的 `/v1/chat/completions`；
- 丢弃 OpenAI Responses API 服务端链式 `response_id`，增加重复上下文上传；
- 丢弃 Anthropic Prompt Caching (`cache_control`) 与 Thinking 签名 (`signature`)；
- 将 Gemini 原生 Google Search Grounding 降级为外部搜索，丢失网页引用（Citation token offset）；
- 切换供应商时上下文格式割裂。

**vva 的目标**：在保持统一 Session 抽象的同时，支持各供应商原生特性的 Coding Harness。

---

## 2. 核心架构与设计约束

### 2.1 统一存储与“零兜底”约束 (Unified Storage & Zero Fallback Rule)
> **核心原则**：统一领域存储，不进行兜底降级。各供应商实现原生协议适配转换与专属强类型 Meta 存储。

1. **统一领域存储 (Unified Domain Storage)**：
   - 遵循四层级联实体结构：$$\text{Session} \longrightarrow \text{Turn} \longrightarrow \text{Message} \longrightarrow \text{ContentPart}$$
   - 底层持久化（JSONL）保存领域模型实体与强类型元数据，与厂商私有请求/响应报文解耦。
2. **零兜底与零降级约束 (Zero Fallback & Zero LCD Degradation)**：
   - **适配器层零兜底**：Provider 仅实现厂商官方原生协议规范。
     - **禁止协议退化**：Responses API 适配器不检查 Chat Completions 的 `choices` 字段，不进行数据伪造与猜测；
     - **禁止非标字段别名猜测**：不编写 `"output_text" | "text"`、`"web_search_call" | "web_search"` 或 `.or_else(|| json["reasoning_effort"])` 等猜测链。厂商官方规范未定义的字段，不在代码中臆造；
   - **UI / Harness 调度层零兜底**：UI 由标准事件流（`AgentEvent`）驱动渲染，不编写从会话历史覆盖聊天记录的同步补丁。界面未展示时排查适配器转换逻辑；
   - **单测 Mock 真实性约束**：单元测试 Mock 报文对照官方标准文档构造，不在生产代码中为错误测试数据添加容错分支。
   - **例外：SKILL.md frontmatter 非规范内容**：生态技能包常携带规范外字段（如 `version`）或非字符串 `metadata` 值，解析时告警后忽略，不跳过技能加载；其余规范字段的校验仍严格执行。

### 2.2 强类型供应商元数据 (Strict Typed Meta)
> **类型系统约束**：使用强类型结构体，不使用松散的 `HashMap<String, serde_json::Value>`。

为核心供应商定义专属强类型元数据（见 `src/model/meta.rs`）：
- **OpenAI Responses API**：`ResponseTurnMeta`（`response_id` 链式绑定与 `ResponseOutputItem` 原生条目回传）
- **Anthropic Messages API**：`AnthropicTurnMeta`
- **Google Gemini API**：`GeminiTurnMeta`

---

## 3. 设计文档索引 (Docs Reference)

技术设计方案参见 `docs/` 目录：

| 模块 | 设计文档 | 核心内容摘要 |
|---|---|---|
| **架构全局** | [docs/architecture.md](docs/architecture.md) | 痛点分析、LCD 陷阱防范、整体数据流与分层调用拓扑 |
| **领域模型** | [docs/domain_model.md](docs/domain_model.md) | 四层级联实体架构、多模态一等公民、引用注脚系统与强类型元数据 |
| **持久化存储** | [docs/storage.md](docs/storage.md) | JSONL 追加写存储引擎、`Header` 与 `Turn` 记录规范、抗崩溃容灾 |
| **供应商适配** | [docs/providers.md](docs/providers.md) | Responses 链式继承、Anthropic 缓存与签名、Gemini 搜索注脚与通用 SSE 解码 |
| **配置路由** | [docs/configuration.md](docs/configuration.md) | JSON/JSONC 词法注释剥离、`${VAR}` 环境变量展开、模型规格寻址与端点规范化 |
| **终端交互** | [docs/ui.md](docs/ui.md) | Ratatui TUI 架构、会话任务与 `SessionView` 订阅、`InputBuffer` 多行编辑与模态框状态解耦 |
| **Agent Space** | [docs/agent.md](docs/agent.md) | 常驻 IM agent：分层小模型 Router、TTL 清理与 curator 沉淀、rollover 压缩、Scheduler、cache 稳定性纪律 |

---

## 4. 最新代码目录结构

```
src/
├── lib.rs                  # 核心模块导出
├── event.rs                # AgentEvent 统一事件流词汇表 (Provider 流式增量 + Harness 工具/Notice/Aborted 事件)
├── find.rs                 # 工作区文件遍历与模糊匹配打分 (walk_workspace / fuzzy_match；TUI @ 补全与 Web go-to-file 共用)
├── system.rs               # 系统环境信息采集与系统提示组装 (日期 / OS / arch / workspace / scratch)
├── main.rs                 # CLI 命令行入口 (vva run [prompt], vva session list/inspect, vva web)
├── config/                 # 配置管理 (JSON/JSONC 解析、环境展开与路由)
│   ├── mod.rs              # load_config 自动寻址与分层合并 (含 FALLBACK_MODEL_SPEC)
│   ├── model.rs            # AppConfig, ProviderConfig (provider_type 为 Option，合并时仅显式声明才覆写), ModelConfig；glm-coding-plan 预设补全
│   ├── parser.rs           # strip_jsonc_comments 与 interpolate_env_vars
│   ├── resolver.rs         # resolve_model 只做配置解析并返回 ResolvedModel；Provider 由 ResolvedModel::create_provider 单独构造
│   └── state.rs            # 运行时偏好 (~/.vva/state.json)：上次选用的模型与推理档位
├── model/                  # 领域模型与强类型体系
│   ├── mod.rs              # 模块导出与通用类型
│   ├── meta.rs             # Response, Anthropic, Gemini 专属强类型轮次元数据 (含 ResponseOutputItem 原生条目链)
│   ├── content.rs          # 多模态 (Image)、搜索与 Annotation
│   ├── message.rs          # Role 与 Message (内容分组值对象，仅持有 parts)
│   ├── turn.rs             # Turn 与 TokenUsage
│   ├── session.rs          # Session 与 JSONL 记录定义 (SessionHeader / JsonlRecord / subagent parent·agent·depth 标记)
│   └── tool.rs             # ToolCall, ToolResult, ToolDefinition
├── storage/                # 存储抽象与实现
│   ├── mod.rs              # SessionStore Trait
│   └── jsonl.rs            # JsonlSessionStore 按项目目录存储 (sessions/{project_key}/{sess_xxxxx}.jsonl + scratch/)
├── provider/               # 供应商适配器层 (原生协议直通)
│   ├── mod.rs              # Provider Trait (capabilities/complete/complete_stream)、能力声明与共享 HTTP Client (超时)
│   ├── common.rs           # 跨供应商共享辅助 (EventForwarder 背压转发 / require_* 严格读取 / RoleBlockMerger / normalize_endpoint / header_value)
│   ├── http.rs             # 共享传输封装 (post_json 非流式 / post_sse 流式事件循环 SseEventHandler)
│   ├── response/           # OpenAI Responses API (previous_response_id + 原生搜索)
│   │   ├── mod.rs          # ResponseProvider 与 Provider Trait 实现
│   │   ├── request.rs      # 请求体构建 (previous_response_id 增量切片 / 原生条目回传 / 媒体)
│   │   ├── parse.rs        # 响应解析 (output items 反序列化 → 领域投影 + TurnMeta)
│   │   ├── stream.rs       # SSE 事件累积处理器 (delta / output_item.done / response.completed)
│   │   └── search.rs       # web_search_call → 领域 WebSearchCall 共享投影
│   ├── anthropic/          # Anthropic Messages API (Prompt Caching + Thinking 签名，流式/非流式统一解析)
│   │   ├── mod.rs          # AnthropicProvider 与 Provider Trait 实现
│   │   ├── request.rs      # 请求体构建 (cache_control 断点 / thinking 签名回显 / 角色合并)
│   │   ├── parse.rs        # 响应 DTO 严格解析与 assemble_response 统一组装
│   │   └── stream.rs       # SSE 流式事件累积处理器 (content blocks / usage / stop_reason)
│   ├── gemini/             # Google Gemini API (Grounding 多 chunk 合并投影 + 代码沙盒 + 共享累积器)
│   │   ├── mod.rs          # GeminiProvider 与 Provider Trait 实现 (端点/鉴权头)
│   │   ├── request.rs      # 请求体构建 (contents 角色合并 / 媒体 inlineData / tools)
│   │   ├── parse.rs        # GeminiAccumulator：整包/流式 chunk 统一合并 (grounding / parts / usage)
│   │   └── stream.rs       # SseEventHandler 实现 (合并后按背压转发增量事件)
│   └── sse.rs              # 通用标准 Server-Sent Events (SSE) 流式事件解码器 (仅 data: 行)
├── agent_space/            # 常驻 IM agent (`vva agent`，设计见 docs/agent.md)
│   ├── mod.rs              # AgentSpace：启动期配置校验与 run (装配 channel / runtime / scheduler / lifecycle，ctrl-c 退出)、agent_toggles、tool_factory (SendFile / Schedule 按会话 origin 运行期注入，经 SessionManager::with_tool_factory)
│   ├── runtime/            # Runtime：入站消息 → 命令 / 文件缓冲 → 路由 → session 轮次 → 出站文本
│   │   ├── mod.rs          # 主循环、轮次执行、出站与推送 (rollover 切换、记忆更新广播 ManagedSession::note)
│   │   ├── inbound.rs      # 入站、路由与建会话
│   │   ├── commands.rs     # 斜杠命令执行与 /status 统计表
│   │   ├── schedule.rs     # 定时任务执行、多聊天通知与错过提示
│   │   └── mode_model.rs   # agent 模式模型 (model.json) 读取、校验与落盘
│   ├── command.rs          # 斜杠命令解析
│   ├── binding.rs          # BindingStore (focus) / MessageIndex (消息 id → session) / PendingFiles (待附件缓冲)
│   ├── decisions.rs        # OpenRouter Decisions API (alpha) 路由后端：choice/noul 题目映射、置信度门槛，实现 RouteModel
│   ├── progress.rs         # 长轮次进度汇报：按固定间隔对新增轮次做确定性摘要 (文件 / 命令意图 / 文字) 并推送，with_progress 包裹投递
│   ├── router.rs           # Router：L0 reply 锚点 / L1 小模型结构化决策 / L2 显式报错 + router.jsonl
│   ├── scheduler.rs        # ScheduleStore：schedule.json 任务表 (cron / at / every，多 bindings) + schedule_state.json 状态，热重载、失败计数、错过检测 (不补跑)
│   ├── schedule_tool.rs    # Schedule 工具 (create/list/pause/resume/delete)
│   ├── lifecycle.rs        # 空闲 TTL → curator 沉淀 → 删除
│   ├── curator.rs          # 知识沉淀 (kind=curator 会话，游标增量，memory/ git 提交；memory_reminder 供会话注入记忆索引快照)
│   ├── send_file.rs        # SendFile 工具：经会话所在渠道发送本地文件
│   ├── rollover.rs         # 上下文达预留线时同会话压缩并切换新会话（接替会话重生成当前记忆索引快照）；context_tokens / cache_hit_rate
│   └── channel/            # Channel trait 与 Inbound/Outbound；files.rs 附件落盘辅助；qq/ 官方 OpenAPI + 网关
├── agents/                 # Subagent 定义：YAML agent 类型解析与发现
│   └── mod.rs              # AgentDef 强类型解析与校验、discover_agents (内置 general/explore + 全局 ~/.vva/agents、~/.agents/agents + 项目 .vva/agents、.agents/agents，同名后者覆盖前者)
├── skills/                 # Agent Skills (agentskills.io 规范：SKILL.md 技能包解析与发现)
│   └── mod.rs              # SkillFrontmatter 强类型解析与校验、discover_skills (全局 ~/.vva/skills、~/.agents/skills + 项目 .vva/skills、.agents/skills，优先级 项目 .vva > 项目 .agents > 全局 .vva > 全局 .agents，同名后者覆盖前者)
├── tools/                  # Coding 工具集
│   ├── mod.rs              # Tool Trait 与 ToolRegistry (register/retain 白名单裁剪)
│   ├── agent.rs            # Agent 工具与 SessionBroker trait：派生 subagent 会话 (前台阻塞/后台回投) + SubagentReport/完成通知格式化
│   ├── session_ipc.rs      # SendMessage / WaitSession 跨会话消息工具 (空闲触发轮次、忙时入队)
│   ├── shell.rs            # Shell 执行 (前台超时/输出上限/退出码检查 + run_in_background 后台执行)
│   ├── background.rs       # 后台任务注册表 (TaskOutput / TaskStop 工具，TaskStop 兼停 sess_ 子会话，内存态、随会话结束回收)
│   ├── filesystem/         # Read、Write、Edit、Glob、Grep 与共享基建 (路径解析/目录遍历/brace 展开/unified diff)
│   │   ├── mod.rs          # 共享辅助与 parse_args 解析、各工具 re-export
│   │   ├── read.rs         # Read (分页/媒体嗅探/hashline 快照/内容版本号)
│   │   ├── write.rs        # Write (独占创建)
│   │   ├── edit.rs         # Edit 精确替换引擎 (CRLF 规范化/冲突检测/行尾保持/原子落盘) + edit_lock/EditPlan 共享基建
│   │   ├── glob.rs         # Glob (mtime 排序/上限截断)
│   │   └── grep.rs         # Grep (include 过滤/hashline 输出)
│   ├── apply_patch.rs      # Edit（patch）：Codex V4A
│   ├── skill.rs            # Skill 工具：按 name 加载 SKILL.md 全文指令 (渐进披露第二层)
│   ├── web/                # 外置 web search / fetch provider 与同名客户端工具 (settings.web 配置驱动)
│   │   ├── mod.rs          # WebSearch/WebFetchProvider Trait、ExternalWeb 解析 (能力校验与 api_key 校验)
│   │   ├── brave.rs        # Brave Search LLM Context (grounding.generic 按 url 聚合，仅 search)
│   │   ├── google.rs       # Google：以已配置 Gemini 模型仅挂载原生 google_search / url_context (search + fetch)
│   │   ├── tavily.rs       # Tavily /search 与 /extract (search + fetch)
│   │   ├── local.rs        # 内置 fetch：HTTP 抓取 + dom_smoothie 正文提取 + htmd 转 Markdown
│   │   └── tool.rs         # web_search / web_fetch 工具 (原生能力缺位时注册)
│   └── hashline/           # Edit（hashline）：[PATH#TAG] 快照 + PUT/CUT/REM/MV 行号补丁
│       ├── mod.rs          # HashlineEditTool (Tool Trait 实现) 与模块导出
│       ├── session.rs      # HashlineSession (文件指纹 tag 与已展示行号集合登记)
│       ├── snapshot.rs     # 快照渲染 (format_read/format_grep_file/编辑结果报告)
│       ├── apply.rs        # 补丁应用 (分组预检/寄存器/块区间解析/落盘提交)
│       ├── parse.rs        # 补丁文法解析 (Section/FileOp/Op)
│       └── block.rs        # `N*` 块操作启发式解析
├── harness/                # 调度引擎与会话运行时
│   ├── mod.rs
│   ├── agent.rs            # AgentEngine 组装 (Provider + ToolRegistry + CompletionOptions + EngineDeps 依赖注入) 与 capability_notices 能力协商；subagent 会话按 agent 定义裁剪工具
│   ├── mention.rs           # @ 文件引用：提示词 token 解析 (CJK 边界/收尾标点) 与工作区文件读取展开 (文本→FileRef，图片/PDF→原生附件，二进制/超限/能力缺失降级文本注记)
│   ├── testing.rs          # (cfg(test)) 共享 StubBroker 桩
│   └── session/            # SessionManager 句柄表 + SessionTask 会话任务 (单一所有者顺序执行提交/配置/取消，按 revision 节流发布 SessionView 快照)
│       ├── mod.rs          # 模块导出 (SessionManager/ManagedSession/SessionView/RunState)
│       ├── view.rs         # SessionView 只读快照与 RunState
│       ├── handle.rs       # ManagedSession RPC 句柄与 Command 协议 (Submit/Deliver/Note/SubagentFinished 等；note 只追加不起轮次) 与 turn_seq 轮次计数
│       ├── manager.rs      # SessionManager 句柄表/生命周期/配置热替换/with_tool_factory 会话工具工厂/with_preamble 首次落盘 reminder 前导 + SessionBrokerHandle (spawn_subagent/deliver/wait_turn 子代理编排)
│       └── task.rs         # SessionTask 执行任务 (事件流驱动草稿/agent 循环/AskUser/中止时合成未决工具结果/后台任务与子代理完成注入续跑/Deliver 邮箱冲刷)
├── ui/                     # 现代化 Ratatui 交互式终端用户界面
    ├── mod.rs              # run_tui 终端生命周期、主循环与单帧 draw 布局
    ├── events.rs           # 键盘 / 鼠标 / 粘贴输入事件分发 (弹窗独占 → 补全选单 → 普通编辑)
    ├── app/                # App 响应式控制器、状态机与子系统调度
    │   ├── mod.rs          # App 结构体定义、生命周期与模块导出
    │   ├── session.rs      # 会话切换、SessionView 快照刷新与模态框状态调度
    │   ├── editor.rs       # 输入缓冲编辑、光标移动、斜杠命令补全与 @ 文件补全 (token 检测/候选刷新/应用插入)
    │   ├── dispatch.rs     # 用户提交、斜杠命令路由与会话请求分发
    │   ├── pane.rs         # TextPane 可选中文本面板与 ChatCache 历史前缀缓存 (命中判定/选区/行缓存)
    │   ├── selection.rs    # TextCoord/TextSelection/SelectionTracker 与剪贴板
    │   ├── scroll.rs       # 视口滚动与坐标映射
    │   ├── tests.rs        # App 完整自动化单元测试集
    │   └── test_support.rs # 测试共享基建 (临时 JSONL 存储 + App 构造)
    ├── command.rs          # 斜杠命令系统与 Claude Code 风格前缀自动补全、@ 文件候选模型 (FileCandidate)
    ├── history.rs          # SidebarStats 纯函数统计投影 (compute_stats)
    ├── input.rs            # InputBuffer 多行文本编辑、光标计算与 CJK 对齐算法
    ├── markdown.rs         # Markdown 解析与终端样式渲染 (pulldown-cmark)
    ├── text.rs             # 文本展示工具 (按显示宽度截断 truncate，CJK 双列)
    ├── modals/             # 独立的模态弹窗状态机与单测
    │   ├── mod.rs          # ActiveModal 枚举 (变体直接携带状态)、ModalAction、FilterableModal Trait 与按键归一化
    │   ├── sessions.rs     # SessionsModalState (按工程过滤与会话切换)
    │   ├── models.rs       # ModelsModalState (模型清单筛选)
    │   ├── variants.rs     # VariantsModalState (推理档位切换)
    │   └── config.rs       # ConfigModalState (设置项交互切换)
    └── views/              # 纯渲染层组件
        ├── mod.rs          # 视图模块导出
        ├── chat/           # 聊天流渲染 (已完成轮次 + 流式草稿同源渲染)
        │   ├── mod.rs      # 模块导出 facade (外部路径不变)
        │   ├── state.rs    # ChatRender 输出槽与 RenderState 增量状态
        │   ├── pipeline.rs # 渲染管线 (append_parts/append_part/thought_kind 统一决策；用户附件 FileRef/Image/File 紧凑引用行)
        │   ├── tool.rs     # 工具调用头部行 (逐工具摘要)
        │   ├── tool_block.rs # 工具展开块 (边框/填充/结果正文)
        │   ├── diff.rs     # Edit 差异渲染 (LCS 统一/双栏)
        │   ├── thought.rs  # 思考内容渲染 (内联/折叠启发式)
        │   ├── citations.rs # 搜索引用注脚与 Sources 块
        │   ├── user_block.rs # 用户消息引用块与附件引用行 (push_user_attachment_row)
        │   ├── selection.rs # 文本选区 (提取/高亮/叠加)
        │   ├── view.rs     # 终端视口 (面板/滚动/状态行)
        │   └── tests.rs    # 聊天渲染测试集
        ├── config_modal.rs # 设置弹窗视图
        ├── header.rs       # 顶部标题栏
        ├── help_modal.rs   # 帮助弹窗视图
        ├── input_area.rs   # 复合输入区视图 (状态条、命令/文件建议浮层)
        ├── modal_shell.rs  # 模态弹窗公共外壳 (render_modal_shell 渲染骨架/居中布局/过滤行/滚动窗口/空态/提示行)
        ├── models_modal.rs # 模型选择弹窗视图
        ├── sessions_modal.rs # 会话选择弹窗视图
        ├── sidebar.rs      # 右侧统计看板
        └── variants_modal.rs # 推理档位弹窗视图
└── web/                    # Web 界面 (Axum 服务器)
    ├── mod.rs              # 模块导出 (run_server)
    ├── api.rs              # REST + SSE API (会话 CRUD/提交/中止/配置/事件流订阅/模型清单)
    └── server.rs           # 路由组装与静态资源服务 (rust_embed 内嵌前端 dist，SPA 回退)

frontend/                    # Web 前端源码 (React + Vite + TypeScript)；build.rs 构件期编译，产物 dist 由 rust_embed 打包进二进制；设计规范与开发契约见 frontend/AGENTS.md
├── src/App.tsx              # 根组件：会话状态与 SSE 协调核心、API 动作回调、布局编排
├── src/api.ts               # 统一 request() 请求层 (错误提取/空响应处理)
├── src/types.ts             # 后端 serde 序列化镜像类型
├── src/sessionSync.ts       # SSE 会话订阅
├── src/hooks/               # usePersistentState (localStorage 持久化) / useResizable (面板拖拽) /
│                            # useComposerVisibility (移动端输入框手势机) / useStatsPanel (用量弹层) /
│                            # useFileTree (懒加载目录树) / useMediaQuery
├── src/utils/               # highlight (hljs 语言映射/按行切分) / relatedFiles (会话涉及文件提取) / math (LaTeX 定界符归一) / editDiff (编辑入参 → 结构化差异) / changeMarks (本会话改动行落点)
├── src/styles/trace.css     # 工作轨迹样式 (轨道/字形/行网格/差异)
├── src/styles/files.css     # 文件区样式 (Explorer/Viewer/标签/IDE 三栏布局)
└── src/components/          # Sidebar / Chat / Composer / Parts (工作轨迹) + trace/describe (工具行模型) / files/ (Explorer 跳转搜索+会话文件+目录树 / Viewer 改动标记与区间定位 / Workspace 对话抽屉与 IDE 三栏) /
                             # StatsPanel / EmptyStage / Sash / AskUserModal / SettingsModal / icons
```

---

## 5. 后续开发与扩展指南

1. **新增模型/供应商适配**：
   - 在 `src/model/meta.rs` 中定义强类型结构体，并在 `TurnMeta` 中引入字段；
   - 在 `src/config/model.rs` 与 `src/config/resolver.rs` 中注册对应的 ProviderType 路由；
   - 在 `src/provider/` 中实现 `Provider` trait，在 `capabilities()` 中声明支持的原生能力；
   - 编写单测验证供应商专属特性（如 cache、signature、response_id、reasoning_effort）的双向转换保真性。
2. **多模态扩展**：
   - 读取图片/音视频文件时使用 `MediaSource::Path` 或 `MediaSource::Base64`；
   - 适配器将通用模态转为目标 API 兼容的 Payload（如 Gemini 的 `inline_data`，Anthropic 的 `base64` image source）。
3. **测试覆盖与规范约束**：
   - 修改核心模型、解析器、适配器或 UI 状态后，运行 `cargo test` 确保全量测试通过，并保持 `cargo clippy -- -D warnings` 零告警；
   - **单测 Mock 规范**：单测数据按照官方最新规范格式构造，不在生产适配器中增加未定义的兜底逻辑。
