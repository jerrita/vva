# frontend/AGENTS.md — Web 前端设计规范与开发契约

vva Web 界面的设计方向为 **Terminal Editorial**：终端基因 + 编辑排版感，配色源自薇薇安紫罗兰色板。
本文件是前端开发的契约层；`src/styles.css` 的 `:root` 令牌是色彩与字体的**唯一事实源**。

## 1. 设计原则

1. **方向承诺**：保持既有美学方向并贯彻，不为局部便利引入风格外的样式。
2. **令牌优先**：永不硬编码颜色/字体；一律引用 `:root` 令牌。找不到合适令牌时先补令牌再用。
3. **紫罗兰主导 + 绯红点睛**：紫色承载全部交互语义；`--rose` 仅限运行状态脉冲（聊天 `status-chip` 与侧栏生成中指示点），不得扩散。
4. **无框架**：不引入 UI 框架 / CSS 框架 / 原子类库；样式全部手写于 `src/styles.css`。

## 2. 色彩令牌（styles.css :root）

| 令牌 | 语义 |
|---|---|
| `--bg` / `--panel` / `--panel-2` | 页面底（工作台底板） / 面板（圆角分区） / 次级面（hover、代码底） |
| `--border` / `--border-soft` | 边框 / 弱分隔线 |
| `--text` / `--muted` / `--faint` | 正文 / 次要 / 最弱层级文字 |
| `--accent` / `--accent-hover` / `--accent-soft` / `--on-accent` | 紫罗兰信号色：链接、激活态、聚焦环、主按钮、搜索开关选中 |
| `--rose` | 绯红点睛：状态脉冲点 |
| `--danger` / `--danger-soft` | 破坏性操作与错误（删除按钮、错误横幅/错误块） |
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
- **侧栏**：宽度可拖拽（200–420px，经右侧 `.sash` 分隔柱，Pointer Capture 实现），宽度与收起状态持久化于 localStorage；收起仅作用于桌面断点，窄屏保持抽屉式。页脚为 Settings 入口（`.sidebar-foot` 按钮），打开全局配置弹窗。会话条目：生成中为 `--rose` 脉冲点（`--rose` 仅限此状态点与聊天 `status-chip`）；生成结束且当前未打开为 `--accent` 实心未读点（打开即清，集合持久化于 `vva.sessions-unread`）。subagent 会话按 `parent` 嵌套在父会话之下（缩进 + `.session-fork` 分支标记，父不在列表则保持顶层），点击切换会话；会话清单定期刷新（有生成中会话 4s，全部空闲 15s 兜底），使引擎侧派生的子会话与运行状态及时呈现，服务端已消失的会话（含级联删除的子会话）随之移除。删除会话级联删除其全部子会话（后端 `delete_session` 递归）。顶部 Notice 横幅每条带关闭按钮（`.notice-dismiss` 靠左，避开右上统计浮块；`POST /api/sessions/{id}/notices/dismiss` 按下标移除，本地乐观移除 + SSE 同步全部订阅者）。
- **文件抽屉**：宽度可拖拽（240–760px，经左侧 `.sash` 分隔柱，Pointer Capture 实现），支持「工程文件」（树状目录懒加载、折叠展开与点选预览）与「会话变动」（Read/Write/Edit 变动汇总）双选项卡切换。
- **项目分组**：标签用文件夹图标表达折叠状态（展开=打开文件夹/收起=闭合文件夹），图标按项目名哈希着紫罗兰系色；mono 加粗标签 + 会话数胶囊徽章 + 悬停显示新建与隐藏按钮（新建预选该项目 workspace 并回空态，会话在首条消息发送时创建）。折叠状态与隐藏列表均持久化于 localStorage（`vva.projects-collapsed` / `vva.projects-hidden`）；打开或新建位于隐藏项目的会话时该项目自动恢复显示。
- **新建项目目录点选**：新建模式为多级目录点选（`GET /api/fs/dirs` 列一级子目录，含上一级按钮），路径文本保持原始大小写（父级 uppercase 需显式覆盖）。
- **气泡**：用户消息反色（`--bubble-user` 底 + `--bubble-user-text`），圆角 `14px 14px 4px 14px`；支持在输入框粘贴图片附件（Base64 随消息提交），气泡内以缩略图展示；悬停气泡显示操作行（复制原文、回退=丢弃此轮及之后并放回输入框、分叉=保留之前历史创建新会话并切换），运行中禁用，触屏常显。
- **空态**：无会话时为 `.empty-stage` 居中版式（品牌大字 + 提示 + 项目选择 + 输入框），项目选择位于输入框上方，输入框样式与会话内一致；发首条消息即按所选项目创建会话。
- **内容展示**：思考（`.reasoning-row`）、搜索（`.search-row`）与工具（`.tool-row`）均去卡片化，采用透明底与无边框设计；展开/收起经 `::details-content` + `interpolate-size` 高度过渡（Chromium 131+，不支持则直接切换）。
- **工具展示**：`tool_call` 与其 `tool_result` 按 `tool_call_id` 配对，统一步调为 `.tool-row` 去卡片化样式（透明底无边框，状态 + 工具名 + 详情）。文件相关工具（Read / Write / Edit）单行展示路径与 diff 统计，点击调起右侧边栏预览；搜索匹配工具（Glob / Grep）单行展示模式与匹配数统计（如 `(xx matches)`），不展开具体内容；Shell 工具单行展示意图与命令，点击可展开输出结果，后台启动在意图后标注 `(background)`；后台任务工具（TaskOutput / TaskStop）单行展示 task_id，点击可展开任务输出。错误块保留 ✕ 标识与错误摘要。
- **机器注入轮次**：`<system-reminder>` 包装的用户轮次不渲染为用户气泡、无回退/分叉操作；全部 reminder（含 `background-task` 后台任务完成通知）不渲染——完成信号经 Notice 横幅传达（可关闭），reminder 轮隙不切断运行段折叠；
- **用时折叠**：纯文本行 `.worked`（`用时 Xm YYs ›`），非卡片；折叠粒度为整个运行段（两次用户消息之间的助手轮序列）——中间轮的思考/工具与文本消息均并入折叠，仅运行段最后一条助手消息正常展示（悬停显示复制操作，复制回答原文 Markdown），用时取末轮 `worked_ms`（整请求周期）；运行段进行中保持展开、结束自动收起，中断或含错误的段同样收起（失败标识在段内卡片摘要行），与最终回答之间以 `--border-soft` 弱分隔线区隔，展开后内部块与外部块同宽同层级，**不允许出现嵌套卡片**。
- **引用注脚**：消息文本的 url_citation 渲染为 `.citations` 折叠行（`引用来源（N）`），按 URL 去重（对齐 TUI `search_citations`），默认收起，展开为编号列表。
- **按钮体系**：`primary-button`/`send-button`（主色实心）、`ghost-button`（描边）、`danger-button`（绯红警示）、`icon-ghost`（图标幽灵）、`chip`/`chip-toggle`（输入框工具行）。
- **输入框**：会话底栏；窄屏向上滚动隐藏（聚焦时除外），回到底部或从屏幕底部上滑重新展示。
- **顶栏**：半透明毛玻璃（`color-mix` 半透明 `--panel` + `backdrop-filter: blur`），脱离文档流覆盖主区（高度令牌 `--topbar-h`），聊天内容从其下方滚过；横幅、聊天区与空态自行让出顶栏高度。桌面端主区 `overflow: hidden` 裁切至圆角。
- **浮层**：
  - 锚定弹层（`.new-panel`）：瞬态表单，点击外部/Esc 关闭；含「已有项目 / 新建项目」双模式（`.segmented` 切换），新项目为绝对路径输入，目录由后端自动创建；
  - 统计浮块（`.stats-panel`）：右上角浮动（会话活跃时），**非阻塞**——无遮罩，主界面可操作；内容滚入面板区域时自动收起（`opacity: 0` 保留测量、不参与命中测试），离开后自动展开；顶栏右缘开关（`.stats-toggle`）为手动覆盖——隐藏时点击强制显示（保持到不再遮挡）、展开时点击手动收起（持久化于 localStorage `vva.stats-collapsed`）；字段：Session / Workspace / Turns / Context（数值 + 比例条，上限缺省 200k）/ In / Out / TPS / Cache Read / Cache Write / Est. Cost；模型与档位在输入框工具行已展示，不重复；
  - 设置弹窗（`.settings-modal`）：侧栏页脚打开，编辑全局配置文件原文（JSONC，路径示于标题行右侧），后端校验解析通过后落盘并热重载；Esc/遮罩点击关闭；
  - 居中确认弹窗（`.modal-overlay` + `.modal`）：**仅用于破坏性操作**，带遮罩与模糊；设置弹窗复用 `.modal` 基类（面板底、边框、阴影、入场动画）叠加自身尺寸。
