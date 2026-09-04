# 统一配置与模型寻址系统 (Configuration & Model Routing System)

## 1. 配置文件发现与分层加载

代码位置：[`src/config/mod.rs`](../src/config/mod.rs) 与 [`src/config/parser.rs`](../src/config/parser.rs)

vva 支持分层配置加载机制：
1. **工作区就近优先**：当前工程目录下的 `.vva/config.jsonc` 或 `.vva/config.json`；
2. **全局用户配置**：用户主目录下的 `~/.vva/config.jsonc` 或 `~/.vva/config.json`；
3. **缺省模板参考**：源码根目录下的 `config.example.jsonc`。

---

## 2. 核心解析特性

### 2.1 JSON 与 JSONC 词法剥离 (`strip_jsonc_comments`)
- 用户配置文件支持标准 JSON 以及带有注释的 JSONC 语法；
- **严格词法扫描**：剥离 `//` 行注释与 `/* ... */` 块级注释；
- **字符串保护**：确保出现在字符串双引号内部的 `//` 或 `/*`（例如 URL 链接 `https://...`）**绝不被误删**。

### 2.2 环境变量插值展开 (`interpolate_env_vars`)
- 配置文件中的敏感信息（如 API Key）建议使用环境变量插值语法：`"${ENV_VAR_NAME}"`；
- 解析引擎在反序列化之前自动扫描并将其替换为对应系统环境变量的真实值；若未找到对应环境变量则优雅保留或提示。

---

## 3. 模型寻址与动态路由规范

代码位置：[`src/config/resolver.rs`](../src/config/resolver.rs) 与 [`src/config/model.rs`](../src/config/model.rs)

### 3.1 规格寻址格式：`{provider_id}/{model_id}`
模型统一采用二段式寻址规范：
- 例如：`"openai/gpt-5-terra"`、`"anthropic/claude-3-7-sonnet"`、`"google/gemini-2.5-pro"`；
- 路由器首先在全局配置的 `providers` 表中寻址供应商节点，继而在其子集 `models` 表中匹配具体模型配置。

### 3.2 节点协议覆写 (`type`)
在某些统一反代网关节点下，某些模型可能使用的是不同的下层协议。配置系统支持在具体模型节点上声明独立的协议类型以覆写供应商默认协议：
```jsonc
{
  "providers": {
    "my-gateway": {
      "type": "response",
      "base_url": "https://gateway.example.com",
      "api_key": "${GATEWAY_KEY}",
      "models": {
        "claude-custom": {
          "type": "anthropic", // 覆写网关默认的 response 协议，改用 anthropic 协议交互
          "name": "Claude Custom Endpoint"
        }
      }
    }
  }
}
```

### 3.3 推理强度档位（variants）声明与校验
- 模型配置支持声明其所支持的思考与推理强度档位，例如：
  `"variants": ["minimal", "low", "medium", "high", "xhigh", "max"]`；
- 当用户在 CLI 或 TUI 中传入 `--variant high` 或使用 `/variant high` 时，路由器会自动进行合法性校验，并将其映射为对应供应商的官方协议字段（如 OpenAI Responses 的 `reasoning_effort`）。

### 3.4 URL 端点规范化
- 根据供应商类型自动规范化 URL 端点（自动去除重复斜杠，自动适配 `/v1` 或 `/v1beta`）；
- 避免因用户在 `base_url` 结尾多写或少写 `/` 导致 HTTP 404。
