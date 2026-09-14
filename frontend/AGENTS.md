# frontend/AGENTS.md — Web 前端设计规范与开发契约

vva Web 界面的设计方向为 **Terminal Editorial**：终端基因 + 编辑排版感，配色源自薇薇安紫罗兰色板。
本文件是前端开发的契约层；`src/styles.css` 的 `:root` 令牌是色彩与字体的**唯一事实源**。

## 1. 设计原则

1. **方向承诺**：保持既有美学方向并贯彻，不为局部便利引入风格外的样式。
2. **令牌优先**：永不硬编码颜色/字体；一律引用 `:root` 令牌。找不到合适令牌时先补令牌再用。
3. **紫罗兰主导 + 绯红点睛**：紫色承载全部交互语义；`--rose` 仅限运行状态脉冲一处，不得扩散。
4. **无框架**：不引入 UI 框架 / CSS 框架 / 原子类库；样式全部手写于 `src/styles.css`。

## 2. 色彩令牌（styles.css :root）

| 令牌 | 语义 |
|---|---|
| `--bg` / `--panel` / `--panel-2` | 页面底 / 面板 / 次级面（hover、代码底） |
| `--border` / `--border-soft` | 边框 / 弱分隔线 |
| `--text` / `--muted` / `--faint` | 正文 / 次要 / 最弱层级文字 |
| `--accent` / `--accent-hover` / `--accent-soft` / `--on-accent` | 紫罗兰信号色：链接、激活态、聚焦环、主按钮、搜索开关选中 |
| `--rose` | 绯红点睛：状态脉冲点 |
| `--danger` / `--danger-soft` | 破坏性操作与错误（删除按钮、错误横幅/错误块） |
| `--warning` / `--warning-soft` | Notice 横幅 |
| `--dot` | 聊天区点阵纹理 |

- 浅色（薰衣草纸面）/ 深色（深茄紫夜面）由 `prefers-color-scheme` 自动切换，新增颜色必须同时定义两套。
- 滚动条已全局主题化（thin、透明轨道、圆角拇指），禁止默认样式回归。

## 3. 字体

| 令牌 | 字体 | 用途 |
|---|---|---|
| `--font-display` | Space Grotesk | 品牌、页面/弹窗标题、空态大字 |
| `--font-body` | 系统栈（CJK 友好） | 正文、消息、输入框 |
| `--font-mono` | JetBrains Mono | 一切**元信息**：项目分组、统计面板、耗时/token、状态标签、chips、页脚（可配大写 + `letter-spacing: 0.05em`） |

字体经 `@fontsource/*` 打包进产物（见 `src/main.tsx`），离线可用；不得改为 CDN 引用。

## 4. 版式与组件契约

