# 配置参考 (Configuration Reference)

本文档是 vva 配置系统的完整用户参考。设计原理与实现细节见 [configuration.md](configuration.md)。

---

## 1. 配置文件加载

### 1.1 查找顺序

一次启动会加载**全局配置**与**项目配置**各至多一份，项目配置优先级高于全局配置：

1. **项目配置**（按以下顺序取第一个命中的）：
   - CLI 参数 `--config <path>`（支持 `~` 前缀，文件缺失时报错）；
   - 环境变量 `VVA_CONFIG` 指定的路径（支持 `~` 前缀，路径不存在时报错）；
   - 当前目录下的 `vva.jsonc` → `vva.json` → `.vva/config.jsonc` → `.vva/config.json`。
2. **全局配置**：`~/.vva/config.jsonc` → `~/.vva/config.json`。

两者均未找到时，进入环境变量直连模式（见 1.4）。

### 1.2 合并规则

项目配置叠加在全局配置之上，按字段粒度合并：

| 对象 | 规则 |
|---|---|
| `settings.default` | 项目声明非空才覆写 |
| `settings.small_model` | 项目声明才覆写 |
| `settings.editor` | 项目声明才覆写 |
| 已有 provider 的 `base_url` / `api_key` | 项目声明非空才覆写 |
| 已有 provider 的 `type` | 仅项目**显式声明**才覆写（防止省略时被默认值翻转协议） |
| `models` | 按 `model_id` 逐个合并：项目覆盖同名模型、新增新模型 |
| 新 provider | 整体加入 |

### 1.3 语法特性

- **JSON / JSONC**：支持 `//` 行注释与 `/* ... */` 块注释，字符串字面量内部的注释符号不受影响；
- **环境变量插值**：`${VAR}` 展开为对应环境变量的值，未设置时展开为空字符串；
- **路径展开**：`--config` 与 `VVA_CONFIG` 的路径支持 `~` 前缀。

### 1.4 无配置文件：环境变量直连

未找到任何配置文件时，按标准环境变量构造官方端点的供应商：

| 环境变量 | 生成的 provider id | 协议 |
|---|---|---|
| `OPENAI_API_KEY` | `openai` | response |
| `ANTHROPIC_API_KEY` | `anthropic` | anthropic |
| `GEMINI_API_KEY` | `gemini` | gemini |

此模式下默认模型为 `openai/gpt-5.6-terra`；由于供应商未声明 `models`，任意模型名可直接使用，例如 `vva run --model anthropic/claude-sonnet-4-5`。

---

## 2. 顶层结构

```jsonc
{
  "settings": {
    "default": "a/gpt-5.6-terra",     // 缺省模型规格 {provider_id}/{model_id}
    "small_model": "a/gpt-5.6-luna",  // 会话标题生成用的小模型，缺省时用主模型
    "editor": "hashline",             // 缺省文件编辑协议：replace | patch | hashline
    "web": { /* 外置 web search / fetch，见 2.1 */ },
    "agent": { /* agent space，见 2.2 */ }
  },
  "providers": {
    "<provider_id>": { /* ... */ }
  }
}
```

### settings

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `default` | string | 是 | 缺省模型规格；未传 `--model` 且 `state.json` 不可用时使用 |
| `small_model` | string | 否 | 会话标题生成所用的模型规格；缺省时沿用主模型 |
| `editor` | string | 否 | 模型未声明 `annotation.editor` 时新会话采用的编辑协议：`replace` / `patch` / `hashline`；缺省 `hashline` |
| `web` | object | 否 | 外置 web search / fetch 补全，见 2.1 |
| `agent` | object | 否 | agent space（常驻 IM agent）配置，见 2.2；`vva agent` 要求声明 |

### 2.1 settings.web 外置搜索

```jsonc
"web": {
  "provider": {
    "brave":  { "api_key": "${BRAVE_API_KEY}" },   // Brave Search LLM Context，仅 search
    "tavily": { "api_key": "${TAVILY_API_KEY}" },  // Tavily，search + fetch
    "google": { "model": "homo/gemini-3.8-flash" } // 已配置的 Gemini 模型，仅挂载原生 google_search，仅 search
  },
  "search": "brave",   // web search 使用的 provider 名
  "fetch": "local"     // web fetch 使用的 provider 名；local 为内置，无需声明
}
```