- **AskUser 面板**：停靠输入框上方（`.ask-user-dock` 内联展开，复用 `.modal` 基类），无遮罩不挡聊天记录，历史可滚动回看；面板限高（`min(440px, 60dvh)`）内部滚动；提问挂起期间输入区保持展示（窄屏下滑隐藏不生效）。
- **动效**：入场 `rise`（分组列表 stagger 45ms、上限 8 组）、`pulse` 状态点；微交互 `transition 0.15s`。不加大型/装饰性动画。
- **图标**：全部内联 SVG，集中定义于 `src/components/icons.tsx`（stroke 2、round，顶栏面板类 1.8），不引入图标库；颜色继承 `currentColor`。

## 5. 工程契约

- **构建**：`cargo build` 经 `build.rs` 自动 `npm ci && npm run build`，产物 `dist/` 由 rust_embed 打包进二进制；`dist/` 与 `node_modules/` 不入库。
- **类型契约**：`src/types.ts` 必须与后端 `src/model/*` 的 serde 序列化格式一一镜像；新增后端字段同步更新。
- **API 层**：所有请求经 `src/api.ts` 的 `request()`（统一错误提取与空响应处理），不得在组件内裸写 fetch。
- **错误反馈**：请求失败走 App 顶部 error banner；后端 Notice 走 notice banner。
- **验证**：改动后 `npm run build`（含 `tsc --noEmit`）+ `cargo build`；视觉变更用 headless Chromium 截图验证浅/深双主题。
