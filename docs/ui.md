# 终端用户界面架构设计 (Terminal UI Architecture)

## 1. 架构总览与设计目标

代码位置：[`src/ui/`](../src/ui/)

vva 终端界面基于 Rust TUI 框架 [Ratatui](https://github.com/ratatui/ratatui) 与 [Crossterm](https://github.com/crossterm-rs/crossterm) 构建，提供 Coding 交互体验。

---

## 2. 模块化解耦分层架构

`src/ui/` 遵循职责分离原则，划分为五个子系统：

```
src/ui/
├── mod.rs                  # TUI 初始化、终端模式配置与主事件循环 (run_app_loop)
├── app.rs                  # App 核心控制器、Session 切换调度、斜杠命令分发
├── session_manager.rs      # 全局 Session 单例与并发后台运行管理器
├── command.rs              # 斜杠命令系统与前缀匹配补全引擎
├── input.rs                # InputBuffer 多行文本编辑与 CJK 宽字符光标算法
├── history.rs              # ChatEntry 对话流与 SidebarStats 纯函数视图投影
├── modals/                 # 4 个独立的模态弹窗状态机
│   ├── mod.rs              # ActiveModal 枚举及子模块统一导出
│   ├── sessions.rs         # SessionsModalState (按工程过滤会话、上下翻页)
│   ├── models.rs           # ModelsModalState (模型清单筛选、高亮)
│   ├── variants.rs         # VariantsModalState (推理档位切换)
│   └── config.rs           # ConfigModalState (设置项交互式切换)
└── views/                  # 纯渲染层组件（无状态业务逻辑）
    ├── header.rs           # 顶部标题栏
    ├── chat.rs             # 中间对话流（气泡渲染、思考折叠、工具调用卡片、Assistant 回复 Markdown 渲染）
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

### 3.1 单例 `SessionManager` 与并发后台执行
- **代码位置**：[`src/ui/session_manager.rs`](../src/ui/session_manager.rs)
- **核心职责**：
  - 维护内存中所有已加载的会话实例单例（`ManagedSession`），避免多实例写入冲突；
  - **前后台解耦**：用户切换会话后，后台 Agent 继续在独立 Tokio 任务中运行并向广播信道发送事件；
  - 切换回原会话时，订阅其专属的 `broadcast::Receiver<AgentEvent>`。

### 3.2 `InputBuffer`：多行文本编辑器
- **代码位置**：[`src/ui/input.rs`](../src/ui/input.rs)
- **核心功能**：
  - **多字节 Unicode 边界处理**：按字符索引处理移动和编辑，避免在多字节字符中间切割引发 panic；
  - **多行粘贴与换行符归一化**：将 `\r\n` 与 `\r` 规整为 `\n`；
  - **全角 CJK 视觉宽度对齐**：通过 `char_idx_for_visual_width`，在多行上下移动光标时计算全角字符的终端渲染宽度（Span width），保持光标垂直对齐；
  - **视口滚动**：维护 `scroll_y` 与 `scroll_x`，当内容或光标超出可视区域时调整视口位置。

### 3.3 `history.rs`：纯函数数据投影
- **代码位置**：[`src/ui/history.rs`](../src/ui/history.rs)
- **核心职责**：
  - `entries_from_turns`：将底层四层级联实体中的消息流提取、解构成适合 UI 呈现的平铺对话流列表（`ChatEntry::UserPrompt`, `Thinking`, `AssistantMessage`, `ToolCall`, `ToolResult`, `SystemNotice`）；
  - `compute_stats`：遍历各轮交互核算 Token 消耗、Anthropic 缓存命中/写入量，并结合模型单价估算美元花费。

### 3.4 模态弹窗状态机与渲染层解耦
- **代码位置**：[`src/ui/modals/`](../src/ui/modals/) 与 [`src/ui/views/`](../src/ui/views/)
- 模态框的状态数据、过滤器逻辑与键盘导航单元测试内聚在 `modals/` 对应文件中；
- 视图渲染组件（`views/*_modal.rs`）仅负责从状态中提取数据并使用 Ratatui 部件呈现，两者单向依赖，职责边界明确。

### 3.5 `markdown.rs` & `chat.rs`：Markdown 渲染与视口滚动引擎
- **代码位置**：[`src/ui/markdown.rs`](../src/ui/markdown.rs) 与 [`src/ui/views/chat.rs`](../src/ui/views/chat.rs)
- **核心功能**：
  - **CommonMark 渲染器**：基于 `pulldown-cmark` 构建渲染器，支持 `# / ## / ###` 徽标、代码块外框、Unicode 表格与列表悬挂缩进（Hanging Indent）；
  - **CJK 标点强调边界预处理**：对 `**标题：**内容` 等标点边界注入修正空格，使解析器正常闭合加粗标记；
  - **加粗文本样式**：加粗 Span 使用金黄色 (`Color::Rgb(255, 215, 80)`) 与粗体修饰；
  - **视口滚动控制**：`App` 维护行位坐标并限制越界，防止滚轮溢出；阅读历史时保持屏幕位置，返回底部后恢复自动跟随；
  - **滚动条组件**：基于 Ratatui `Scrollbar` 与 `ScrollbarState`，在内容超出可视区时在右侧渲染滚动滑块。
