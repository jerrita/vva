# 终端用户界面架构设计 (Terminal UI Architecture)

## 1. 架构总览与设计目标

代码位置：[`src/ui/`](../src/ui/)

vva 终端界面基于 Rust TUI 框架 [Ratatui](https://github.com/ratatui/ratatui) 与 [Crossterm](https://github.com/crossterm-rs/crossterm) 构建，提供 Coding 交互体验。

---

## 2. 模块化解耦分层架构

`src/ui/` 遵循职责分离原则，划分为四个子系统：会话视图订阅（`app/`）、输入编辑（`input.rs`）、模态状态机（`modals/`）与纯渲染层（`views/`）。

```
src/ui/
├── app/                    # App 响应式控制器、状态机与子系统调度
│   ├── mod.rs              # App 结构体定义、生命周期与统一导出
│   ├── session.rs          # 会话切换、SessionView 快照刷新与模态框状态调度
│   ├── editor.rs           # 输入缓冲编辑、光标移动与斜杠命令补全
│   ├── dispatch.rs         # 用户提交、斜杠命令路由与会话请求分发
│   ├── pane.rs             # TextPane 可选中文本面板（命中判定 / 选区 / 渲染行缓存）
│   ├── selection.rs        # 文本与输入选区模型 (TextCoord/TextSelection) 与剪贴板
│   ├── scroll.rs           # 视口滚动与坐标映射
│   ├── tests.rs            # App 完整自动化单元测试集
│   └── test_support.rs     # 测试共享基建 (临时 JSONL 存储 + App 构造)
├── command.rs              # 斜杠命令系统与前缀匹配补全引擎
├── input.rs                # InputBuffer 多行文本编辑与 CJK 宽字符光标算法
├── history.rs              # SidebarStats 纯函数统计投影
├── markdown.rs             # Markdown 解析与终端样式渲染
├── text.rs                 # 按显示宽度截断的文本工具 (CJK 双列)
├── modals/                 # 4 个独立的模态弹窗状态机
│   ├── mod.rs              # ActiveModal 枚举（变体携带状态）、ModalAction 与按键归一化
│   ├── sessions.rs         # SessionsModalState (按工程过滤会话、上下翻页)
│   ├── models.rs           # ModelsModalState (模型清单筛选、高亮)
│   ├── variants.rs         # VariantsModalState (推理档位切换)
│   └── config.rs           # ConfigModalState (设置项交互式切换)
└── views/                  # 纯渲染层组件（无状态业务逻辑）
    ├── header.rs           # 顶部标题栏
    ├── chat.rs             # 中间对话流（已完成轮次与流式草稿同源渲染、思考折叠、工具调用卡片、Markdown 渲染）
    ├── modal_shell.rs      # 模态弹窗公共外壳（居中布局、过滤行、滚动窗口、空态与提示行）
    ├── sidebar.rs          # 右侧详情边栏（宽屏自适应，Token与花费看板）
    ├── input_area.rs       # 复合输入区（状态条、命令建议浮层、多行输入框）
    ├── sessions_modal.rs   # /sessions 弹窗视图
    ├── models_modal.rs     # /model 弹窗视图
    ├── variants_modal.rs   # /variant 弹窗视图
    ├── config_modal.rs     # /config 弹窗视图
    └── help_modal.rs       # /help 快捷键与指令帮助弹窗
```

---

## 3. 核心子系统剖析

### 3.1 会话视图订阅与并发后台执行
- **代码位置**：[`src/harness/session.rs`](../src/harness/session.rs) 与 [`src/ui/app/session.rs`](../src/ui/app/session.rs)
- **核心职责**：
  - 会话的读写归属对应的会话任务（`SessionTask`），UI 只持有 `ManagedSession` 句柄并通过命令信道提交请求；
  - **前后台解耦**：用户切换会话后，后台任务继续运行，并把已完成轮次与流式草稿写入 `watch::Sender<Arc<SessionView>>`；
  - 切换回原会话时订阅其 `watch::Receiver`，读到的快照即当前完整状态，无需回放事件；
  - 流式增量按 33ms 节流发布（终端重绘为 100ms），单个 token 只累积不复制整份草稿；
  - 会话任务的通知在 `refresh_view` 时按到达顺序汇入界面通知流，界面只维护一份通知序列。

### 3.2 `TextPane`：可选中文本面板与行缓存
- **代码位置**：[`src/ui/app/pane.rs`](../src/ui/app/pane.rs)
- 聊天流与侧边栏是同一种结构：渲染行、命中矩形与选区共同构成 `TextPane`，鼠标交互按面板派发而不逐个枚举目标；
- 聊天流行缓存以 `ChatCacheKey`（视图 `revision`、渲染宽度、思考展示模式、通知条数）为失效依据，渲染与复制读同一份行；
- 渲染只把可视窗口交给部件，每帧工作量与视口高度相关，与历史长度无关。

### 3.3 `InputBuffer`：多行文本编辑器
- **代码位置**：[`src/ui/input.rs`](../src/ui/input.rs)
- **核心功能**：
  - **多字节 Unicode 边界处理**：按字符索引处理移动和编辑，避免在多字节字符中间切割引发 panic；
  - **多行粘贴与换行符归一化**：将 `\r\n` 与 `\r` 规整为 `\n`；
  - **全角 CJK 视觉宽度对齐**：通过 `char_idx_for_visual_width`，在多行上下移动光标时计算全角字符的终端渲染宽度（Span width），保持光标垂直对齐；
  - **视口滚动**：维护 `scroll_y` 与 `scroll_x`，当内容或光标超出可视区域时调整视口位置。

### 3.4 `history.rs`：纯函数数据投影
- **代码位置**：[`src/ui/history.rs`](../src/ui/history.rs)
- **核心职责**：
  - `compute_stats`：遍历各轮交互核算 Token 消耗、Anthropic 缓存命中/写入量，并结合模型单价估算美元花费。
- 对话内容不在此处二次建模：[`views/chat.rs`](../src/ui/views/chat.rs) 直接渲染 `SessionView` 中的 `Turn` 与 `draft: Vec<ContentPart>`，两者使用同一套内容类型。

### 3.5 模态弹窗状态机与渲染层解耦
- **代码位置**：[`src/ui/modals/`](../src/ui/modals/) 与 [`src/ui/views/`](../src/ui/views/)
- `ActiveModal` 的每个变体直接携带对应状态，弹窗是否开启与状态是否存在互为同一事实；
- 按键路由统一为「归一化 → 关闭 / `ModalAction` / 交给状态机」，确认动作由状态读出、由调用方执行；
- 模态框的状态数据、过滤器逻辑与键盘导航单元测试内聚在 `modals/` 对应文件中；
- 视图渲染组件（`views/*_modal.rs`）仅负责从状态中提取数据并使用 Ratatui 部件呈现，两者单向依赖，职责边界明确。

### 3.6 `markdown.rs` & `chat.rs`：Markdown 渲染与视口滚动引擎
- **代码位置**：[`src/ui/markdown.rs`](../src/ui/markdown.rs) 与 [`src/ui/views/chat.rs`](../src/ui/views/chat.rs)
- **核心功能**：
  - **CommonMark 渲染器**：基于 `pulldown-cmark` 构建渲染器，支持 `# / ## / ###` 徽标、代码块外框、Unicode 表格与列表悬挂缩进（Hanging Indent）；
  - **CJK 标点强调边界预处理**：对 `**标题：**内容` 等标点边界注入修正空格，使解析器正常闭合加粗标记；
  - **加粗文本样式**：加粗 Span 使用金黄色 (`Color::Rgb(255, 215, 80)`) 与粗体修饰；
  - **视口滚动控制**：`App` 维护行位坐标并限制越界，防止滚轮溢出；阅读历史时保持屏幕位置，返回底部后恢复自动跟随；
  - **滚动条组件**：基于 Ratatui `Scrollbar` 与 `ScrollbarState`，在内容超出可视区时在右侧渲染滚动滑块。
