# 终端界面 (TUI)

基于 [Ratatui](https://github.com/ratatui/ratatui) 与 [Crossterm](https://github.com/crossterm-rs/crossterm)，代码位于 [`src/ui/`](../src/ui/)，目录结构见 [code_map](code_map.md)。

分为四个子系统：会话视图订阅（`app/`）、输入编辑（`input.rs`）、模态状态机（`modals/`）、纯渲染层（`views/`）。状态向下单向流动：`App` 持有状态，`views/` 只读渲染。

## 1. 会话视图订阅

[`src/harness/session/`](../src/harness/session/) · [`src/ui/app/session.rs`](../src/ui/app/session.rs)

- 会话读写归属 `SessionTask`，UI 只持有 `ManagedSession` 句柄，经命令信道提交请求；
- 前后台解耦：切换会话后后台任务继续运行，已完成轮次与流式草稿写入 `watch::Sender<Arc<SessionView>>`；切回时订阅 `watch::Receiver`，读到的快照即完整状态，无需回放事件；
- 流式增量按 33ms 节流发布（终端重绘 100ms），草稿写时复制，单个 token 只累积不复制整份草稿；
- 会话级与能力通知在 `refresh_view` 时按身份和内容变化投递到 toast 队列；轮次结果通知仍在所属轮次展示，未变化的快照不会让已消失的 toast 重现。

## 2. 渲染

`/blur` 切换背景透明度，并通过 toast 确认状态：默认 blur on，沿用终端背景；blur off 时以深灰蓝 `Rgb(20, 23, 29)` 铺满整帧，保留消息块、代码块、选区与 toast 的专用底色。背景在所有浮层绘制后补齐，弹窗与补全区域同样生效。该开关与侧边栏、思考展示模式一样仅保存在当前 TUI 进程中，切换会话时保留，重启后恢复默认。

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

### 2.5 通知 toast（`app/toast.rs`、`views/toast.rs`）

- 本地反馈（如“已复制”、操作错误）、会话通知与能力提示在右上角悬浮显示，距顶部 1 行、右侧 2 列，宽度随内容收缩，最多 60 列且不超过终端宽度减 6；窄屏自动折行；
- toast 在侧栏和模态弹窗之后绘制，独立背景与左右边框覆盖底层内容，不挤占聊天或输入区；覆盖区域不响应底层鼠标操作，也不携带底层聊天超链接；
- 按到达顺序每次显示一条，剩余数量显示为“+N 条通知”；每条从成为可见项起 5 秒后自动消失，空闲时同样生效；
- 信息 / 复制成功 / 能力警告 / 操作错误分别使用青 / 绿 / 黄 / 红色边框与级别标题；
- Esc 保持原有的弹窗、补全、待发消息与选区处理优先级；进入通知关闭分支时清空 toast 队列，并照常请求关闭会话级通知。提交输入或切换会话也清空当前队列，切入会话重新展示其通知；
- 待发消息和草稿附件数量仍在输入框上方展示。

## 3. 输入

### 3.1 `InputBuffer`（`input.rs`）

- 按字符索引编辑与移动，不在多字节字符中间切割；
- 粘贴时 `\r\n` / `\r` 归一为 `\n`；
- 上下移动光标时用 `char_idx_for_visual_width` 按终端显示宽度对齐 CJK 全角字符；
- 维护 `scroll_y` / `scroll_x`，内容或光标越界时调整视口。

### 3.2 事件路由与状态条（`events.rs`、`views/input_area.rs`）

输入分发顺序：弹窗独占 → 全局聊天浏览/档位/粘贴快捷键 → 聊天浏览 → 补全选单 → 普通编辑。

- 状态条回写 `model_selector_area` / `variant_selector_area` / `web_toggle_area` 命中矩形，鼠标按下按命中派发弹窗或开关；
- 输入框、聊天流与侧边栏的鼠标拖拽在释放时自动复制非空选区，保留选区，并通过 toast 显示“已复制”或剪贴板错误；普通单击保持组件原有动作，Ctrl+C 仍复制活跃选区并清除选区；
- 右键仍保留鼠标入口；每种动作及其他鼠标交互的键盘入口见下表；
- 档位清单由 `App::variant_options` 统一构造（default 置首），与 `/variant` 弹窗同源。

运行中的待发消息展示在输入框上方，选择态由 `App.selected_input` 按消息 ID 维护；
队列及模式由 `SessionTask` 管理，并经 `SessionView.pending_inputs` 发布。
按键语义见[运行中发送消息](usage.md#运行中发送消息)。

### 3.3 键盘聊天浏览与鼠标操作对照

`Ctrl+O` 在编辑与聊天浏览间切换，初次定位最新的已渲染块（可在屏幕外）；以青色 `▶` 标记焦点标题，输入框边框展示浏览键位。浏览时草稿、光标和待发消息保持不变，普通字符不写入草稿；弹窗仍独占输入。焦点按用户轮次、响应或组件 ID 保存，重排和流式追加后继续定位同一块；目标消失或切换会话时退出浏览。移动焦点或展开/收折会将标题滚入视口并暂停自动吸底，PgUp/PgDn 可继续阅读长块；退出后 End（输入为空时）回到底部并恢复自动跟随。

| 上下文 / 快捷键 | 行为 | 对应鼠标操作 |
|---|---|---|
| 无弹窗：Ctrl+O | 进入/退出聊天浏览，Esc 也可退出 | 无需鼠标定位消息/块 |
| 浏览：↑/↓、k/j、Shift+Tab/Tab | 上一个/下一个用户消息、响应、工具、思考或引用块；边界停止 | 移动鼠标定位组件 |
| 浏览：Home/End | 第一/最后一个块 | 滚动后定位 |
| 浏览：Enter、Space、→、l | 展开响应/思考/引用；工具隐藏→默认展示，截断预览→全文；用户消息打开 Revert/Fork 菜单（会话空闲时） | 左键展开组件、点击历史用户消息 |
| 浏览：←、h | 收折当前响应/思考/引用；Shell 全文→预览→隐藏，Edit/Write 全文→隐藏 | 右键收折工具正文、思考正文或历史响应；引用原为左键切换 |
| 浏览：y / Shift+Y | 复制当前块已渲染文本（含屏幕外行）/侧栏全文；收折内容需先展开再复制 | 拖拽聊天/侧栏文字后释放复制 |
| 无弹窗：Ctrl+T | 按模型声明顺序循环推理档位，末位回 default | 右键档位标签 |
| 无弹窗：Ctrl+V、Ctrl+Shift+V、Shift+Insert | 粘贴系统剪贴板到输入框；浏览时自动回到编辑 | 右键输入框 |
| 无弹窗：终端 Paste（macOS 通常 Cmd+V） | 终端发送 bracketed paste，整段插入并归一化换行，不提交；浏览时回到编辑 | 终端粘贴菜单 |
| 编辑或浏览：PgUp/PgDn；编辑且输入为空：Home/End | 翻页、到顶/到底 | 滚轮与滚动条拖拽 |
| /model、/variant、/search、/thinking | 模型/档位弹窗、联网/思考展示开关 | 左键状态条标签 |
| 弹窗：↑/↓、Enter | 选择并确认条目 | 点击列表条目 |

输入框用方向键定位光标；任意字符范围的鼠标拖选仍可使用，键盘复制提供整块/侧栏全文粒度。终端 OSC 8 链接的打开手势归终端管理，可复制包含 URL 的展开块后使用。`Cmd` 由终端处理，TUI 不绑定它；新增键位不需要 Option-as-Meta 或扩展键盘协议。现有 Ctrl+A/E、Ctrl+K/U、Ctrl+C 与补全键位保留；Ctrl+C 在浏览时仍遵循复制选区→清空输入→中止→退出的级联。

设计参考：[Claude Code](https://code.claude.com/docs/en/interactive-mode) 用 Ctrl+O 进入 transcript viewer；[OpenCode](https://opencode.ai/docs/keybinds/) 用 Ctrl+T 循环档位，并以 Ctrl+X leader 组织多种消息操作。vva 没有既有 leader，使用独立浏览状态，使单字母操作只在浏览时生效。

`/help` 列出完整键位，↑/↓、j/k、PgUp/PgDn、Home/End 滚动，Esc/q 关闭，小终端中也可查看全部条目。

### 3.4 `@` 文件引用

[`app/editor.rs`](../src/ui/app/editor.rs) · [`app/mention.rs`](../src/app/mention.rs) · [`find.rs`](../src/find.rs)

- **触发**：光标处 `@` 位于行首或空白 / 开括号（含全角）之后，token 止于空白或 CJK 标点；不与 `user@host` 冲突；
- **候选**：启动时后台线程 `walk_workspace`（遵循 .gitignore，与 Web go-to-file 共用）建索引，输入时 `rank_candidates` 取前 8 条（空查询按 mtime 降序）；浮层 ↑↓ / Ctrl-P/N 循环，Tab / Enter 插入 `@path `，Esc 关闭；
- **展开**：UI 只插入纯文本，展开在 harness `SessionTask::start`，TUI / Web / `vva run` 共用：文本文件读为 `ContentPart::FileRef`（128KB 截断标记），图片 / PDF 转原生附件并过输入能力校验，二进制 / 超限 / 能力缺失降级为文本注记，路径不存在发 Notice；
- **渲染**：`FileRef` 渲染为紧凑引用行（`📎 path · N 行`），Web 为可点击芯片。

## 4. 模态弹窗

`ActiveModal` 的每个变体直接携带状态，开启与否和状态存在互为同一事实。按键统一为「归一化 → 关闭 / `ModalAction` / 交给状态机」，确认动作由状态读出、调用方执行。状态、过滤与键盘导航的单测内聚在 `modals/`；`views/*_modal.rs` 只从状态取数渲染，两者单向依赖。

## 5. 后台工具块

后台 Shell / Agent 的生命周期和结果在原调用位置展示，详见 [background_tasks](background_tasks.md)。TUI 与 Web 共用 `SessionView.background_tasks` 结构化状态源。

右侧 Details 在会话基本信息之后显示 `Background Tasks` 分组，无任务时隐藏：

- 当前会话在本进程启动的后台 Shell 命令和脱离执行的 Agent 子代理共用此分组；短描述分别来自 Shell 的 `intent` 与 Agent 的 `description`，下一行显示 Shell / Agent、running / finished / failed 状态与耗时，停止和中断保留 stopped / interrupted 状态；
- 运行任务优先，各组按启动顺序排列；最多展示 3 项，超出时显示剩余数量。有限高度下优先展示基本信息与任务，后续详情按视口裁剪；
- 启动快照携带强类型 `BackgroundTaskActivity`（名称、类型、单调时钟起点），后续 `BackgroundTaskUpdated` 保留它，首次终态固定完成时刻。终态保留 5 秒后从侧栏移除，原工具块仍按领域生命周期保留；完成早于启动回执时只补齐展示信息，不回退运行态；
- 沿用 100ms tick / 重绘循环，主助手空闲时同样更新计时并清理到期展示；切换会话后计时继续。活动元信息仅驻留内存，不改变 JSONL 或 Web 协议，重启后历史任务不重新进入侧栏；
- Scheduler 属于 Agent Space daemon，未接入 TUI 的独立运行时；`SendMessage` 沿用跨会话交互与回复展示，不创建独立后台任务项。

分组与弱化元信息的样式参考 [OpenCode 侧栏](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/routes/session/sidebar.tsx)，Shell / 子代理集中查看的方式参考 [Claude Code 后台任务界面](https://code.claude.com/docs/en/interactive-mode)。
