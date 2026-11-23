# 配置参考

## 1. 配置文件加载

### 1.1 查找顺序

一次启动加载至多一份全局配置与至多一份项目配置，项目配置优先。

| 类型 | 查找顺序，取第一个命中的 |
|---|---|
| 项目配置 | CLI 参数 `--config <path>` → 环境变量 `VVA_CONFIG` → 当前目录下的 `vva.jsonc` → `vva.json` → `.vva/config.jsonc` → `.vva/config.json` |
| 全局配置 | `~/.vva/config.jsonc` → `~/.vva/config.json` |

`--config` 与 `VVA_CONFIG` 的路径支持 `~` 前缀，文件不存在时报错。两者都未找到时进入[环境变量直连](#14-环境变量直连)。

### 1.2 合并规则

项目配置叠加在全局配置之上：

| 对象 | 规则 |
|---|---|
| `settings.default` | 项目声明非空才覆写 |
| `settings.small_model` / `settings.editor` | 项目声明才覆写 |
| `settings.web` / `settings.agent` | 项目声明则整段替换 |
| 已有 provider 的 `type` | 项目声明才覆写，省略时保持全局的协议 |
| 已有 provider 的 `base_url` / `api_key` | 项目声明非空才覆写 |
| `models` | 按 `model_id` 逐个合并：项目覆盖同名模型，加入其余模型 |
| 新 provider | 整体加入 |

### 1.3 语法

- **JSON / JSONC**：支持 `//` 行注释与 `/* ... */` 块注释，字符串字面量内的注释符号不受影响。
- **环境变量插值**：`${VAR}` 展开为环境变量的值，未设置时为空字符串。

### 1.4 环境变量直连

未找到任何配置文件时，按标准环境变量构造官方端点的供应商：

| 环境变量 | provider id | 协议 |
|---|---|---|
| `OPENAI_API_KEY` | `openai` | response |
| `ANTHROPIC_API_KEY` | `anthropic` | anthropic |
| `GEMINI_API_KEY` | `gemini` | gemini |

此模式下默认模型为 `openai/gpt-5.6-terra`。供应商未声明 `models`，任意模型名都可直接使用，例如 `vva run --model anthropic/claude-sonnet-4-5`。

### 1.5 env 文件

| 文件 | 生效方式 |
|---|---|
| 全局 `~/.vva/env` | 进程启动时写入进程环境，已存在的变量不覆盖；参与配置文件的 `${VAR}` 插值 |
| 项目 `<workspace>/.vva/env` | 每条 Shell 命令（前台与 `run_in_background`）执行前读取并注入子进程，覆盖同名的进程变量。修改即时生效，`vva web` 的多个工作区互不串扰 |

格式是 dotenv 子集：`KEY=VALUE`、`#` 注释、可选的 `export ` 前缀。

- 双引号值支持 `\n` `\t` `\"` `\\` 转义，单引号值原样保留。
- 未加引号与双引号的值展开 `${VAR}`：先查本文件已定义的键，再查进程环境。
- 非法行、非法键名、引号未闭合时报错并带行号。全局文件出错使启动失败，项目文件出错使该条命令失败。

## 2. settings

```jsonc
{
  "settings": {
    "default": "a/gpt-5.6-terra",     // 默认模型规格 {provider_id}/{model_id}
    "small_model": "a/gpt-5.6-luna",  // 生成会话标题的小模型，省略时用主模型
    "editor": "patch",                // 默认文件编辑协议：replace | patch | hashline
    "web": { /* 外置搜索与抓取，见 2.1 */ },
    "agent": { /* Agent Space，见 2.2 */ }
  },
  "providers": {
    "<provider_id>": { /* 见 3 */ }
  }
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `default` | string | 是 | 默认模型规格；未传 `--model` 且 `state.json` 不可用时使用 |
| `small_model` | string | 否 | 生成会话标题的模型规格；省略时用主模型 |
| `editor` | string | 否 | 模型未声明 `annotation.editor` 时，新会话采用的编辑协议；默认 `patch` |
| `web` | object | 否 | 外置搜索与抓取，见 2.1 |
| `agent` | object | 否 | Agent Space 配置，见 2.2；`vva agent` 要求声明 |

### 2.1 settings.web 外置搜索

```jsonc
"web": {
  "provider": {
    "zhipu": {
      "api_key": "${ZHIPU_API_KEY}",
      "search_engine": "search_std" // 可选；search_mcp 使用 Coding Plan Search MCP
    },
    "deepseek": {
      "api_key": "${DEEPSEEK_API_KEY}",
      "api_base": "https://api.deepseek.com" // 可选，省略时使用此地址
    }, // DeepSeek，search + fetch
    "brave":  { "api_key": "${BRAVE_API_KEY}" },   // Brave Search LLM Context，仅 search
    "tavily": { "api_key": "${TAVILY_API_KEY}" },  // Tavily，search + fetch
    "google": { "model": "homo/gemini-3.8-flash" } // 已配置的 Gemini 模型，仅 search
  },
  "search": "brave",   // 搜索使用的 provider 名
  "fetch": "local"     // 抓取使用的 provider 名；local 为内置，无需声明
}
```

| provider | 能力 | 说明 |
|---|---|---|
| `deepseek` | search + fetch | DeepSeek Search / Fetch API。搜索返回面向模型的 summary，抓取返回 Markdown |
| `brave` | search | Brave LLM Context |
| `zhipu` | search | 智谱 Web Search API / Coding Plan Search MCP |
| `tavily` | search + fetch | Tavily |
| `google` | search + fetch | `model` 指向已配置的 Gemini 模型，见下 |
| `local` | fetch | 内置：HTTP 抓取加 Readability 式正文提取，无需声明 |

| 字段 | 取值 |
|---|---|
| `search` | `brave` / `tavily` / `google` / `deepseek` / `zhipu` |
| `fetch` | `local` / `tavily` / `google` / `deepseek` |

**google**：search 请求只挂载原生 `google_search`，来源取自 grounding，片段为被引用的文本段。fetch 请求挂载 `url_context` 并同挂 `google_search`，正文为模型据页面给出的 Markdown 转述。

**DeepSeek `api_base`**：根地址。去掉末尾 `/` 后拼接 `/v1/tools/search` 或 `/v1/tools/fetch`，自定义地址的路径前缀保留：`https://example.com/proxy/` 对应 `https://example.com/proxy/v1/tools/search`。

**智谱 `search_engine`**：默认 `search_std`，可选 `search_pro`、`search_pro_sogou`、`search_pro_quark`、`search_mcp`。

- 前四项调用 [Web Search API](https://docs.bigmodel.cn/api-reference/工具-api/网络搜索)。结果条数由工具参数 `max_results` 控制：搜狗按 10 的倍数请求，夸克由服务端决定条数，最终都按 `max_results` 截取。
- `search_mcp` 调用 [Coding Plan 联网搜索 MCP](https://docs.bigmodel.cn/cn/coding-plan/mcp/search-mcp-server)，需要有对应权限的 Coding Plan Key。它使用服务端声明的搜索工具，结果在本地按 `max_results` 截取。
- 搜索总超时 60 秒，MCP 出错直接报错。

**何时生效**

- 模型缺少原生能力（`annotation.web_search: false`、`annotation.web_fetch: false`，或协议本身未提供）、会话 web 开关开启、且对应位配置了外置 provider 时，注册同名的客户端工具 `web_search` / `web_fetch`。
- 原生能力在位时优先使用原生，外置工具不注册。Responses 的原生 `web_search` 含 `open_page` / `find_in_page`，视为 fetch 在位。
- 接管后端在状态栏标注：TUI 状态行显示 `web:<search>+<fetch>`，Web 输入区的「搜索」chip 标题同样标注。
- `settings.web` 未配置时，搜索选项报「被忽略」通知。
- 选中的 provider 不具备对应能力、未声明或密钥缺失时，会话启动报错。

### 2.2 settings.agent

```jsonc
"agent": {
  "model": "a/gpt-5.6-terra",
  "curator_model": "a/gpt-5.6-luna", // 可选：记忆整理使用的独立模型
  "ttl_secs": 86400,
  "channels": {
    "qq": { "app_id": "123456", "secret": "${QQ_BOT_SECRET}", "allowed_users": ["<openid>"] }
  },
  // 可选：L1 路由改用 Decisions API，密钥取自 providers.openrouter.api_key
  "router": { "decisions": { "provider": "openrouter", "model": "typesafe/jev-1.13", "min_confidence": 0.6 } }
}
```

- 省略的字段取默认值。时长为整数秒，预留为整数 token。
- `router` 默认复用 `settings.small_model`；声明 `router.decisions` 时改用决策 API。
- 项目配置声明 `agent` 时整段替换全局的 `agent`。

| 字段 | 默认 | 说明 |
|---|---|---|
| `model` | `settings.default` | 普通会话使用的模型规格 |
| `curator_model` | 当前 agent 模式模型 | curator 使用的独立模型规格，见下 |
| `ttl_secs` | `86400`（24h） | 会话未指定 TTL 时的空闲清理时长，见 [TTL 清理](agent.md#41-ttl-清理) |
| `rollover_reserve_tokens` | `32000` | 上下文达到 `limit.context` 减去该值时，回合结束触发 rollover |
| `pending_files_ttl_secs` | `600` | 待附件缓冲的等待时长 |
| `task_timeout_secs` | `600` | 定时任务单次执行的超时，超时中止并计为失败 |
| `task_max_failures` | `3` | 定时任务连续失败多少次后自动停用 |
| `progress_interval_secs` | `300` | 长轮次的进度摘要间隔，`0` 关闭；见 [进度汇报](agent.md#8-进度汇报) |
| `router.decisions` | 无 | L1 决策 API，字段见下表 |
| `channels.qq.app_id` / `secret` | 无 | QQ 机器人凭据。声明了 qq 而任一为空（如 `${VAR}` 未设置）时启动报错 |
| `channels.qq.allowed_users` | `[]` | 用户白名单，白名单外静默丢弃。空表示拒绝所有人，含 `"*"` 表示允许任何用户 |

`curator_model` 省略时跟随 agent 模式模型，包括 `model.json` 与运行期切换。配置后在启动时校验，不受 `/model` 影响。

`router.decisions` 的字段：

| 字段 | 说明 |
|---|---|
| `type` | 可选，`openrouter` / `opencode`。省略时，`provider` 是 `opencode` 或 `openrouter` 则取对应服务，其余取 `openrouter` |
| `provider` | 提供 `api_key` 的 `providers.<provider>` 节点名 |
| `model` | 服务原生的模型 ID |
| `min_confidence` | 默认 `0.6`，取值 0–1。`action` / `focus` / `ttl` 题的置信度低于此值时续接 focus；无 focus 时报错 |
| `endpoint` | 可选，覆盖服务的默认端点 |

两种服务共用 `state` + `questions` 请求与 `answers` + `usage` 响应的转换，都用 Bearer 鉴权。声明 `router.decisions` 后不再要求 `small_model`。

- **OpenRouter Decisions**（alpha）：默认端点 `https://openrouter.ai/api/alpha/decisions`，模型如 `typesafe/jev-1.13`。
- **OpenCode System One**：默认端点 `https://opencode.ai/inference/systemone/v1/systemone`，支持 `jev-1.13` 与 `jev-1.13-free`。参见 [OpenCode 文档](https://opencode.ai/v2/docs/console/models/#endpoints)。

OpenCode 配置示例，与已有的主模型和渠道配置合并：

```jsonc
{
  "providers": {
    "opencode": { "api_key": "${OPENCODE_API_KEY}" }
  },
  "settings": {
    "agent": {
      "router": {
        "decisions": {
          "provider": "opencode",
          "model": "jev-1.13",
          "min_confidence": 0.6
        }
      }
    }
  }
}
```

## 3. providers

```jsonc
{
  "providers": {
    "a": {
      "type": "response",              // 可选：response | anthropic | gemini，默认 response
      "base_url": "https://gw.example.com", // 可选，默认用官方端点
      "api_key": "${MY_API_KEY}",      // 明文或 ${VAR} 插值
      "models": { /* 见 4 */ }
    }
  }
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | string | 协议：`response`（OpenAI Responses）、`anthropic`（Anthropic Messages）、`gemini`（Google Gemini）。默认 `response` |
| `base_url` | string | API 端点前缀，默认取官方端点 |
| `api_key` | string | 鉴权密钥，推荐用 `${VAR}` 插值 |
| `models` | object | 模型清单，`model_id` → 模型配置，见 4。可整体省略 |

### 3.1 base_url 规范化

端点按协议规范化：去掉结尾多余的 `/`，未带版本后缀时自动补全。

| type | 默认端点 | 自动补全的后缀 |
|---|---|---|
| `response` | `https://api.openai.com/v1` | `/v1`；已以 `/v1` 或 `/responses` 结尾则原样 |
| `anthropic` | `https://api.anthropic.com/v1` | `/v1`；已以 `/v1` 或 `/messages` 结尾则原样 |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta` | `/v1beta`；已以 `/v1beta`、`/v1` 结尾或含 `/models` 则原样 |

### 3.2 省略 models

`models` 省略或为空时，该供应商下任意模型名都可直接使用，模型按协议默认参数调用。环境变量直连（1.4）与 glm-coding-plan 预设（6）的节点属于这种情况。

## 4. models

```jsonc
"models": {
  "gpt-5.6-terra": {
    "name": "GPT-5.6 Terra",                  // 显示名，必填
    "type": "anthropic",                      // 可选：模型级协议覆写
    "modalities": { "input": ["text", "image", "pdf"], "output": ["text"] },
    "reasoning": true,
    "limit": { "context": 400000, "output": 128000 },
    "variants": ["minimal", "low", "medium", "high"],
    "cost": {
      "input": 2, "output": 12,
      "cache_read": 0.2, "cache_write": 2.5,
      "context_over_200k": { "input": 4, "output": 18, "cache_read": 0.4 }
    },
    "annotation": { "web_fetch": false, "editor": "replace" }
  }
}
```

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `name` | string | 必填 | 显示名 |
| `type` | string | 继承 provider | 模型级协议覆写，用于统一反代网关下个别模型使用不同协议 |
| `modalities.input` | string[] | `[]` | 输入模态：`text` / `image` / `video` / `pdf`。提交附件时与协议能力双重校验 |
| `modalities.output` | string[] | `[]` | 输出模态 |
| `reasoning` | bool | `false` | 是否为推理模型 |
| `limit.context` | int | — | 上下文窗口上限（tokens），用于统计面板的比例展示 |
| `limit.output` | int | — | 单轮输出上限（tokens） |
| `variants` | string[] | — | 推理档位白名单，常用值 `minimal` / `low` / `medium` / `high` / `xhigh` / `max` |
| `cost` | object | — | 费率，见 4.1 |
| `annotation` | object | — | 模型级注记，见 4.2 |

### 4.1 cost 费率

单位为美元 / 百万 tokens，用于成本估算。

| 字段 | 说明 |
|---|---|
| `input` / `output` | 普通 input / output token 的费率 |
| `cache_read` | 缓存读取费率 |
| `cache_write` | 缓存写入费率 |
| `context_over_200k` | 上下文超过 200k tokens 时整体替换费率：`input` / `output` / `cache_read` / `cache_write`，cache 字段省略时取基础费率 |

### 4.2 annotation 注记

| 字段 | 默认 | 说明 |
|---|---|---|
| `annotation.websocket` | `true` | Responses：使用 WebSocket |
| `annotation.web_fetch` | `true` | 向端点声明 `web_fetch` 服务端工具 |
| `annotation.web_search` | `true` | 向端点声明原生 web search |
| `annotation.standalone_search` | `false` | Anthropic：搜索改为独立子请求 |
| `annotation.affinity` | provider 节点 id | 模型亲和组 |
| `annotation.editor` | `settings.editor` | 文件编辑协议 |
| `annotation.web_tool_version` | `20260318` | Anthropic：服务端 web 工具的版本后缀 |
| `annotation.fast` | `false` | 请求体携带 `"service_tier": "fast"` |

- **`websocket`**：
  - 设为 `false` 时使用 HTTP JSON / SSE。
  - 不支持 WebSocket 的端点需关闭；连接失败时不切换传输。
  - 续接与重建规则见 [providers §2.1](providers.md#21-传输与上下文续接)。
- **`web_fetch`**：
  - 设为 `false` 时不声明 `web_fetch`。
  - 用于未接线该 server tool 的中转端点：工具声明组中出现端点未识别的条目时，端点会整组丢弃声明，只声明 `web_search` 可保住原生搜索。
  - 原生 fetch 缺位后，配置了 `settings.web.fetch` 时由客户端工具补全。
- **`web_search`**：
  - 设为 `false` 表示模型或端点不支持原生搜索，不向端点声明。
  - 配置了 `settings.web.search` 时由客户端工具 `web_search` 补全。
- **`standalone_search`**：适用于 Kimi 等搜索调用 ID 与结果 ID 不一致的端点。
  - 联网开启且允许搜索时，经 core 的 `StandaloneSearchTool::install` 注册客户端 `web_search`，关闭主请求的原生搜索声明，用当前模型和端点独立检索。
  - 优先于外置搜索 provider，受 `annotation.web_search` 约束。
  - 细节见 [providers §3.4](providers.md#34-独立搜索)。
- **`affinity`**：
  - 同组模型切换时互相复用原生回传状态（response_id 链、encrypted thinking、签名块）。
  - 跨组切换时历史按通用方式重建：推理签名丢弃，搜索调用按当前工具面重新投影。此时发提示。
  - 混系网关（同一节点下有多个模型家族）按模型标注，如 `"gpt"`。
- **`editor`**：
  - 决定注册的文件工具集，见[领域模型 §1.1](domain_model.md#11-session)。
  - 会话创建时确定并存入应用记录，之后换模不改变。
  - 自定义 agent 的 `tools` 白名单写具体工具名。
- **`web_tool_version`**：Anthropic 协议下 `web_search_<版本>` / `web_fetch_<版本>` 服务端工具的版本后缀，原生与独立搜索共用。
  - 用于尚未识别最新版本的中转端点（报 `unknown variant web_search_20260318`），可设为 `"20260209"`。
  - 独立搜索只向独立请求声明搜索工具；主请求的 fetch 由 `annotation.web_fetch` 控制。
- **`fast`**：设为 `true` 时请求体携带 `"service_tier": "fast"`（Responses API），走端点的快速档位。

例如 `kimi-for-coding`：

```json
"kimi-for-coding": {
  "name": "Kimi",
  "type": "anthropic",
  "annotation": {
    "standalone_search": true,
    "web_tool_version": "20250305",
    "web_fetch": false
  }
}
```

此例关闭主请求的原生 fetch 声明；配置了 `settings.web.fetch` 时，页面抓取由对应的客户端工具提供。

## 5. 模型寻址与推理档位

### 5.1 规格格式

- `{provider_id}/{model_id}`，如 `a/gpt-5.6-terra`。
- 裸 `{model_id}` 只在所有供应商的 `models` 中唯一命中时可用，多命中时报歧义错误。
- 默认供应商未声明 `models` 时（预设或环境变量直连），裸模型名按默认供应商解析。

### 5.2 档位校验与映射

`--variant` / `/variant` 传入的档位先按 `variants` 白名单校验，非法值报 `InvalidVariant` 并列出合法项。校验通过后映射为对应协议的官方字段：

- `response`：`reasoning.effort`。
- `anthropic`：`output_config.effort`，可用档位以[官方 Effort 文档](https://platform.claude.com/docs/en/build-with-claude/effort)的模型支持范围为准。
- `gemini`：`generationConfig.thinkingConfig.thinkingLevel`，并设置 `includeThoughts: true` 请求思考摘要；可用档位以[官方 Thinking 文档](https://ai.google.dev/gemini-api/docs/generate-content/thinking#controlling_thinking)的模型支持范围为准。Gemini 2.5 使用 `thinkingBudget`，不支持此档位参数。

`none` 关闭推理档位。模型声明了 `variants` 时，白名单包含 `none` 才可用。

### 5.3 档位记忆

`~/.vva/state.json` 记录上次使用的模型与档位（`last_model` / `last_variant`），切换模型、档位或会话时更新。下次启动未传 `--model` 时使用该选择，无法解析时回落 `settings.default`。

## 6. glm-coding-plan 预设

智谱 GLM Coding Plan 只需填写 `api_key`，其余字段自动补全：

```jsonc
{
  "providers": {
    "glm-coding-plan": {
      "api_key": "${ZAI_API_KEY}"
    }
  }
}
```

- **识别**：供应商 id 为 `glm-coding-plan`（`type` 省略或为 `response`），或任意 id 声明 `type: "glm-coding-plan"`。
- **补全**：协议 `response`；端点 `https://open.bigmodel.cn/api/v1`（Codex Responses 端点，原生 `web_search`）；模型 `glm-5.3` 与 `glm-5.3-flash`。已声明的字段与同名模型不被覆盖。

自定义节点 id 时声明 `type`：

```jsonc
{
  "providers": {
    "zhipu": {
      "type": "glm-coding-plan",
      "api_key": "${ZAI_API_KEY}"
    }
  }
}
```

## 7. 运行时文件布局

```
~/.vva/
├── config.jsonc            # 全局配置（或 config.json）
├── state.json              # 运行时偏好（last_model / last_variant）
├── vva.log                 # TUI 模式日志，级别由 RUST_LOG 控制
└── sessions/
    └── {project_key}/      # 按工作区路径编码的项目目录
        ├── {session_id}.jsonl
        ├── scratch/        # 项目便签
        └── tool_outputs/   # 大体积工具输出
```

## 8. 编辑器补全

仓库根目录的 [schema.json](../schema.json) 描述完整配置结构，含各字段说明与枚举取值。配置文件声明 `$schema` 指向它，编辑器即可提供字段补全、悬停说明与校验：

```jsonc
{
  "$schema": "/path/to/vva/schema.json", // 本地路径或远程 URL
  "settings": { /* ... */ }
}
```

`$schema` 在解析时忽略。
