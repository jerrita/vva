# 原生供应商适配与转换规范 (Native Provider Adapters & Zero-Fallback Rule)

## 1. 适配器转换“零兜底”约束 (Zero Fallback Rule)

代码位置：[`src/provider/`](../src/provider/)

适配器遵循以下约束：
1. **原生协议适配**：Provider 模块实现厂商官方原生协议，不退化为通用 Chat Completions 或跨厂商伪造报文；
2. **禁止字段别名猜测**：
   - 不编写类似 `"output_text" | "text"`、`"web_search_call" | "web_search"` 等别名猜测链；
   - 官方最新文档未定义的字段，不在代码中臆造；
3. **单测 Mock 真实性约束**：
   - 单元测试 Mock 报文对照厂商官方文档规范构造，不在生产适配器代码中为错误测试数据添加容错分支。

### 1.1 接口边界
[`Provider`](../src/provider/mod.rs) trait 对外只暴露调用方真正需要的能力：

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn complete(...) -> Result<ProviderResponse, ProviderError>;
    async fn complete_stream(...) -> Result<ProviderResponse, ProviderError>;
}
```

端点拼装、鉴权头、请求构建与响应解析属于各适配器内部实现。传输由 [`http.rs`](../src/provider/http.rs) 共享：`post_json` 负责非流式往返，`post_sse` 负责补 Accept 头、发起请求并驱动 SSE 事件循环，适配器只提供 URL、鉴权头、请求体与 `SseEventHandler`。

跨供应商的通用辅助集中在 [`common.rs`](../src/provider/common.rs)：事件背压转发（`EventForwarder`）、同角色块合并（`RoleBlockMerger`）以及严格字段读取（`require_str` / `require_u64` / `require_bool` / `require_value` / `require_array`）——必填字段缺失或类型不符一律报协议错误，三家适配器使用同一套读取器与错误语义。

---

## 2. OpenAI Responses API 适配器

代码位置：[`src/provider/response.rs`](../src/provider/response.rs)

### 2.1 服务端链式 `previous_response_id`
- **历史状态管理**：传统 Chat Completions 无法保存服务端状态，多轮对话需重复上传历史消息。
- **Responses 原生机制**：
  - 适配器在构建请求 Payload 时，从 Session 历史中定位最近一次有效的 Assistant 轮次，提取其 `response_id` 作为本次请求的 `previous_response_id`：
    ```json
    {
      "model": "gpt-5-terra",
      "previous_response_id": "resp_abc123",
      "input": [
        {
          "role": "user",
          "content": "Add unit tests for this feature"
        }
      ]
    }
    ```
  - 上传增量用户消息与未结算的工具返回，减少网络往返负载与 Token 消耗。

### 2.2 原生 Web Search 与注脚
- 原生启用 `"tools": [{ "type": "web_search" }]`，请求 `include: ["web_search_call.action.sources"]`；
- 流式 `response.output_item.done` 将官方 item 投影为领域 `WebSearchCall`，并作为 `AgentEvent` 发布；落盘领域条目沿用该快照；
- `response.completed.output` 原样写入 `TurnMeta.output_items`，供协议回传；
- 返回的文本如果携带官方的 `url_citation` 注脚，直接提取 `url`, `title`, `start_index`, `end_index` 并封装入 `Annotation::UrlCitation`。

### 2.3 GLM Coding Plan
配置 `type: "glm-coding-plan"` 或供应商 id `glm-coding-plan` 时，路由到同一 Responses 适配器，默认端点 `https://open.bigmodel.cn/api/v1`（官方 Codex Responses），以使用原生联网搜索。

---

## 3. Anthropic Messages API 适配器

代码位置：[`src/provider/anthropic.rs`](../src/provider/anthropic.rs)

### 3.1 Prompt Caching (`cache_control`) 注入
- **原生机制**：Anthropic 要求在特定块注入 `{"type": "ephemeral"}` 标记缓存断点。
- **适配器行为**：
  - 在全局系统提示词（System Prompt）尾部追加 `cache_control`；
  - 在长对话的历史消息分界点维护断点标记；
  - 响应报文中提取 `cache_creation_input_tokens` 与 `cache_read_input_tokens` 并写入 `AnthropicTurnMeta`，供计费引擎结算折扣。

