# Changelog

## [1.7.0] - 2026-10-24

### 新增

- 新增 `vva version` 与 `vva update` 命令：打印当前版本，并以当前可执行文件所在目录调用官方安装脚本更新；安装前比对版本跳过已是最新，下载后校验二进制版本，替换经临时文件完成，运行中的旧进程不受影响。

### 改进

- 安装脚本改由 `releases/latest` 重定向解析 tag，避免 GitHub API 限额；网络请求统一重试，兼容 Windows PowerShell 5.1。
- 系统提示去掉品牌前缀；`agent_space` 模块更名为 `space`。
- 前端引入 ESLint 配置并修复告警。

### 修复

- Web 界面运行中的 Shell 工具行展示实际命令。
- 仅收到系统提醒、等待续接时，前次回答继续折叠在工作段，不提前拆出。

## [1.6.0] - 2026-10-21

### 新增

- 后台 Shell、后台 Agent 与跨会话消息的结果到达后自动唤醒空闲会话，忙时入队并在工具调用与回执配对后注入。

### 改进

- 移除 WaitSession 等待工具，模型无需调用工具等待完成；用户停止或模型调用失败后暂停自动续跑，后续结果仍更新工具块并持久化。
- 含后台工作的会话不受 TTL 清理；停止信号传播到仍有未完成工作的空闲子代理。

## [1.5.1] - 2026-10-20

### 改进

- Shell 工具参数说明明确 JSON 解码与 Bash 转义的边界，补充双引号内字面 `\n` 的行为与多行参数写法。

## [1.5.0] - 2026-10-20

### 新增

- 支持 Android arm64 / Termux，发布 `android-arm64` 安装包，安装脚本自动识别平台并默认安装到 `$PREFIX/bin`。

### 改进

- Android 主目录优先读取 `$HOME`，未设置时使用 `/data/data/com.termux/files/home`。
- 适配 Termux 原生 LLVM 工具链，Android WebSocket 使用 Rustls，消除 OpenSSL 构建依赖。
- 补充 Termux 安装与源码构建说明。

## [1.4.0] - 2026-10-20

### 新增

- 支持智谱搜索：可选 Web Search API（`search_std` / `search_pro` / `search_pro_sogou` / `search_pro_quark`）与 Coding Plan 联网搜索 MCP（`search_mcp`）。

## [1.3.1] - 2026-10-20

### 修复

- macOS ARM64 构建保留默认 CPU 特性，修复 `target-cpu=generic` 导致的 `ring` 编译断言失败。
- 一键安装脚本支持 macOS ARM64，下载对应平台发布包，并同步安装文档。

## [1.3.0] - 2026-10-20

### 新增

- 支持 DeepSeek 搜索与网页抓取，可配置 API 端点。
- 发布工作流增加 macOS ARM64 原生构建，提供 `macos-arm64.tar.gz` 发布包。

### 改进

- Web 与 TUI 在原工具块内展示后台任务状态和输出，并将任务查询结果归并到对应调用。
- 持久化后台任务快照，重新加载后保留任务结果，重启后将未完成任务标记为中断。
- 后台任务状态更新保留工具块展开状态及滚动位置。