| 字段 | 说明 |
|---|---|
| `provider` | 外置 provider 声明：`brave`（Brave LLM Context，仅 search）、`tavily`（Tavily，search + fetch）、`google`（`model` 指向已配置的 gemini 协议模型；search 单次请求仅挂载原生 `google_search`，来源取自 grounding、片段为被引用文本段；fetch 挂载 `url_context` 并同挂 `google_search`，正文为模型据页面给出的 Markdown 转述而非逐字原文）；`local` 为内置 fetch provider（HTTP 抓取 + Readability 式正文提取），不在此声明 |
| `search` | provider 名选择：`brave` / `tavily` / `google` |
| `fetch` | provider 名选择：`local` / `tavily` / `google` |

模型原生 web 能力缺位时（`annotation.web_search: false`、`annotation.web_fetch: false` 或协议本身未提供），会话 web 开关开启且对应位配置了外置 provider 的情况下，注册同名客户端工具 `web_search` / `web_fetch` 补全，并在状态栏标注接管后端（TUI 状态行 `web:<search>+<fetch>`，Web 输入区「搜索」chip 标题）；原生能力在位时优先使用原生，外置工具不注册。Responses 的原生 `web_search` 含 `open_page` / `find_in_page`，视为 fetch 在位。`settings.web` 未配置时行为与既往一致（搜索选项报"被忽略"通知）。选中的 provider 不具备对应能力、未声明或密钥缺失时，会话启动报错。

### 2.2 settings.agent 常驻 IM agent

```jsonc
"agent": {
  "model": "a/gpt-5.6-terra",
  "ttl_secs": 18000,
  "channels": {
    "qq": { "app_id": "123456", "secret": "${QQ_BOT_SECRET}", "allowed_users": ["<openid>"] }
  },
  // 可选：L1 路由改用 Decisions API（密钥取自 providers.openrouter.api_key）
  "router": { "decisions": { "provider": "openrouter", "model": "typesafe/jev-1.13", "min_confidence": 0.6 } }
}
```

整段省略字段取默认值；项目配置声明 `agent` 时整段替换全局。时长为整数秒，预留为整数 token。router 缺省复用 `settings.small_model`；声明 `router.decisions` 时使用决策 API，可选 OpenRouter Decisions（alpha）或 OpenCode System One。

| 字段 | 默认 | 说明 |
|---|---|---|
| `model` | `settings.default` | oneshot 与 topic session 使用的模型规格 |
| `ttl_secs` | `18000`（5h） | 普通 session 空闲清理 |
| `oneshot_ttl_secs` | `0` | oneshot 回复完即可清理，不做记忆沉淀 |
| `rollover_reserve_tokens` | `32000` | 上下文达到 `limit.context - 预留` 时回合结束触发 rollover |
| `pending_files_ttl_secs` | `600` | 待附件缓冲等待时间 |
| `task_timeout_secs` | `600` | 定时任务单次执行超时，超时中止并计为失败 |
| `progress_interval_secs` | `180`（3min） | 轮次（含定时任务）未结束时每隔此时长向聊天推送进度摘要：改动文件、命令意图、文字消息、读取/检索次数；`0` 关闭 |
| `task_max_failures` | `3` | 定时任务连续失败多少次后自动停用 |
| `router.decisions` | 无 | L1 决策 API：`type`（可选，`openrouter` / `opencode`；省略时按 `provider` 同名预设选择，其余节点沿用 `openrouter`）、`provider`（取 `providers.<provider>.api_key`）、`model`（服务原生模型 ID）、`min_confidence`（默认 `0.6`，`action` / `session` 题低于此值续接 focus；无 focus 时显式报错）、`endpoint`（可选覆盖服务默认端点）；声明后不再要求 `small_model` |
| `channels.qq.app_id` / `secret` | 无 | QQ 机器人凭据；已声明 qq 而二者为空（如 `${VAR}` 未设置）时启动报错 |
| `channels.qq.allowed_users` | `[]` | 用户白名单；白名单外静默丢弃；空表示拒绝所有人，含 `"*"` 表示允许任何用户 |

