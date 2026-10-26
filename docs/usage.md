# 使用指南

## 终端界面

```bash
vva                  # 在当前目录启动
vva run -w <目录>     # 指定工作区
vva run -s <id>      # 恢复指定会话
```

输入框中 `@路径` 引用工作区文件（Tab 补全），图片与 PDF 作为原生附件发送。斜杠命令：

| 命令 | 别名 | 作用 |
|---|---|---|
| `/new` | | 以当前配置新建会话 |
| `/sessions` | `/session` | 按项目选择并恢复会话 |
| `/model [规格]` | `/m` | 选择或切换模型 |
| `/variant [档位]` | `/v` `/effort` | 选择或切换推理档位 |
| `/search` | `/web` | 开关联网搜索 |
| `/thinking` | `/think` `/th` | 思考内容在单行摘要与全文间切换 |
| `/config` | `/settings` | 调整会话配置 |
| `/skills` | `/skill` | 浏览已发现的技能 |
| `/clear` | `/c` | 删除当前会话并新建 |
| `/sidebar` | `/sb` | 开关右侧统计看板 |
| `/help` | `/h` `/?` | 命令与快捷键帮助 |
| `/quit` | `/exit` `/q` | 退出 |

已安装的技能同时以 `/<技能名>` 出现在命令补全中。

### 运行中发送消息

运行中提交的消息默认进入 `after` 队列，当前执行正常结束后按提交顺序逐条开启下一轮。
`steer` 在下一次模型请求前插入引导，延续当前执行与计时；并行工具批次等待全部结果提交后再插入。
最终回复返回时仍有引导，则继续执行。待发内容属于进程运行态，实际消费时才写入会话历史；
停止或失败暂停自动消费，待发内容保留供取回编辑。

TUI 输入框为空时，↑ / ↓ 选择待发消息，默认选中最近一条。
Enter 将选中项从 `after` 提升为 `steer`；选中项已为 `steer` 时，向前寻找最近的 `after` 并提升。
Esc 将选中项从 `steer` 降为 `after`，再次 Esc 取回输入框并选中前一条。
取回正文放在现有草稿之前，用空行分隔，附件按相同顺序合并。无待发消息时 Esc 保留草稿；
停止执行使用 Ctrl+C（先复制选区、清空草稿，再停止）。

Web 运行中 Enter 或「排队」提交消息，在待发列表逐条选择「轮后发送 / 插入引导」或「取回编辑」。
停止执行使用独立的停止按钮。

## 单次执行

```bash
vva run "梳理 src/harness 的调用链并总结"
```

结果流式打印到终端。常用参数：

| 参数 | 作用 |
|---|---|
| `-m, --model` | 模型规格，如 `openai/gpt-5.6-terra` |
| `-v, --variant` | 推理档位：`minimal` / `low` / `medium` / `high` / `xhigh` / `max` |
| `--web-search` | 开启联网搜索 |
| `-s, --session-id` | 在已有会话上继续 |
| `-w, --workspace` | 工作区目录，默认当前目录 |
| `-c, --config` | 指定配置文件 |

## 浏览器界面

```bash
vva web --port 3000 [--host 127.0.0.1] [-w <目录>]
```

功能与终端界面一致，另提供：

- 按项目分组的会话列表，subagent 会话嵌套在父会话下；
- 消息回退与分叉、粘贴图片；
- 文件浏览器与 IDE 布局（`⌘/Ctrl+P` 跳转文件，`⌘/Ctrl+\` 切换布局），本会话改动行高亮；
- 在设置弹窗直接编辑全局配置，保存后热重载。

Web 界面无鉴权，暴露到非本机地址前请自行加反向代理与访问控制。

## 会话管理

会话以 JSONL 追加写保存在 `~/.vva/sessions/{项目}/`，进程中断不丢失已完成轮次。

```bash
vva session list            # 列出全部会话
vva session inspect <id>    # 查看完整记录
```

## 扩展

| 机制 | 位置 | 说明 |
|---|---|---|
| 指令文件 | `~/.vva/AGENTS.md`、项目根及子目录 `AGENTS.md` | 自动注入为用户指令 |
| 技能 | `~/.vva/skills`、`~/.agents/skills`、`.vva/skills`、`.agents/skills` | [Agent Skills](https://agentskills.io) 规范的 `SKILL.md`，按需加载全文 |
| Subagent | `~/.vva/agents`、`~/.agents/agents`、`.vva/agents`、`.agents/agents` | YAML 定义的 agent 类型；内置 `general` 与 `explore` |
| 环境变量 | `~/.vva/env`（启动时载入）、`.vva/env`（每条 shell 命令注入） | dotenv 格式 |

同名定义的优先级：项目 `.vva` > 项目 `.agents` > 全局 `.vva` > 全局 `.agents`。

## Agent Space（常驻 IM agent）

```bash
vva agent
```

在 IM 中与 agent 对话（目前支持 QQ 机器人），工作区固定为 `~/.vva/space/`。消息由小模型路由到合适的会话，空闲会话到期后整理为长期记忆（`memory/`，git 版本化），上下文接近上限时自动压缩续接；支持定时任务与文件收发。所有会话都是普通 vva 会话，可在终端或 Web 界面查看与接管。

配置见 [配置参考 §2.2](config-reference.md#22-settingsagent-常驻-im-agent)，在聊天中发送 `/help` 查看全部命令。设计细节见 [agent.md](agent.md)。
