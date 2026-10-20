> 此项目为个人玩具，目前你不应该期望能将其用于稳定的生产

<div align="center">
  <h1 align="center">
    <img src="frontend/public/favicon.png" width="200">
    <br/>
    Vendor-Versatile Agent
  </h1>
  <p align="center">用满每家模型原生能力的 AI 编程助手</p>
</div>

vva 是终端与浏览器中的 AI 编程助手：用自然语言描述任务，模型在你的项目里读写文件、执行命令、联网查资料，直到完成。

多数编程助手把所有模型压成同一个 Chat Completions 接口，缓存、思考签名、原生搜索等能力随之丢失。vva 为每家供应商实现原生协议：

- **OpenAI Responses**：`previous_response_id` 服务端链式续接，只上传增量上下文；
- **Anthropic Messages**：Prompt Caching 断点与 Thinking 签名完整回传，长会话持续命中缓存；
- **Google Gemini**：原生 Google Search Grounding，回答附带精确到片段的引用来源。

结果是更低的 token 成本、更快的响应，以及模型能力不打折扣。

## 特性

- **三种界面**：全屏终端界面、浏览器界面（含文件浏览与 IDE 布局）、单次命令行执行，共享同一套会话；
- **会话持久化**：按项目自动保存，随时恢复、回退或分叉；
- **用量透明**：实时显示 token、缓存命中与费用；
- **多模态**：`@` 引用工作区文件，图片与 PDF 作为原生附件发送；
- **可扩展**：支持 `AGENTS.md` 指令、[Agent Skills](https://agentskills.io)、自定义 subagent；
- **联网兜底**：模型无原生搜索时可接入 Brave / Tavily / Gemini / DeepSeek / 智谱搜索与网页抓取；
- **常驻 IM agent**：`vva agent` 接入 QQ，自动路由会话、沉淀长期记忆、执行定时任务。

## 安装

一键安装或更新到最新 release：

```bash
# Linux x86_64 / arm64、macOS Apple Silicon（arm64）
curl -fsSL https://raw.githubusercontent.com/jerrita/vva/master/scripts/install.sh | bash
```

```powershell
# Windows x64
irm https://raw.githubusercontent.com/jerrita/vva/master/scripts/install.ps1 | iex
```

从源码构建需要 Rust 1.85+；构建期需要 Node.js 与 npm（Web 前端打包进二进制，运行时无需 Node）。

```bash
cargo install --git https://github.com/jerrita/vva
```

## 快速开始

已有官方 API Key 时，设置环境变量即可直接使用：

```bash
export OPENAI_API_KEY=...
vva                                          # 默认模型 openai/gpt-5.6-terra

export ANTHROPIC_API_KEY=...                 # 或 GEMINI_API_KEY
vva run --model anthropic/claude-sonnet-4-5  # 以 供应商/模型 指定
```

使用其他供应商或兼容端点时，写一份 `~/.vva/config.jsonc`：

```jsonc
{
  "settings": { "default": "zai/glm-5.3" },
  "providers": {
    "zai": {
      "type": "glm-coding-plan", // 智谱预设：只需密钥，端点与模型自动补全
      "api_key": "${ZAI_API_KEY}"
    }
  }
}
```

自定义供应商、推理档位、费率、联网搜索等配置见 [配置参考](docs/config-reference.md)；配置文件声明 `"$schema"` 指向 [schema.json](schema.json) 可获得编辑器补全。

也可以把下面的 prompt 交给其他 coding agent，由它辅助完成配置：

```text
帮我配置 vva（https://github.com/jerrita/vva）。
先阅读配置参考与 schema：
- https://raw.githubusercontent.com/jerrita/vva/master/docs/config-reference.md
- https://raw.githubusercontent.com/jerrita/vva/master/schema.json

然后询问我要使用的供应商、端点、模型与是否需要联网搜索，据此生成 ~/.vva/config.jsonc：
- 文件开头声明 "$schema" 指向上面的 schema.json；
- API Key 以 "${ENV_VAR}" 引用，密钥写入 ~/.vva/env（KEY=VALUE 格式），不写进配置文件；
- 写入后运行 `vva run "hello"` 验证，报错时按配置参考修正。
```

## 使用

```bash
vva                                    # 终端界面
vva run "梳理 src/harness 的调用链"     # 单次执行，结果流式输出
vva web                                # 浏览器界面，http://127.0.0.1:3000
vva agent                              # 常驻 IM agent
```

终端界面中输入 `/help` 查看命令，`/model` 切换模型，`/sessions` 恢复历史会话。

## 文档

- [使用指南](docs/usage.md)：命令、参数、扩展机制与 Agent Space
- [配置参考](docs/config-reference.md)：配置文件全部字段
- [设计文档](docs/README.md)：架构、领域模型、存储引擎、供应商适配

## 许可证

[AGPL-3.0](LICENSE)
