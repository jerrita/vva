# 供应商适配

适配器实现厂商官方原生协议，约束（零兜底、Mock 真实性）见 [AGENTS.md §2](../AGENTS.md#2-设计约束)。代码位置 [`src/provider/`](../src/provider/)。

## 1. 公共层

### 1.1 接口边界
[`Provider`](../src/provider/mod.rs) trait 对外暴露调用方需要的能力：

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn complete(...) -> Result<ProviderResponse, ProviderError>;
    async fn complete_stream(...) -> Result<ProviderResponse, ProviderError>;
}
```

端点拼装、鉴权头、请求构建与响应解析属于各适配器内部实现。[`http.rs`](../src/provider/http.rs) 提供 `post_json` 与 `post_sse`；[`websocket.rs`](../src/provider/websocket.rs) 使用独立的 HTTP/1.1 客户端升级连接，沿用公共客户端配置以保留代理、TLS 与鉴权行为，并在空闲期间处理控制帧。适配器负责协议事件到领域模型的转换。

跨供应商的通用辅助集中在 [`common.rs`](../src/provider/common.rs)：事件背压转发（`EventForwarder`）、同角色块合并（`RoleBlockMerger`）以及严格字段读取（`require_str` / `require_u64` / `require_bool` / `require_value` / `require_array`）——必填字段缺失或类型不符一律报协议错误，三家适配器使用同一套读取器与错误语义。

### 1.2 Freeform 工具
工具入参形态由 [`ToolInput`](../src/model/tool.rs) 声明：`Json(schema)` 或 `Freeform { grammar }`（grammar 为可选 Lark 文法）。领域层的 freeform 调用为 `ToolCall { kind: Custom, arguments: <原始输入文本> }`，协议差异只在适配器内处理：
- **Responses**：原生 custom tool。声明为 `{"type":"custom", "format": text | grammar(lark)}`，输出条目 `custom_tool_call` 解析为 Custom 调用，结果以 `custom_tool_call_output` 回传；
- **Anthropic / Gemini**：无 freeform 协议，声明时投影为单字段 `{"input": string}` 的 function（`common::function_parameters`），解析时取出 `input` 原文构造 Custom 调用，回放时重新包装；参数不是恰含字符串 `input` 的对象时报协议错误。

当前 freeform 工具为 `apply_patch`（附 Codex V4A Lark 文法）与 `hashline_edit`。

---

## 2. OpenAI Responses API 适配器

代码位置：[`src/provider/response/`](../src/provider/response/)

### 2.1 传输与上下文续接

传输选择见 [config-reference §4.2](config-reference.md#42-annotation-注记)。`complete` 与 `complete_stream` 使用相同的传输选择；WebSocket 的非流式调用收集原生事件后返回完整结果。

- **连接生命周期**：Provider 由 Session 持有，工具注册表重建、后续用户输入和后台完成续跑复用连接；会话间独立。模型、端点、凭据或传输配置变化时更新绑定。
- **WebSocket 请求**：发送 `response.create`，首次携带完整上下文。成功完成后记录连接内 `response_id` 及输入与原生输出的条目指纹；下一次仅在历史前缀、亲和组和请求参数一致时携带 `previous_response_id` 并发送增量。工具结果直接关联服务端已返回的调用，历史图片、PDF 和文本无需重复上传。
- **状态保真**：连接内锚点支持 `store: false`，仅在 `response.completed` 解析成功后更新，不落盘；领域历史及原始附件完整保留。指纹缓存不保存第二份媒体内容。
- **重建边界**：首次连接、重启或空闲连接关闭后发送完整上下文；历史回退、分支或请求参数变化时重新建立链。请求中止、在途断线、超时、非法事件或服务端错误均显式报错并清除连接状态，后续调用重新建链；不自动重放失败请求。
- **HTTP 请求**：从历史中提取最近的同亲和组、已存储 `response_id`，发送其后的增量；没有有效锚点时发送完整上下文。该链式机制减少上传量，历史上下文仍参与输入 token 计量。
- **事件投影**：两种传输共用 `ResponseStreamHandler`，经 `AgentEvent` 驱动界面。调试日志仅记录 WebSocket 的增量模式、条目数和发送字节数。

### 2.2 原生 Web Search 与注脚
- 原生启用 `"tools": [{ "type": "web_search" }]`，请求 `include: ["web_search_call.action.sources"]`；`web_search_call.action` 含 `search` / `open_page` / `find_in_page`，原生搜索在位即含页面抓取，不注册外置 `web_fetch`；
- 流式 `response.output_item.done` 将官方 item 投影为领域 `WebSearchCall`，并作为 `AgentEvent` 发布；落盘领域条目沿用该快照；
- `response.completed.output` 原样写入 `TurnMeta.output_items`，供协议回传；
- 返回的文本如果携带官方的 `url_citation` 注脚，直接提取 `url`, `title`, `start_index`, `end_index` 并封装入 `Annotation::UrlCitation`。

`type: "glm-coding-plan"` 预设复用同一适配器，见 [config-reference §6](config-reference.md#6-内置供应商预设glm-coding-plan)。

---

## 3. Anthropic Messages API 适配器

代码位置：[`src/provider/anthropic/`](../src/provider/anthropic/)

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
- **模型级工具注记**：未接线 `web_fetch` 的中转端点会在声明中带上它时整组丢弃工具（连带 web_search 失效）。模型注记 `web_fetch: false` 可关闭 web_fetch 声明，仅保留 web_search。其余注记见 [config-reference §4.2](config-reference.md#42-annotation-注记)。
- **服务端代执行**：`server_tool_use` 与 `web_search_tool_result` / `web_fetch_tool_result` 块由服务端直接产出，无需回传 `tool_result`。
- **统一投影**：web_search 调用对投影为 `WebSearchCall`（Search 动作，携带来源列表，错误结果按 Failed 上报）；web_fetch 调用对投影为 `WebSearchCall`（OpenPage 动作）；文本块中的 `web_search_result_location` 引用投影为 `Annotation::WebSearchResultLocation`（无字符偏移，以 `cited_text` 摘录定位）。
- **原生块链保真回传**：assistant 全部原生内容块（强类型 `AnthropicContentBlock`，含 `encrypted_content` / `encrypted_index` 等服务端加密字段与 `redacted_thinking`）存入 `AnthropicTurnMeta.content_blocks`，多轮请求逐块原样回传，缺失或改动即 400；同 Responses `output_items` 原生条目链策略。
- **流式事件**：`server_tool_use` 的输入经 `input_json_delta` 增量下发；结果块在 `content_block_start` 整块到达；引用经 `citations_delta` 逐条追加至当前 text 块。
- **端点能力诊断**：模型误以客户端 `tool_use` 调用服务端工具时（端点未实现 server tools 的典型症状），工具结果返回显式诊断（端点不支持原生服务端工具）。

### 3.4 独立搜索

模型启用 `annotation.standalone_search` 后，主会话调用客户端 `web_search`，工具使用同一模型、端点和凭据发起独立 Messages 流式请求，仅携带搜索词和搜索工具，以 `tool_choice` 强制选择搜索并关闭 thinking；工具版本及开关见 [config-reference §4.2](config-reference.md#42-annotation-注记)。

独立请求只提取文本与搜索结果的 URL、可选标题，不按服务端调用 ID 配对；搜索原生块和加密字段不进入主会话。提取结果作为普通 `ToolResult` 文本回传，主会话继续按客户端调用 ID 关联调用与结果。该路径不修改原生 Anthropic 适配器的强类型校验。搜索错误、缺少结果块、非法报文和流中断均作为工具错误返回；合法的空结果列表表示未命中。原生 fetch 与外置 fetch 沿用原有配置。

---

## 4. Google Gemini API 适配器

代码位置：[`src/provider/gemini/`](../src/provider/gemini/)

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

---

## 6. 原生结构化输出 (`response_schema`)

`CompletionOptions.response_schema: Option<ResponseSchema { name, schema }>` 声明期望的 JSON Schema，各供应商用官方原生机制约束输出，返回的 text 即 JSON 文本，由调用方 serde 反序列化为强类型。适配器不修复、不校验返回内容，非法输出（拒答 / 截断）由上层报错。

| 供应商 | 请求字段 |
|---|---|
| Responses | `text.format = {type:"json_schema", name, schema, strict:true}` |
| Anthropic | `output_config.format = {type:"json_schema", schema}`（无需 beta header，无 `name`） |
| Gemini | `generationConfig.responseMimeType="application/json"` + `generationConfig.responseJsonSchema` |
