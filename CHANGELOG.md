# Changelog

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
