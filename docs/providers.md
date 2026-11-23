# 供应商适配

适配器实现厂商官方原生协议，代码位于 [`crates/vva-core/src/provider/`](../crates/vva-core/src/provider/)。设计约束见 [AGENTS.md §2](../AGENTS.md#2-设计约束)。

## 1. 公共层

### 1.1 接口边界

[`Provider`](../crates/vva-core/src/provider/mod.rs) trait：

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn complete(...) -> Result<ProviderResponse, ProviderError>;
    async fn complete_stream(...) -> Result<ProviderResponse, ProviderError>;
}
```

端点拼装、鉴权头、请求构建与响应解析都在各适配器内部。适配器负责把协议事件转换为领域模型。

| 模块 | 职责 |
|---|---|
| [`http.rs`](../crates/vva-core/src/provider/http.rs) | `post_json` 与 `post_sse` |
| [`websocket.rs`](../crates/vva-core/src/provider/websocket.rs) | 用独立的 HTTP/1.1 客户端升级连接，与公共客户端共用代理、TLS 与鉴权配置，空闲期间处理控制帧 |
| [`common.rs`](../crates/vva-core/src/provider/common.rs) | 事件背压转发 `EventForwarder`、同角色块合并 `RoleBlockMerger`、严格字段读取 `require_str` / `require_u64` / `require_bool` / `require_value` / `require_array` |

三家适配器共用同一套字段读取器与错误语义：必填字段缺失或类型不符即报协议错误。

### 1.2 Freeform 工具

工具入参形态由 [`ToolInput`](../crates/vva-core/src/model/tool.rs) 声明：`Json(schema)` 或 `Freeform { grammar }`，grammar 为可选的 Lark 文法。领域层的 freeform 调用是 `ToolCall { kind: Custom, arguments: <原始输入文本> }`，协议差异只在适配器内处理。

| 协议 | 声明 | 调用与回传 |
|---|---|---|
| Responses | 原生 custom tool：`{"type":"custom","format": text \| grammar(lark)}` | `custom_tool_call` 解析为 Custom 调用，结果以 `custom_tool_call_output` 回传 |
| Anthropic / Gemini | 无 freeform 协议，声明为单字段 `{"input": string}` 的 function（`common::function_parameters`） | 取出 `input` 原文构造 Custom 调用，回放时重新包装 |

Anthropic / Gemini 收到的参数不是恰含字符串 `input` 的对象时，报协议错误。

当前的 freeform 工具：`apply_patch`（附 Codex V4A Lark 文法）与 `hashline_edit`。

## 2. OpenAI Responses API 适配器

代码位于 [`crates/vva-core/src/provider/response/`](../crates/vva-core/src/provider/response/)。

### 2.1 传输与上下文续接

传输由 `annotation.websocket` 选择，见 [config-reference §4.2](config-reference.md#42-annotation-注记)。`complete` 与 `complete_stream` 使用相同的传输；WebSocket 的非流式调用收集原生事件后返回完整结果。

**WebSocket 连接**

- 连接由 Session 持有。工具注册表重建、后续用户输入与后台完成续跑复用同一连接，会话之间独立。
- 模型、端点、凭据或传输配置变化时更新绑定。
- 请求发送 `response.create`，首次携带完整上下文。
- 成功完成后记录连接内的 `response_id` 与输入、原生输出的条目指纹。下一次请求只在历史前缀、亲和组和请求参数一致时携带 `previous_response_id` 并发送增量。
- 工具结果直接关联服务端已返回的调用，历史图片、PDF 和文本无需重复上传。
- 连接内锚点支持 `store: false`，只在 `response.completed` 解析成功后更新，不落盘。领域历史与原始附件保留，指纹缓存不保存第二份媒体内容。

**HTTP 请求**

- 从历史中提取最近的同亲和组、已存储的 `response_id`，发送其后的增量；没有有效锚点时发送完整上下文。
- 链式续接减少上传量，历史上下文按全量计入输入 token。

**重建与出错**

- 首次连接、重启、空闲连接关闭、历史回退或分支、请求参数变化时发送完整上下文并重新建链。
- 请求中止、在途断线、超时、非法事件或服务端错误都报错并清除连接状态，后续调用重新建链。失败请求不重放。

**事件投影**

- 两种传输共用 `ResponseStreamHandler`，经 `AgentEvent` 驱动界面。
- 调试日志记录 WebSocket 的增量模式、条目数与发送字节数。

### 2.2 原生 Web Search 与注脚

- 声明 `"tools": [{ "type": "web_search" }]`，请求 `include: ["web_search_call.action.sources"]`。
- `web_search_call.action` 含 `search` / `open_page` / `find_in_page`。原生搜索在位即包含页面抓取，此时不注册外置 `web_fetch`。
- 流式 `response.output_item.done` 把官方 item 转为领域 `WebSearchCall`，发布为 `AgentEvent`；落盘的也是该快照。
- `response.completed.output` 写入 `TurnMeta.output_items`，供回传。
- 文本携带官方 `url_citation` 注脚时，提取 `url`、`title`、`start_index`、`end_index`，封装为 `Annotation::UrlCitation`。

`type: "glm-coding-plan"` 预设复用同一适配器，见 [config-reference §6](config-reference.md#6-glm-coding-plan-预设)。

### 2.3 推理摘要与缓存写入用量

`CompletionOptions.reasoning_summary: Option<ReasoningSummary>` 独立控制摘要，枚举 `ReasoningSummary::{Auto, Concise, Detailed}` 序列化为 `auto`、`concise`、`detailed`。只有 Responses 使用此字段。

`reasoning` 对象的取值：

| 条件 | 发送内容 |
|---|---|
| `disable_thinking: true` | `{"reasoning":{"effort":"none"}}` |
| 设置了 `reasoning_summary` | 该 summary；`reasoning_effort` 非空时一并发送 effort |
| 未设置 `reasoning_summary`，`reasoning_effort` 非空 | effort 与 `summary: "concise"` |
| 两者皆无 | 不发送 `reasoning` 对象 |

`TokenUsage.cache_write_tokens` 与 `cache_write_tokens_details` 对应官方 `usage.input_tokens_details` 下的同名字段，只记录 Responses 的缓存写入量。

- 总量与明细各自保留，不求和补字段，也不从明细推算总量。
- 字段缺失或为 null 时读为 `None`，为 0 时读为 `Some(0)`。
- 明细类型 `CacheWriteTokensDetails` 含必填的 `ttl_30m_tokens` 与 `ttl_12h_tokens`；明细对象存在时两个计数都必须提供且合法。
- 流式完成与非流式共用解析。

字段依据：[官方 prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)、[官方 usage 类型](https://github.com/openai/openai-python/blob/main/src/openai/types/responses/response_usage.py)、[Reasoning 参数](https://github.com/openai/openai-python/blob/main/src/openai/types/shared_params/reasoning.py)。

## 3. Anthropic Messages API 适配器

代码位于 [`crates/vva-core/src/provider/anthropic/`](../crates/vva-core/src/provider/anthropic/)。

### 3.1 Prompt Caching

- 适配器在系统提示词尾部与长对话的历史消息分界点注入 `cache_control: {"type": "ephemeral"}` 缓存断点。
- 响应中的 `cache_creation_input_tokens` 与 `cache_read_input_tokens` 写入 `AnthropicTurnMeta`，供计费结算。
- `TokenUsage` 口径：`prompt_tokens = input_tokens`，`total_tokens = input_tokens + output_tokens`。缓存读同时计入 `cached_tokens`，缓存创建只计入 Anthropic meta。
- 流式 `message_delta.usage` 的 `input_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens` 覆盖已有累计值，不相加。字段缺失时保留已有值，0 覆盖，JSON null 不覆盖。
- 非流式路径使用相同口径。

覆盖语义的例子见[官方流式文档](https://platform.claude.com/docs/en/build-with-claude/streaming#streaming-request-with-web-search-tool-use)。

### 3.2 Thinking 签名

- 启用 Extended Thinking 的模型在返回思考内容时附带加密签名 `signature`。
- 回传历史时透传签名：`{"type": "thinking", "thinking": "...", "signature": "..."}`，满足 API 校验。
- 签名保存在 `ContentPart::Reasoning { content, signature }`，请求序列化时还原。

### 3.3 服务端工具：Web Search 与 Web Fetch

**声明**

- `enable_web_search` 开启时，在 `tools` 中声明 `web_search_<版本>` 与 `web_fetch_<版本>`，版本默认 `20260318`，可由 `annotation.web_tool_version` 修改。
- `_20260209` 起的版本默认经代码执行容器调用。vva 未声明 programmatic tool calling，因此注入 `allowed_callers: ["direct"]` 直连，否则 API 返回 400。
- 未接线 `web_fetch` 的中转端点会在声明带它时整组丢弃工具，连带 `web_search` 失效。模型注记 `web_fetch: false` 关闭 `web_fetch` 声明，只保留 `web_search`。

**执行与投影**

- `server_tool_use` 与 `web_search_tool_result` / `web_fetch_tool_result` 块由服务端产出，无需回传 `tool_result`。
- web_search 调用对转为 `WebSearchCall`：`Search` 动作，携带来源列表；错误结果按 `Failed` 上报。
- web_fetch 调用对转为 `WebSearchCall`：`OpenPage` 动作。
- 文本块中的 `web_search_result_location` 引用转为 `Annotation::WebSearchResultLocation`，以 `cited_text` 定位。

**原生块链回传**

assistant 的全部原生内容块存入 `AnthropicTurnMeta.content_blocks`，多轮请求逐块回传；缺失或改动即返回 400，与 Responses 的 `output_items` 策略相同。块类型为强类型 `AnthropicContentBlock`，含 `encrypted_content` / `encrypted_index` 等服务端加密字段与 `redacted_thinking`。

**流式事件**

- `server_tool_use` 保留 `content_block_start` 中的 `input`；有非空 `input_json_delta` 时用拼接后的 JSON 替换。
- 结果块在 `content_block_start` 整块到达。
- 引用经 `citations_delta` 逐条追加到当前 text 块。

**pause_turn 续包**

- 后续响应可只含 `web_search_tool_result` / `web_fetch_tool_result`。适配器从传入 `Session` 的 assistant `TurnMeta.anthropic.content_blocks` 提取同 scope 下尚未匹配结果的 `server_tool_use`，按 `tool_use_id` 关联历史 input 后投影。
- 同一响应内的调用与结果配对保持；缺失调用或工具名不匹配报协议错误。
- 流式与非流式处理相同。

**宿主续发**

- 宿主检查 `ProviderResponse.meta.anthropic.stop_reason == Some("pause_turn")`，把响应 messages、usage 与 meta 保存为 assistant Turn 追加到 Session，再以相同 affinity 调用 `Provider::complete` / `complete_stream`。
- 宿主不生成客户端 `tool_result`；只保存通用 messages 会丢失配对输入。
- ManagedSession 没有自动续发循环。

**端点能力诊断**

模型误以客户端 `tool_use` 调用服务端工具时（端点未实现 server tools 的典型症状），工具结果返回诊断：端点不支持原生服务端工具。

### 3.4 独立搜索

`StandaloneSearchTool::install(&provider, &mut options, &mut tools)` 把搜索改为独立子请求，用于搜索调用 ID 与结果 ID 不一致的端点。

```rust
let mut options = CompletionOptions {
    model,
    enable_web_search: true,
    enable_web_fetch: false,
    web_tool_version: Some("20250305".into()),
    ..Default::default()
};
StandaloneSearchTool::install(&provider, &mut options, &mut tools);
```

安装：

- 注册客户端 `web_search`，关闭主请求的原生搜索，清理同名的服务端预留。
- 按 `ToolRegistry::register` 的语义替换已有的同名客户端工具；安装后不再注册外置搜索。
- 捕获独立请求所需的模型、输出上限与 `web_tool_version`。
- `enable_web_search` 为 `false` 时不做任何修改。

宿主先选定搜索实现，联网开关与工具权限在安装前求值。应用的 `annotation.standalone_search` 映射到这一入口。

独立请求复用同一 Provider 的端点与凭据，只携带搜索词与搜索工具。它省略 thinking 与 `allowed_callers`，使用 `tool_choice: {"type":"auto"}` 与 `output_config: {"effort":"medium"}`。

安装后对主请求的影响：

- `enable_web_search` 为 `false`；`enable_web_fetch` 与 `web_tool_version` 保持原值。
- 主请求启用原生 fetch 时，`web_tool_version` 同时用于 fetch。宿主需选择两种工具都支持的版本，或按示例关闭原生 fetch。
- 版本默认值见 [config-reference §4.2](config-reference.md#42-annotation-注记)。

结果处理：

- 只提取文本与搜索结果的 URL、可选标题。
- 提取结果作为普通 `ToolResult` 文本回传，主会话按客户端调用 ID 关联调用与结果。
- 搜索原生块与加密字段不进入主会话。
- 搜索错误、缺少结果块、非法报文和流中断都作为工具错误返回；合法的空结果列表表示未命中。
- 原生 fetch 与外置 fetch 的配置不变。

### 3.5 流式工具参数解析错误

`complete_stream` 解析工具参数 JSON 失败时返回 `ProviderError::AnthropicToolInputJson`。

| 字段 | 含义 |
|---|---|
| `block_index` | SSE 的块索引 |
| `stop_reason` / `message_stop_seen` | 已收到的结束信息 |
| `source` | `classify()` 给出 JSON 错误类别，`line()` 与 `column()` 给出位置 |
| `raw_input` | 传给解析器的完整字符串；`Display` 与 `Debug` 都省略它 |

出错前已转发的正文保留在事件通道中，调用方可读取。

## 4. Google Gemini API 适配器

代码位于 [`crates/vva-core/src/provider/gemini/`](../crates/vva-core/src/provider/gemini/)。

### 4.1 Google Search Grounding

- 原生声明 `tools: [{ "google_search": {} }]`。
- Gemini 返回分离的 `groundingMetadata`：`groundingChunks` 是网页列表，`groundingSupports` 带字符索引范围。
- 适配器按 `groundingSupports` 的 `segment.startIndex` / `segment.endIndex` 对照 `groundingChunks` 索引，计算字符偏移，映射为 `Annotation::UrlCitation` 并注入对应的 `ContentPart::Text`。

### 4.2 代码沙盒与安全评级

- 内置 Python 沙盒的执行结果映射到 `GeminiTurnMeta.code_execution_result`。
- 安全拦截评级映射到 `GeminiTurnMeta.safety_ratings`。

### 4.3 Thought signatures

- 流式与非流式响应都把原生内容块保存到 `GeminiTurnMeta.content_parts`，同亲和组回传时保持原始顺序与块边界。
- `thoughtSignature` 随原始块回传，覆盖并行工具调用的首块签名、多步调用与空文本签名块，遵循 [Gemini 官方要求](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)。

## 5. SSE 流式事件解码器

[`sse.rs`](../crates/vva-core/src/provider/sse.rs) 的 `SseDecoder` 遵循 W3C 规范，为三家协议的流式解析提供底层支撑：

- 分块传输（Chunked）输入拆包；
- 多字节 UTF-8 跨包重组；
- CRLF / LF 换行归一化；
- 空行触发事件。

## 6. 结构化输出

`CompletionOptions.response_schema: Option<ResponseSchema { name, schema }>` 声明期望的 JSON Schema。各供应商用官方原生机制约束输出，返回的 text 即 JSON 文本，由调用方反序列化为强类型。拒答或截断的非法输出由上层报错。

| 供应商 | 请求字段 |
|---|---|
| Responses | `text.format = {type:"json_schema", name, schema, strict:true}` |
| Anthropic | `output_config.format = {type:"json_schema", schema}`，无需 beta header 与 `name` |
| Gemini | `generationConfig.responseMimeType="application/json"` 加 `generationConfig.responseJsonSchema` |
