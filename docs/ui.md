# 终端界面 (TUI)

基于 [Ratatui](https://github.com/ratatui/ratatui) 与 [Crossterm](https://github.com/crossterm-rs/crossterm)，代码位于 [`src/ui/`](../src/ui/)，目录结构见 [code_map](code_map.md)。

分为四个子系统：会话视图订阅（`app/`）、输入编辑（`input.rs`）、模态状态机（`modals/`）、纯渲染层（`views/`）。状态向下单向流动：`App` 持有状态，`views/` 只读渲染。

## 1. 会话视图订阅

[`src/harness/session/`](../src/harness/session/) · [`src/ui/app/session.rs`](../src/ui/app/session.rs)

- 会话读写归属 `SessionTask`，UI 只持有 `ManagedSession` 句柄，经命令信道提交请求；
- 前后台解耦：切换会话后后台任务继续运行，已完成轮次与流式草稿写入 `watch::Sender<Arc<SessionView>>`；切回时订阅 `watch::Receiver`，读到的快照即完整状态，无需回放事件；
- 流式增量按 33ms 节流发布（终端重绘 100ms），草稿写时复制，单个 token 只累积不复制整份草稿；
- 会话通知在 `refresh_view` 时按到达顺序汇入界面通知流，界面只维护一份序列。

## 2. 渲染

### 2.1 `TextPane` 与行缓存（`app/pane.rs`、`app/draft.rs`）

聊天流与侧边栏同为 `TextPane`：渲染行、命中矩形与选区共同构成，鼠标交互按面板派发。

- `ChatCache` 记录同一行缓冲中的历史前缀边界与思考分组状态：历史随会话快照、宽度或思考展示模式变化重建，草稿与通知随视图 `revision` 或通知条数变化重建；渲染与复制读同一份行；
- 草稿按部件身份对齐已渲染前缀，前缀部件不重渲染；
- 只把可视窗口交给部件，每帧工作量与视口高度相关，与历史长度无关。

### 2.2 聊天流（`views/chat/`）

已完成轮次与流式草稿同源渲染，直接读 `SessionView` 中的 `Turn` 与 `draft: Vec<ContentPart>`，不做二次建模。`history.rs` 的 `compute_stats` 为侧栏投影 Token、缓存命中 / 写入量与按模型单价估算的花费。

### 2.3 Markdown（`markdown.rs`）

基于 `pulldown-cmark`：标题徽标、代码块外框、Unicode 表格、列表悬挂缩进；对 `**标题：**内容` 等 CJK 标点边界注入修正空格使加粗正常闭合；加粗使用金黄色 `Rgb(255, 215, 80)`。

### 2.4 滚动

`App` 维护行位坐标并限制越界；阅读历史时保持屏幕位置，回到底部后恢复自动跟随；内容超出时右侧渲染 `Scrollbar`。

## 3. 输入

### 3.1 `InputBuffer`（`input.rs`）

- 按字符索引编辑与移动，不在多字节字符中间切割；
- 粘贴时 `\r\n` / `\r` 归一为 `\n`；
- 上下移动光标时用 `char_idx_for_visual_width` 按终端显示宽度对齐 CJK 全角字符；
- 维护 `scroll_y` / `scroll_x`，内容或光标越界时调整视口。

### 3.2 事件路由与状态条（`events.rs`、`views/input_area.rs`）

输入分发顺序：弹窗独占 → 补全选单 → 普通编辑。

- 状态条回写 `model_selector_area` / `variant_selector_area` / `web_toggle_area` 命中矩形，鼠标按下按命中派发弹窗或开关；
- 右键级联：复制活跃选区 → 命中档位标签时循环切换档位（`App::cycle_variant`，按模型声明顺序，越过末位回 default）→ 其余区域粘贴；
- 档位清单由 `App::variant_options` 统一构造（default 置首），与 `/variant` 弹窗同源。

运行中的待发消息展示在输入框上方，选择态由 `App.selected_input` 按消息 ID 维护；
队列及模式由 `SessionTask` 管理，并经 `SessionView.pending_inputs` 发布。
按键语义见[运行中发送消息](usage.md#运行中发送消息)。

### 3.3 `@` 文件引用

[`app/editor.rs`](../src/ui/app/editor.rs) · [`harness/mention.rs`](../src/harness/mention.rs) · [`find.rs`](../src/find.rs)

- **触发**：光标处 `@` 位于行首或空白 / 开括号（含全角）之后，token 止于空白或 CJK 标点；不与 `user@host` 冲突；
- **候选**：启动时后台线程 `walk_workspace`（遵循 .gitignore，与 Web go-to-file 共用）建索引，输入时 `rank_candidates` 取前 8 条（空查询按 mtime 降序）；浮层 ↑↓ / Ctrl-P/N 循环，Tab / Enter 插入 `@path `，Esc 关闭；
- **展开**：UI 只插入纯文本，展开在 harness `SessionTask::start`，TUI / Web / `vva run` 共用：文本文件读为 `ContentPart::FileRef`（128KB 截断标记），图片 / PDF 转原生附件并过输入能力校验，二进制 / 超限 / 能力缺失降级为文本注记，路径不存在发 Notice；
- **渲染**：`FileRef` 渲染为紧凑引用行（`📎 path · N 行`），Web 为可点击芯片。

## 4. 模态弹窗

`ActiveModal` 的每个变体直接携带状态，开启与否和状态存在互为同一事实。按键统一为「归一化 → 关闭 / `ModalAction` / 交给状态机」，确认动作由状态读出、调用方执行。状态、过滤与键盘导航的单测内聚在 `modals/`；`views/*_modal.rs` 只从状态取数渲染，两者单向依赖。

## 5. 后台工具块

后台 Shell / Agent 的生命周期和结果在原调用位置展示，详见 [background_tasks](background_tasks.md)。TUI 与 Web 共用 `SessionView.background_tasks` 结构化状态源。
