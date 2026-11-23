<div align="center">
  <h1 align="center">
    <img src="frontend/public/favicon.png" width="200">
    <br/>
    Vendor-Versatile Agent
  </h1>
  <p align="center">用满每家模型原生能力的 AI 编程助手</p>
</div>

- **原生联网**：直接使用各家模型自带的搜索；模型无原生搜索时接入 Brave / Tavily / Gemini / DeepSeek / 智谱搜索与网页抓取；
- **三种界面**：全屏终端界面、浏览器界面（含文件浏览与 IDE 布局）、单次命令行执行，共享同一套会话；
- **跨供应商会话**：会话以统一格式存储，中途切换模型或供应商，上下文可接续；
- **常驻 IM agent**：`vva agent` 接入 QQ，自动路由会话、整理长期记忆、执行定时任务。


## 快速开始

把下面这段话发给任意 coding agent（Claude Code、Codex 等），由它询问需求并完成配置：

```text
阅读 https://raw.githubusercontent.com/jerrita/vva/master/docs/config-reference.md，
帮我配置 vva 的 ~/.vva/config.jsonc。
先问我要接入哪些供应商，再逐个询问所需信息，例如协议类型、端点、API Key 所在的环境变量、可用模型与默认模型。
配置完成后安装并执行 vva run "你好" 验证。
```

全部配置字段见[配置参考](docs/config-reference.md)。配置文件声明 `"$schema"` 指向 [schema.json](schema.json) 可获得编辑器补全。

## 安装

一键安装或更新到最新 release：

```bash
curl -fsSL https://raw.githubusercontent.com/jerrita/vva/master/scripts/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/jerrita/vva/master/scripts/install.ps1 | iex
```

默认安装到 `~/.local/bin`，Termux 为 `$PREFIX/bin`，Windows 为 `%LOCALAPPDATA%\vva\bin`，可用 `VVA_INSTALL_DIR` 修改。

Termux 先安装依赖：`pkg install bash curl tar coreutils ca-certificates`。

```bash
vva version   # 查看当前版本
vva update    # 更新到最新 release
```

### 从源码构建

需要 Rust 1.88+，构建期需要 Node.js 与 npm。

```bash
cargo install --git https://github.com/jerrita/vva
```

## 使用

```bash
vva                                    # 终端界面
vva run "总结这个项目的目录结构"       # 单次执行，结果流式输出
vva web                                # 浏览器界面，http://127.0.0.1:3000
vva agent                              # 常驻 IM agent
```

终端界面中输入 `/help` 查看命令，`/model` 切换模型，`/sessions` 恢复历史会话。

## 文档

- [使用指南](docs/usage.md)：命令、参数、扩展机制与 Agent Space
- [配置参考](docs/config-reference.md)：配置文件全部字段
- [设计文档](docs/README.md)：架构、领域模型、存储引擎、供应商适配

## Rust 集成

`crates/vva-core`（导入名 `vva_core`）可单独复用，默认不启用 feature；按需启用 `response` / `anthropic` / `gemini`，JSONL 存储启用 `jsonl`。用法见 [headless 示例](crates/vva-core/examples/headless.rs)与[架构文档](docs/architecture.md#4-外部消费与生命周期)。

## 许可证

[AGPL-3.0](LICENSE)
