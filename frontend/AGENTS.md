# frontend/AGENTS.md — Web 前端设计规范与开发契约

vva Web 界面的设计方向为 **Terminal Editorial**：终端基因 + 编辑排版感，配色源自薇薇安紫罗兰色板。
本文件是前端开发的契约层；`src/styles.css` 的 `:root` 令牌是色彩与字体的**唯一事实源**。

## 1. 设计原则

1. **方向承诺**：保持既有美学方向并贯彻，不为局部便利引入风格外的样式。
2. **令牌优先**：永不硬编码颜色/字体；一律引用 `:root` 令牌。找不到合适令牌时先补令牌再用。
3. **紫罗兰主导 + 绯红点睛**：紫色承载全部交互语义；`--rose` 仅限品牌光标与运行状态脉冲两处，不得扩散。
4. **无框架**：不引入 UI 框架 / CSS 框架 / 原子类库；样式全部手写于 `src/styles.css`。

## 2. 色彩令牌（styles.css :root）

| 令牌 | 语义 |
|---|---|
| `--bg` / `--panel` / `--panel-2` | 页面底 / 面板 / 次级面（hover、代码底） |
| `--border` / `--border-soft` | 边框 / 弱分隔线 |
| `--text` / `--muted` / `--faint` | 正文 / 次要 / 最弱层级文字 |
| `--accent` / `--accent-hover` / `--accent-soft` / `--on-accent` | 紫罗兰信号色：链接、激活态、聚焦环、主按钮、搜索开关选中 |
| `--rose` | 绯红点睛：品牌光标、状态脉冲点 |
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
- **气泡**：用户消息反色（`--bubble-user` 底 + `--bubble-user-text`），圆角 `14px 14px 4px 14px`。
- **内容块**：统一走 `.block`（`<details>` + `<summary>` + `▸` 箭头旋转展开），宽度恒 100%；搜索/思考/工具/结果块共用此模式。
- **用时折叠**：纯文本行 `.worked`（`用时 Xm YYs ›`），非卡片；展开后内部块与外部块同宽同层级，**不允许出现嵌套卡片**。
- **按钮体系**：`new-create`/`send-button`（主色实心）、`ghost-button`/`new-cancel`（描边）、`danger-button`（绯红警示）、`icon-ghost`（图标幽灵）、`chip`/`chip-toggle`（输入框工具行）。
- **浮层**：
  - 锚定弹层（`.new-panel`）：瞬态表单，点击外部/Esc 关闭；
  - 统计浮块（`.stats-panel`）：**非阻塞**——无遮罩，主界面可操作，仅按钮与 Esc 关闭；
  - 居中确认弹窗（`.modal-overlay` + `.confirm-modal`）：**仅用于破坏性操作**，带遮罩与模糊。
- **动效**：入场 `rise`（分组列表 stagger 45ms、上限 8 组）、`blink` 品牌光标、`pulse` 状态点；微交互 `transition 0.15s`。不加大型/装饰性动画。
- **图标**：内联 SVG（stroke 2、round），不引入图标库；颜色继承 `currentColor`。

## 5. 工程契约

- **构建**：`cargo build` 经 `build.rs` 自动 `npm ci && npm run build`，产物 `dist/` 由 rust_embed 打包进二进制；`dist/` 与 `node_modules/` 不入库。
- **类型契约**：`src/types.ts` 必须与后端 `src/model/*` 的 serde 序列化格式一一镜像；新增后端字段同步更新。
- **API 层**：所有请求经 `src/api.ts` 的 `request()`（统一错误提取与空响应处理），不得在组件内裸写 fetch。
- **错误反馈**：请求失败走 App 顶部 error banner；后端 Notice 走 notice banner。
- **验证**：改动后 `npm run build`（含 `tsc --noEmit`）+ `cargo build`；视觉变更用 headless Chromium 截图验证浅/深双主题。
