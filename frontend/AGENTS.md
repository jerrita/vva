# frontend/AGENTS.md — Web 前端设计规范与开发契约

vva Web 界面的设计方向为 **Terminal Editorial**：终端基因 + 编辑排版感，配色源自薇薇安紫罗兰色板。
本文件是前端开发的契约层；`src/styles.css` 的 `:root` 令牌是色彩与字体的**唯一事实源**。

## 1. 设计原则

1. **方向承诺**：保持既有美学方向并贯彻，不为局部便利引入风格外的样式。
2. **令牌优先**：永不硬编码颜色/字体；一律引用 `:root` 令牌。找不到合适令牌时先补令牌再用。
3. **紫罗兰主导 + 绯红点睛**：紫色承载全部交互语义；`--rose` 仅表示「正在运行」（聊天 `status-chip`、侧栏生成中指示点、轨迹中运行中的步骤字形），不得扩散到其他语义。
4. **无框架**：不引入 UI 框架 / CSS 框架 / 原子类库；样式全部手写于 `src/styles.css`（工作轨迹独立为 `src/styles/trace.css`）。

## 2. 色彩令牌（styles.css :root）

| 令牌 | 语义 |
|---|---|
| `--bg` / `--panel` / `--panel-2` | 页面底（工作台底板） / 面板（圆角分区） / 次级面（hover、代码底） |
| `--border` / `--border-soft` | 边框 / 弱分隔线 |
| `--text` / `--muted` / `--faint` | 正文 / 次要 / 最弱层级文字 |
| `--accent` / `--accent-hover` / `--accent-soft` / `--on-accent` | 紫罗兰信号色：链接、激活态、聚焦环、主按钮、搜索开关选中 |
| `--rose` | 绯红点睛：状态脉冲点 |
| `--danger` / `--danger-soft` | 破坏性操作与错误（删除按钮、错误横幅/错误块） |
| `--diff-add` / `--diff-del` | 编辑差异的增删行与计数 |
| `--warning` / `--warning-soft` | Notice 横幅 |
| `--dot` | 聊天区点阵纹理 |
| `--radius-pane` / `--workbench-gap` | 工作台面板圆角 / 窗口内边距与列间距（两套主题共用） |

- 浅色（薰衣草纸面）/ 深色（深茄紫夜面）由 `prefers-color-scheme` 自动切换，新增颜色必须同时定义两套。
- 滚动条已全局主题化（thin、透明轨道、默认隐去、悬停显现圆角拇指），禁止默认样式回归。

## 3. 字体

| 令牌 | 字体 | 用途 |
|---|---|---|
| `--font-display` | Space Grotesk | 品牌、页面/弹窗标题、空态大字 |
| `--font-body` | 系统栈（CJK 友好） | 正文、消息、输入框 |
| `--font-mono` | JetBrains Mono | 一切**元信息**：项目分组、统计面板、耗时/token、状态标签、chips、页脚（可配大写 + `letter-spacing: 0.05em`） |

字体经 `@fontsource/*` 打包进产物（见 `src/main.tsx`），离线可用；不得改为 CDN 引用。

## 4. 版式与组件契约