- **内容列**：聊天与输入框 `max-width: 760px` 居中；聊天区背景为 22px 网格点阵（`--dot`）。
- **侧栏**：宽度可拖拽（200–420px，经右缘 `.sidebar-resize` 手柄，Pointer Capture 实现），宽度与收起状态持久化于 localStorage；收起仅作用于桌面断点，窄屏保持抽屉式。页脚为 Settings 入口（`.sidebar-foot` 按钮），打开全局配置弹窗。
- **项目分组**：标签用文件夹图标表达折叠状态（展开=打开文件夹/收起=闭合文件夹），图标按项目名哈希着紫罗兰系色；mono 加粗标签 + 会话数胶囊徽章 + 悬停显示新建与隐藏按钮（新建直接在该项目 workspace 下创建会话）。折叠状态与隐藏列表均持久化于 localStorage（`vva.projects-collapsed` / `vva.projects-hidden`）；打开或新建位于隐藏项目的会话时该项目自动恢复显示。
- **新建项目目录点选**：新建模式为多级目录点选（`GET /api/fs/dirs` 列一级子目录，含上一级按钮），路径文本保持原始大小写（父级 uppercase 需显式覆盖）。
- **气泡**：用户消息反色（`--bubble-user` 底 + `--bubble-user-text`），圆角 `14px 14px 4px 14px`；支持在输入框粘贴图片附件（Base64 随消息提交），气泡内以缩略图展示；悬停气泡显示操作行（复制原文、回退=丢弃此轮及之后并放回输入框、分叉=保留之前历史创建新会话并切换），运行中禁用，触屏常显。
- **空态**：无会话时为 `.empty-stage` 居中版式（品牌大字 + 提示 + 强调输入框），输入框工具行含项目指定；发首条消息即按所选项目创建会话。
- **内容块**：统一走 `.block`（`<details>` + `<summary>` + `▸` 箭头旋转展开），宽度恒 100%；收起为 `--block` 底（融入页面的薰衣草弱色，浅色取 `--panel-2` 调、深色取 `--panel`，禁用纯白）+ `--border-soft` 描边的弱卡片行（hover 加深至 `--border-soft`），展开整块着 `--panel-2` 且描边加深（错误块 `--danger-soft`）；展开/收起经 `::details-content` + `interpolate-size` 高度过渡（Chromium 131+，不支持则直接切换）。搜索/工具块共用此模式；思考块去卡片化——透明底无边框、摘要 `--faint` 弱存在感，展开亦不加载卡片底盘。
- **工具卡片**：`tool_call` 与其 `tool_result` 按 `tool_call_id` 配对。文件相关工具（Read / Write / Edit）以单行展示（`.block.tool.file-line`，状态 + 工具名 + 路径/diff 统计），不以 block 形式展开，点击直接在右侧边栏预览对应文件；非文件工具（Shell / Glob / Grep）保持卡片折叠块（摘要行 + 命令预览 + 结果前 10 行 +「展开全部」）。展开跟随响应：非文件类当前末尾块全局唯一自动展开（工作段与草稿间不重复保留，工作段仅在草稿为空的间隙保留末尾展开块）、结束后全部收起，错误块不自动展开仅保留 ✕ 标识，用户可手动开合。
- **用时折叠**：纯文本行 `.worked`（`用时 Xm YYs ›`），非卡片；折叠粒度为整个运行段（两次用户消息之间的助手轮序列）——中间轮的思考/工具与文本消息均并入折叠，仅运行段最后一条助手消息正常展示（悬停显示复制操作，复制回答原文 Markdown），用时取末轮 `worked_ms`（整请求周期）；运行段进行中保持展开、结束自动收起，中断或含错误的段同样收起（失败标识在段内卡片摘要行），与最终回答之间以 `--border-soft` 弱分隔线区隔，展开后内部块与外部块同宽同层级，**不允许出现嵌套卡片**。
- **引用注脚**：消息文本的 url_citation 渲染为 `.citations` 折叠行（`引用来源（N）`），按 URL 去重（对齐 TUI `search_citations`），默认收起，展开为编号列表。
- **按钮体系**：`new-create`/`send-button`（主色实心）、`ghost-button`/`new-cancel`（描边）、`danger-button`（绯红警示）、`icon-ghost`（图标幽灵）、`chip`/`chip-toggle`（输入框工具行）。
- **顶栏**：半透明毛玻璃（`color-mix` 半透明 `--panel` + `backdrop-filter: blur`），脱离文档流覆盖主区（高度令牌 `--topbar-h`），聊天内容从其下方滚过；横幅、聊天区与空态自行让出顶栏高度。
- **浮层**：
  - 锚定弹层（`.new-panel`）：瞬态表单，点击外部/Esc 关闭；含「已有项目 / 新建项目」双模式（`.segmented` 切换），新项目为绝对路径输入，目录由后端自动创建；
  - 统计浮块（`.stats-panel`）：右上角浮动（会话活跃时），**非阻塞**——无遮罩，主界面可操作；内容滚入面板区域时自动收起（`opacity: 0` 保留测量、不参与命中测试），离开后自动展开；顶栏右缘开关（`.stats-toggle`）为手动覆盖——隐藏时点击强制显示（保持到不再遮挡）、展开时点击手动收起（持久化于 localStorage `vva.stats-collapsed`）；字段：Session / Workspace / Turns / Context（数值 + 比例条，上限缺省 200k）/ In / Out / TPS / Cache Read / Cache Write / Est. Cost；模型与档位在输入框工具行已展示，不重复；
  - 设置弹窗（`.settings-modal`）：侧栏页脚打开，编辑全局配置文件原文（JSONC，路径示于标题行右侧），后端校验解析通过后落盘并热重载；Esc/遮罩点击关闭；
  - 居中确认弹窗（`.modal-overlay` + `.confirm-modal`）：**仅用于破坏性操作**，带遮罩与模糊。
- **动效**：入场 `rise`（分组列表 stagger 45ms、上限 8 组）、`pulse` 状态点；微交互 `transition 0.15s`。不加大型/装饰性动画。
- **图标**：内联 SVG（stroke 2、round），不引入图标库；颜色继承 `currentColor`。

## 5. 工程契约

- **构建**：`cargo build` 经 `build.rs` 自动 `npm ci && npm run build`，产物 `dist/` 由 rust_embed 打包进二进制；`dist/` 与 `node_modules/` 不入库。
- **类型契约**：`src/types.ts` 必须与后端 `src/model/*` 的 serde 序列化格式一一镜像；新增后端字段同步更新。
- **API 层**：所有请求经 `src/api.ts` 的 `request()`（统一错误提取与空响应处理），不得在组件内裸写 fetch。
- **错误反馈**：请求失败走 App 顶部 error banner；后端 Notice 走 notice banner。
- **验证**：改动后 `npm run build`（含 `tsc --noEmit`）+ `cargo build`；视觉变更用 headless Chromium 截图验证浅/深双主题。
