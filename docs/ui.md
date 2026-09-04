# 终端用户界面架构设计 (Terminal UI Architecture)

## 1. 架构总览与设计目标

代码位置：[`src/ui/`](../src/ui/)

vva 终端界面基于 Rust 著名的 TUI 框架 [Ratatui](https://github.com/ratatui/ratatui) 与 [Crossterm](https://github.com/crossterm-rs/crossterm) 构建，致力于提供媲美 Claude Code / OpenCode 的极致现代化 Coding 交互体验。

---

## 2. 模块化解耦分层架构

经过模块化重构，`src/ui/` 严格遵循职责分离原则，分为五个高内聚子系统：

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
    ├── header.rs           # 顶部极简标题栏
    ├── chat.rs             # 中间对话流（气泡渲染、思考折叠、工具调用卡片）
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
  - **前后台完全解耦**：用户在一个会话中发起耗时任务后，可随时切换到其他会话查看历史或开启新任务，后台 Agent 继续利用 Tokio 独立任务运行并向广播信道发送事件；
  - 切换回原会话时，自动无缝订阅其专属的 `broadcast::Receiver<AgentEvent>`。

### 3.2 `InputBuffer`：独立的轻量多行文本编辑器
- **代码位置**：[`src/ui/input.rs`](../src/ui/input.rs)
- **核心功能**：
  - **多字节 Unicode 边界安全**：按字符索引而非字节偏移移动和编辑，杜绝在中文、Emoji 字符中间切割导致的 `panic`；
  - **多行粘贴与换行符归一化**：自动将 `\r\n` 与 `\r` 规整为 `\n`；
  - **全角 CJK 视觉宽度对齐**：通过 `char_idx_for_visual_width`，在多行上下移动光标时，精准计算全角字符的终端渲染宽度（Span width），确保光标垂直对齐；
  - **视口自适应平滑滚动**：维护 `scroll_y` 与 `scroll_x`，当内容或光标超出输入框可视区域时自动平滑推移。

### 3.3 `history.rs`：纯函数数据投影
- **代码位置**：[`src/ui/history.rs`](../src/ui/history.rs)
- **核心职责**：
  - `entries_from_turns`：将底层四层级联实体中的消息流提取、解构成适合 UI 呈现的平铺对话流列表（`ChatEntry::UserPrompt`, `Thinking`, `AssistantMessage`, `ToolCall`, `ToolResult`, `SystemNotice`）；
  - `compute_stats`：遍历各轮交互核算 Token 消耗、Anthropic 缓存命中/写入量，并结合模型单价估算美元花费。

### 3.4 模态弹窗状态机与渲染层彻底解耦
- **代码位置**：[`src/ui/modals/`](../src/ui/modals/) 与 [`src/ui/views/`](../src/ui/views/)
- 模态框的状态数据、过滤器逻辑与键盘导航单元测试完全内聚在 `modals/` 对应文件中；
- 视图渲染组件（`views/*_modal.rs`）仅负责从状态中提取数据并使用 Ratatui 部件呈现，两者单向依赖，职责边界高度清晰。