### 3.2 Thinking 思考块数字签名 (`signature`) 保持
- **防篡改签名要求**：Anthropic 启用 Extended Thinking 的模型在返回思考内容时随附加密签名 `signature`。
- **签名保持**：回传历史消息时，原样传递 `signature` 字段（`{"type": "thinking", "thinking": "...", "signature": "..."}`），满足 Anthropic API 校验要求。
- **存储与还原**：`ContentPart::Reasoning { content, signature }` 保存并在请求序列化时还原。

### 3.3 原生 Web Search / Web Fetch 服务端工具
- **工具声明**：`enable_web_search` 开启时在 `tools` 中声明 `web_search_20260318` 与 `web_fetch_20260318` 服务端工具。`_20260209` 起的版本默认经代码执行容器调用，vva 未声明 programmatic tool calling，故显式注入 `allowed_callers: ["direct"]` 直连，否则 API 返回 400。
- **模型级工具注记**：未接线 `web_fetch` 的中转端点会在声明中带上它时整组丢弃工具（连带 web_search 失效）。模型配置 `"annotation": { "web_fetch": false }` 可关闭 web_fetch 声明，仅保留 web_search。`"annotation": { "editor": "hashline" | "patch" | "replace" }` 选择 `Edit` 的参数与说明（未声明时取 `settings.editor`，缺省 `hashline`），写入会话后固定。
- **服务端代执行**：`server_tool_use` 与 `web_search_tool_result` / `web_fetch_tool_result` 块由服务端直接产出，无需回传 `tool_result`。
- **统一投影**：web_search 调用对投影为 `WebSearchCall`（Search 动作，携带来源列表，错误结果按 Failed 上报）；web_fetch 调用对投影为 `WebSearchCall`（OpenPage 动作）；文本块中的 `web_search_result_location` 引用投影为 `Annotation::WebSearchResultLocation`（无字符偏移，以 `cited_text` 摘录定位）。
- **原生块链保真回传**：assistant 全部原生内容块（强类型 `AnthropicContentBlock`，含 `encrypted_content` / `encrypted_index` 等服务端加密字段与 `redacted_thinking`）存入 `AnthropicTurnMeta.content_blocks`，多轮请求逐块原样回传，缺失或改动即 400；同 Responses `output_items` 原生条目链策略。
- **流式事件**：`server_tool_use` 的输入经 `input_json_delta` 增量下发；结果块在 `content_block_start` 整块到达；引用经 `citations_delta` 逐条追加至当前 text 块。
- **端点能力诊断**：模型误以客户端 `tool_use` 调用服务端工具时（端点未实现 server tools 的典型症状），工具结果返回显式诊断（端点不支持原生服务端工具），而非误导性的 `Tool not found`。

---

## 4. Google Gemini API 适配器

代码位置：[`src/provider/gemini.rs`](../src/provider/gemini.rs)

### 4.1 原生 Google Search Grounding 投影
- 原生启用 `tools: [{ "google_search": {} }]`；
- **字符偏移量映射**：Gemini 提供分离的 `groundingMetadata`（包含 `groundingChunks` 网页列表与 `groundingSupports` 字符索引范围），由适配器映射为注脚；
- **适配器层计算与投影**：
  - 适配器根据 `groundingSupports` 中声明的 `segment.startIndex` 与 `segment.endIndex`，对照 `groundingChunks` 索引，在内存中计算字符偏移量；
  - 将分离的引用数据映射为标准 `Annotation::UrlCitation` 并注入对应的 `ContentPart::Text` 中。

### 4.2 代码沙盒与安全评级
- 映射 Gemini 内置 Python 沙盒执行结果至 `GeminiTurnMeta.code_execution_result`；
- 映射安全拦截评级至 `GeminiTurnMeta.safety_ratings`。

---

## 5. 通用 SSE 流式事件解码器

代码位置：[`src/provider/sse.rs`](../src/provider/sse.rs)

- 提供通用标准 Server-Sent Events 流式解码器（`SseDecoder`）；
- 遵循 W3C 规范：支持分块 Chunked 输入拆包、多字节 UTF-8 跨包重组、CRLF/LF 换行规范化以及空行事件触发机制；
- 为 Responses、Anthropic 与 Gemini 协议流式解析提供底层支撑。