- **工作台**：桌面端 `.app` 以 `--workbench-gap`（6px）作窗口内边距；列间为同宽 `.sash` 分隔柱（整列可拖，中点三点、无胶囊外壳），侧栏 / 主区 / 文件抽屉各自为 `--radius-pane`（14px）圆角面板，坐落在 `--bg` 底板上。窄屏满铺，抽屉仍为 overlay。
- **内容列**：聊天与输入框 `max-width: 760px` 居中；聊天区背景为 22px 网格点阵（`--dot`）。
- **侧栏**：宽度可拖拽（200–420px，经右侧 `.sash` 分隔柱，Pointer Capture 实现），宽度与收起状态持久化于 localStorage；收起仅作用于桌面断点，窄屏保持抽屉式。页脚为「设置」入口（`.sidebar-foot` 齿轮图标 + 文字按钮），打开全局配置弹窗。项目分组默认仅展示最新 6 条顶层会话，其余折叠为 `.group-more`「还有 N 条」行（展开集合持久化于 `vva.projects-expanded`），当前会话、生成中与未读会话始终可见；切换会话后当前条目自动滚入可视区。会话条目右侧以 mono `.session-time` 展示相对创建时间（`now`/`12m`/`3h`/`5d`，超 30 天为月-日），悬停/聚焦时让位于删除按钮。会话条目与分组标签为 `role=button` 可聚焦元素，Enter/空格激活。会话条目：生成中为 `--rose` 脉冲点（`--rose` 仅限此状态点与聊天 `status-chip`）；生成结束且当前未打开为 `--accent` 实心未读点（打开即清，集合持久化于 `vva.sessions-unread`）。subagent 会话按 `parent` 嵌套在父会话之下（缩进 + `.session-fork` 分支标记，父不在列表则保持顶层），点击切换会话；会话清单定期刷新（有生成中会话 4s，全部空闲 15s 兜底），使引擎侧派生的子会话与运行状态及时呈现，服务端已消失的会话（含级联删除的子会话）随之移除。删除会话级联删除其全部子会话（后端 `delete_session` 递归）。顶部 Notice 横幅中的会话、轮次反馈带关闭按钮，能力提示随配置更新（`.notice-dismiss` 靠左，避开顶栏右侧按钮；`POST /api/sessions/{id}/notices/dismiss` 按稳定 ID 移除，SSE 同步全部订阅者）。
- **文件区（两种布局）**：`components/files/` 下 `Explorer`（跳转搜索 → 本次会话 → 工程目录树）与 `Viewer`（面包屑 → 元信息/改动导航 → 代码）为共用部件，`Workspace.tsx` 组装为两种布局；样式在 `src/styles/files.css`。
  - **对话模式**（默认）：右侧单列抽屉（`.file-sidebar`，300–1000px 可拖拽），在「浏览」与「文件」两视图间切换，两视图常驻 DOM（返回时目录展开与搜索词保持）；Esc / 返回键回浏览；手机满屏。
  - **IDE 模式**（仅桌面，`vva.layout-mode` 持久化）：文件（`.explorer-pane`）| 编辑区（`.editor-pane`，标签最多 12 个、中键关闭）| 对话（`.main`，`--ai-panel-width` 360–720px），会话列表改为左侧浮层（顶栏菜单键呼出）。对话栏内工作轨迹经容器查询（`.chat-scroll` `container-type`）收窄。
  - **快捷键**：`⌘/Ctrl+P` 聚焦文件搜索（后端 `GET /api/fs/find` 模糊匹配，命中字符高亮，↑↓ 选择、Enter 打开）；`⌘/Ctrl+\` 切换两种布局；顶栏与抽屉头部亦有切换按钮。
  - **标记语言**：沿用工作轨迹字形——● 本会话修改（带 `+N −M`）、○ 仅读取；目录树中含改动的祖先目录带弱紫点；树缩进参考线对齐上级展开箭头。本次会话文件按「有改动在前、各自按新近度」排序（`utils/relatedFiles.ts`，覆盖 replace / patch / hashline 三种编辑器）。
  - **定位**：聊天中 Read 行打开时高亮读取区间（`--warning` 底色）并滚入视区；Edit 的「在侧栏打开」与本会话改动文件定位到首处改动。改动行 = 本会话 diff 新增块在当前内容中的落点（`utils/changeMarks.ts`：块匹配，replace 片段按首行后缀/末行前缀，块被打散时回落为唯一行），以行号栏紫色竖条标记，元信息行提供「N 处」上一处/下一处跳转。
  - **Viewer 工具**：面包屑目录段点击在文件树中定位；Markdown 预览/源码切换；自动换行开关（`vva.viewer-wrap`）；复制、下载；行号横向滚动时吸附。
  - **媒体预览**：PDF 使用浏览器内置阅读器，音频使用原生播放器；两者经 `/api/fs/preview` 或 `/api/sessions/{id}/preview` 流式读取，支持 Range / HEAD，无大小限制；文件元信息接口返回空 `content`。文本与图片保留 10MB 预览上限。
- **项目分组**：标签用文件夹图标表达折叠状态（展开=打开文件夹/收起=闭合文件夹），图标按项目名哈希着紫罗兰系色；mono 加粗标签 + 会话数胶囊徽章 + 悬停显示新建与隐藏按钮（新建预选该项目 workspace 并回空态，会话在首条消息发送时创建）。折叠状态与隐藏列表均持久化于 localStorage（`vva.projects-collapsed` / `vva.projects-hidden`）；打开或新建位于隐藏项目的会话时该项目自动恢复显示。
- **新建项目目录点选**：新建模式为多级目录点选（`GET /api/fs/dirs` 列一级子目录，含上一级按钮），路径文本保持原始大小写（父级 uppercase 需显式覆盖）。
- **气泡**：用户消息反色（`--bubble-user` 底 + `--bubble-user-text`），圆角 `14px 14px 4px 14px`；支持在输入框粘贴图片附件（Base64 随消息提交），气泡内以缩略图展示；悬停气泡显示操作行（复制原文、回退=丢弃此轮及之后并放回输入框、分叉=保留之前历史创建新会话并切换），运行中禁用，触屏常显。
- **空态**：无会话时为 `.empty-stage` 居中版式（品牌大字 + 项目行 + 输入框），项目行（`.empty-project`：文件夹图标 + 项目名下拉 + 完整路径）位于输入框上方，输入框样式与会话内一致；发首条消息即按所选项目创建会话。
- **工作轨迹（trace）**：运行段内连续的思考 / 工具 / 搜索步骤串在一条细轨道上（`.trace::before`），正文文本打断轨道成段。实现：`components/Parts.tsx`（节点分段、配对、批次合并、行组件）+ `components/trace/describe.ts`（按工具生成行模型）+ `utils/editDiff.ts`（Edit / Write / apply_patch / hashline_edit → 结构化差异；工具名常量集中于 `src/toolNames.ts`，按具体名字精确分支，apply_patch 与 hashline_edit 的 arguments 为补丁原文，hashline_edit 成功时以结果中的真实 diff 取代推算行）；样式在 `src/styles/trace.css`。
  - **行网格**：字形 | 动词（mono，`--trace-verb` 列宽）| 目标 + 弱化补充（Shell 命令、Grep 范围）| 结果（右对齐 mono：`L260–299`、`+3 −4`、`36 处`、`后台`、`失败`）| 操作提示列（可展开=箭头、预览=文件图标，悬停浮现，触屏常显）。
  - **字形编码**：形状表达动作类别，颜色只表达状态——○ 查看（Read/Grep/Glob/web/Skill）、● 修改（Edit/Write，`--accent` 实心）、▪ 执行（Shell/Task）、◇ 思考、◯ 委派（Agent/SendMessage/Wait/AskUser，`--accent` 描边）、✕ 失败（`--danger`）、运行中为 `--rose` 脉冲。
  - **交互**：Read 行点击在侧栏预览；Edit/Write 行展开为内联差异（`--diff-add` / `--diff-del` 着色，文件头带「在侧栏打开」）；Shell 展开为 `$ 命令` + 输出；Glob/Grep 展开为匹配列表；出错行展开为 `--danger-soft` 底错误原文；思考行收起时显示首行预览，流式中显示最新一行（跑马灯），展开为 Markdown 全文。
  - **批次合并**：连续 ≥3 个同名且成功的 Read/Grep/Glob 合并为一行（`Read 4 个文件 mod.rs model.rs …`），展开为缩进的逐项行。
  - **不自动展开**：流式中任何步骤都不自动展开（避免内容跳动），live 仅影响运行字形与思考跑马灯；展开/收起经 `::details-content` + `interpolate-size` 高度过渡。
- **机器注入轮次**：`<system-reminder>` 包装的用户轮次不渲染为用户气泡、无回退/分叉操作；全部 reminder 不渲染，轮隙不切断运行段折叠；
- **后台工具块**：Shell / Agent 原位展示后台运行、完成、失败、停止与中断状态；运行字形保留动作形状并使用 `--rose` 脉冲。结果列为状态，Shell 展开实际输出，Agent 展开 Markdown 最终回复，任务 ID 与截断说明放块内弱化元信息。TaskOutput 归入原块，查询失败在块内保留；SendMessage 与 TaskStop 保留独立动作，跨会话回复自动回投。后台状态经 `SessionSnapshot.background_tasks` / SSE 增量传递，完成结果持久化，重载后尚未结束的任务显示已中断。后台状态更新保留展开与滚动位置；折叠摘要显示后台运行数；
- **用时折叠**：`.worked` 摘要行 = `用时 Xm YYs` + 动作计数（查看 / 搜索 / 修改 / 执行 / 委派，`失败 N` 以 `--danger` 单列），进行中显示「进行中」；折叠粒度为整个运行段（两次用户消息之间的助手轮序列）——中间轮的思考/工具与文本消息均并入折叠，仅运行段最后一条助手消息正常展示（悬停显示复制操作），用时取末轮 `worked_ms`；运行段进行中保持展开、结束自动收起，与最终回答之间以弱分隔线区隔，**不允许出现嵌套卡片**。
- **代码块**：`.code-window` 细标题栏仅含语言名（缺省 `code`）与复制按钮，不加窗口圆点等装饰。
- **CJK 强调**：Markdown 经 `remark-cjk-friendly` 解析，`**结论：**正文` 这类紧邻中文标点的强调正常生效。
- **引用注脚**：消息文本的 url_citation 渲染为 `.citations` 折叠行（`引用来源（N）`），按 URL 去重（对齐 TUI `search_citations`），默认收起，展开为编号列表。
- **按钮体系**：`primary-button`/`send-button`（主色实心）、`ghost-button`（描边）、`danger-button`（绯红警示）、`icon-ghost`（图标幽灵）、`chip`/`chip-toggle`（输入框工具行）。
- **输入框**：会话底栏；窄屏向上滚动隐藏（聚焦时除外），回到底部或从屏幕底部上滑重新展示。
- **顶栏**：半透明毛玻璃（`color-mix` 半透明 `--panel` + `backdrop-filter: blur`），脱离文档流覆盖主区（高度令牌 `--topbar-h`），聊天内容从其下方滚过；横幅、聊天区与空态自行让出顶栏高度。桌面端主区 `overflow: hidden` 裁切至圆角。
- **浮层**：
  - 锚定弹层（`.new-panel`）：瞬态表单，点击外部/Esc 关闭；含「已有项目 / 新建项目」双模式（`.segmented` 切换），新项目为绝对路径输入，目录由后端自动创建；
  - 用量统计弹层（`.stats-popover`）：锚定顶栏开关（`.stats-toggle`）正下方右对齐（约 260px，面板底/边框/阴影/圆角 12），默认关闭，**不覆盖聊天内容**；点击外部、Esc、切换会话关闭（带 `data-stats-popover` / `data-stats-trigger` 的元素不算外部），无持久化；入场仅 140ms 透明度 + 4px 位移，`prefers-reduced-motion` 下关闭。三组以 `--border-soft` 分隔：上下文（大号 `97,292 / 500k` + 比例条与百分比，≥70% `--warning`、≥90% `--danger`）/ In、Out、Cache read、Cache write、TPS / Turns、Editor、Est. cost；底部一行 faint mono 的会话 ID · workspace，附 CopyButton 复制会话 ID。标签为句首大写正文体，不使用全大写与字距。窄屏（≤760px）弹层相对顶栏左右各留 12px 铺满。输入框工具行发送键左侧有 16px 上下文圆环（`.ctx-ring`，`--accent`，轨道 `--border`，≥70% `--warning`、≥90% `--danger`，title/aria-label `上下文 97,292 / 500k（19%）`），点击打开同一弹层；无统计（空态）不显示；
  - 设置弹窗（`.settings-modal`）：侧栏页脚打开，编辑全局配置文件原文（JSONC，路径示于标题行右侧），后端校验解析通过后落盘并热重载；Esc/遮罩点击关闭；
  - 居中确认弹窗（`.modal-overlay` + `.modal`）：**仅用于破坏性操作**，带遮罩与模糊；设置弹窗复用 `.modal` 基类（面板底、边框、阴影、入场动画）叠加自身尺寸。
- **AskUser 面板**：停靠输入框上方（`.ask-user-dock` 内联展开，复用 `.modal` 基类），无遮罩不挡聊天记录，历史可滚动回看；面板限高（`min(440px, 60dvh)`）内部滚动；提问挂起期间输入区保持展示（窄屏下滑隐藏不生效）。
- **动效**：入场 `rise`（分组列表 stagger 45ms、上限 8 组）、`pulse` 状态点；微交互 `transition 0.15s`。不加大型/装饰性动画；`prefers-reduced-motion` 下入场与脉冲动画全部关闭。
- **键盘聚焦**：全局 `:focus-visible` 为 2px `--accent` 描边，禁止以 `outline: none` 移除而不提供替代样式。
- **图标**：全部内联 SVG，集中定义于 `src/components/icons.tsx`（stroke 2、round，顶栏面板类 1.8），不引入图标库；颜色继承 `currentColor`。

## 5. 工程契约

- **构建**：`cargo build` 经 `build.rs` 自动 `npm ci && npm run build`，产物 `dist/` 由 rust_embed 打包进二进制；`dist/` 与 `node_modules/` 不入库。
- **类型契约**：`src/types.ts` 必须与后端 `src/model/*` 的 serde 序列化格式一一镜像；新增后端字段同步更新。
- **API 层**：所有请求经 `src/api.ts` 的 `request()`（统一错误提取与空响应处理），不得在组件内裸写 fetch。
- **错误反馈**：请求失败走 App 顶部 error banner；后端 Notice 走 notice banner。
- **验证**：改动后 `npm run lint` + `npm run build`（含 `tsc --noEmit`）+ `cargo build`；视觉变更用 headless Chromium 截图验证浅/深双主题。