OpenCode System One 配置示例（与已有主模型及渠道配置合并）：

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

`provider` 引用密钥节点；使用 `opencode` 或 `openrouter` 同名节点时可省略 `type`。自定义节点名时可通过 `type` 指定服务；显式 `type` 优先于节点名，其他省略 `type` 的节点沿用 OpenRouter。OpenCode 默认端点为 `https://opencode.ai/inference/systemone/v1/systemone`，支持 `jev-1.13` 与 `jev-1.13-free`；OpenRouter 默认端点保持 `https://openrouter.ai/api/alpha/decisions`，模型如 `typesafe/jev-1.13`。两者共用 `state` + `questions` 请求与 `answers` + `usage` 响应转换，均使用 Bearer 鉴权。参见 [OpenCode 模型端点与 Jev 文档](https://opencode.ai/v2/docs/console/models/#endpoints)。

---

## 3. providers 供应商节点

```jsonc
{
  "providers": {
    "a": {
      "type": "response",              // 可选：response | anthropic | gemini，默认 response
      "base_url": "https://gw.example.com", // 可选，缺省用官方端点
      "api_key": "${MY_API_KEY}",      // 明文或 ${VAR} 插值
      "models": { /* ... */ }
    }
  }
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | string | 协议类型：`response`（OpenAI Responses）、`anthropic`（Anthropic Messages）、`gemini`（Google Gemini）。缺省 `response` |
| `base_url` | string | API 端点前缀，缺省按下表取官方端点 |
| `api_key` | string | 鉴权密钥，推荐 `${VAR}` 插值 |
| `models` | object | 模型清单，`model_id` → 模型配置；可整体省略（见 3.2） |

### 3.1 base_url 规范化

端点按协议自动规范化，结尾多余的 `/` 被去除，未带版本后缀时自动补全：

| type | 缺省端点 | 自动补全后缀 |
|---|---|---|
| `response` | `https://api.openai.com/v1` | `/v1`（已以 `/v1` 或 `/responses` 结尾则原样） |
| `anthropic` | `https://api.anthropic.com/v1` | `/v1`（已以 `/v1` 或 `/messages` 结尾则原样） |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta` | `/v1beta`（已以 `/v1beta`、`/v1` 结尾或含 `/models` 则原样） |

### 3.2 省略 models

`models` 省略（或为空表）时，该供应商下任意模型名均可直接使用，模型按协议默认参数调用，例如环境变量直连与 6.1 的预设节点。

---

## 4. models 模型节点

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
| `type` | string | 继承 provider | 模型级协议覆写。典型场景：统一反代网关下个别模型使用不同下层协议 |
| `modalities.input` | string[] | `[]` | 输入模态声明：`text` / `image` / `video` / `pdf`；附件提交时据此与协议能力双重校验 |
| `modalities.output` | string[] | `[]` | 输出模态声明 |
| `reasoning` | bool | `false` | 是否为推理模型 |
| `limit.context` | int | — | 上下文窗口上限（tokens），用于统计面板比例展示 |
| `limit.output` | int | — | 单轮输出上限（tokens） |
| `variants` | string[] | — | 推理档位白名单，常用值 `minimal` / `low` / `medium` / `high` / `xhigh` / `max` |
| `cost` | object | — | 费率（美元 / 百万 tokens），用于成本估算，见 4.1 |
| `annotation` | object | — | 模型级注记：服务端工具声明微调与请求档位，见 4.2 |

### 4.1 cost 费率

| 字段 | 说明 |
|---|---|
| `input` / `output` | 普通 input / output token 费率 |
| `cache_read` | 缓存读取费率 |
| `cache_write` | 缓存写入费率 |
| `context_over_200k` | 上下文超过 200k tokens 时的整体费率替换：`input` / `output` / `cache_read` / `cache_write`（cache 字段缺省回落基础费率） |

### 4.2 annotation 注记

| 字段 | 默认 | 说明 |
|---|---|---|
| `annotation.web_fetch` | `true` | 设为 `false` 时不向端点声明 `web_fetch` 服务端工具。用于未接线特定 server tool 的中转端点——工具声明组中出现端点未识别的条目时，端点会整组丢弃声明；仅声明 `web_search` 可保住原生搜索。原生 fetch 缺位后，配置 `settings.web.fetch` 时以客户端工具补全 |
| `annotation.web_search` | `true` | 设为 `false` 标记模型/端点不支持原生 web search，不向端点声明原生搜索；配置 `settings.web.search` 时以客户端工具 `web_search` 补全 |
| `annotation.affinity` | provider 节点 id | 模型亲和组：同组模型切换时互相复用原生回传状态（response_id 链、encrypted thinking、签名块）；跨组切换时历史降维为通用重建（推理签名丢弃、搜索调用按当前工具面重投影）并发提示。混系网关（同一节点下多家族模型）按模型标注，如 `"gpt"` |
| `annotation.editor` | `settings.editor` | 文件编辑协议，工具名均为 `Edit`，仅参数与 description 不同。会话创建时写入 Header 并固定，之后换模不改变：`hashline` 为 `[PATH#TAG]` 快照 + `PUT`/`CUT`/`REM`/`MV` 行号补丁；`replace` 为 `old_string`/`new_string`；`patch` 为 Codex V4A `input` |
| `annotation.web_tool_version` | `20260318` | Anthropic 协议下 `web_search_<版本>` / `web_fetch_<版本>` 服务端工具的版本后缀。用于尚未识别最新版本的中转端点（报 `unknown variant web_search_20260318`），可设为 `"20260209"` |
| `annotation.fast` | `false` | 设为 `true` 时请求体携带 `"service_tier": "fast"`（Responses API），走端点快速档位 |

---

## 5. 模型寻址与推理档位

### 5.1 规格格式

- 推荐写法 `{provider_id}/{model_id}`，如 `a/gpt-5.6-terra`；
- 裸 `{model_id}` 仅当在所有供应商的 `models` 中唯一命中时可用，多命中时报歧义错误；
- 当缺省供应商未声明 `models`（预设 / 环境变量直连）时，裸模型名按缺省供应商解析。

### 5.2 档位校验与映射

`--variant` / `/variant` 传入的档位先按 `variants` 白名单校验，非法值报 `InvalidVariant` 并列出合法项。校验通过后映射为对应协议的官方字段：

- `response` 协议：映射为 `reasoning.effort`；
- `anthropic` / `gemini` 协议：暂不支持推理档位，传入时收到能力 Notice 提示。

`none` 用于关闭推理档位：模型声明了 `variants` 时，仅当白名单中包含 `none` 才可用。

### 5.3 档位记忆

`~/.vva/state.json` 记录上次使用的模型与档位（`last_model` / `last_variant`）。切换模型、切换档位或切换会话时更新；下次启动未传 `--model` 时沿用该选择，无法解析时回落 `settings.default`。

---

## 6. 内置供应商预设：glm-coding-plan

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

- **识别方式**：供应商 id 为 `glm-coding-plan`（`type` 省略或为 `response`），或任意 id 显式声明 `type: "glm-coding-plan"`；
- **自动补全**：协议 `response`；端点 `https://open.bigmodel.cn/api/v1`（Codex Responses 端点，原生 `web_search`）；模型 `glm-5.3` 与 `glm-5.3-flash`（已显式声明的同名字段与模型不被覆盖）。

自定义节点 id 时显式声明 `type`：

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

---

## 7. 运行时文件布局

```
~/.vva/
├── config.jsonc            # 全局配置（或 config.json）
├── state.json              # 运行时偏好（last_model / last_variant）
├── vva.log                 # TUI 模式日志（级别由 RUST_LOG 控制）
└── sessions/
    └── {project_key}/      # 按工作区路径编码的项目目录
        ├── {session_id}.jsonl
        ├── scratch/        # 项目便签
        └── tool_outputs/   # 大体积工具输出
```

---

## 8. 编辑器补全

仓库根目录的 [schema.json](../schema.json) 描述了完整配置结构（含各字段说明与枚举取值）。在配置文件中声明 `$schema` 字段指向它，编辑器即可提供字段补全、悬停说明与校验：

```jsonc
{
  "$schema": "/path/to/vva/schema.json", // 本地路径或远程 URL
  "settings": { /* ... */ }
}
```

`$schema` 字段在解析时被忽略，可放心保留。
