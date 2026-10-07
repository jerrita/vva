# vva — Vendor-Versatile Agent Harness

vva 是一个终端 AI 编程助手：用自然语言描述任务，模型在你的项目里读写文件、执行命令、联网查资料，直到完成任务。支持 OpenAI、Anthropic、Google 及任意兼容端点，会话自动保存，可随时恢复继续。

## 安装

从源码构建（需要 Rust 1.85+；构建期需要 Node.js 与 npm，前端会打包进二进制，运行时无需 Node）：

```bash
git clone <仓库地址>
cd vva
cargo install --path .
```

## 配置

写一份 `~/.vva/config.jsonc`，声明模型与密钥：

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

只有官方 API Key 时可以不写配置文件，直接设置 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY` 或 `GEMINI_API_KEY` 即可使用。

在配置文件首行加 `"$schema"` 指向仓库根目录的 [schema.json](schema.json)，编辑器可获得字段补全与校验；字段说明与更多玩法（自定义供应商、模型协议覆写、推理档位、费率）见 [配置参考](docs/config-reference.md)。

## 使用

### 终端界面

```bash
vva
```

进入全屏交互界面：输入任务后模型自动调用工具干活，聊天流支持 Markdown 渲染与图片/PDF 附件，右侧看板实时显示 token 用量与成本。常用命令：

| 命令 | 作用 |
|---|---|
| `/new` | 新建会话 |
| `/sessions` | 切换历史会话 |
| `/model` | 切换模型 |
| `/variant` | 切换推理档位 |
| `/search` | 开关联网搜索 |
| `/team` | 开关 team 模式（subagent 派生与跨会话消息，默认开启） |
| `/help` | 查看全部命令 |

### 单次执行

```bash
vva run "梳理 src/harness 的调用链并总结"
```

结果直接流式打印到终端，适合脚本与一次性任务。可用 `--model`、`--variant`、`--web-search` 调整本次行为，`-h` 查看全部参数。

### 浏览器界面

```bash
vva web --port 3000
```

打开 <http://127.0.0.1:3000>，功能与终端界面一致，另支持按项目分组管理会话、消息回退与分叉、粘贴图片。

### 会话管理

会话按项目自动存放在 `~/.vva/sessions/`，跨目录也能找回：

```bash
vva session list            # 列出全部会话
vva session inspect <id>    # 查看某次会话的完整记录
```

## 文档

- [配置参考](docs/config-reference.md)
- [设计文档](docs/README.md)（架构、领域模型、存储引擎、供应商适配）
