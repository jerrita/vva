# 原生供应商适配与转换规范 (Native Provider Adapters & Zero-Fallback Rule)

## 1. 适配器转换“零兜底”铁律 (Zero Fallback Rule)

代码位置：[`src/provider/`](../src/provider/)

所有适配器必须严格遵循以下铁律：
1. **纯正原生协议**：每个 Provider 模块必须严格且仅实现该厂商官方原生协议，绝对严禁退化为通用 Chat Completions 或跨厂商伪造报文；
2. **严禁字段别名容错猜测**：
   - 严禁出现类似 `"output_text" | "text"`、`"web_search_call" | "web_search"` 等多重别名猜测链；
   - 官方最新文档未定义的字段，绝对严禁在代码中臆造；
3. **单测 Mock 真实性铁律**：
   - 单元测试中的 Mock 报文必须 100% 严谨对照厂商官方文档最新规范构造，**严禁为了迎合手写错误的测试数据而在生产适配器代码中妥协添加容错分支**。

---

## 2. OpenAI Responses API 适配器

代码位置：[`src/provider/response.rs`](../src/provider/response.rs)

### 2.1 核心原生优势：服务端链式 `previous_response_id`
- **传统弊端**：传统 Chat Completions 无法保存服务端状态，第 10 轮对话必须重复上传前 9 轮的全部消息。
- **Responses 原生机制**：
  - 适配器在构建请求 Payload 时，自动从 Session 历史中定位最近一次有效的 Assistant 轮次，提取其 `response_id` 作为本次请求的 `previous_response_id`：
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
  - 仅需上传最新增量的用户消息与未结算的工具返回，大幅削减网络往返负载与 Token 消耗。

### 2.2 原生 Web Search 与注脚
- 原生启用 `"tools": [{ "type": "web_search_preview" }]`；
- 流式输出中解析 `response.web_search_call` 事件，记录查询状态；
- 返回的文本如果携带官方的 `url_citation` 注脚，直接提取 `url`, `title`, `start_index`, `end_index` 并封装入 `Annotation::UrlCitation`。

---

## 3. Anthropic Messages API 适配器

代码位置：[`src/provider/anthropic.rs`](../src/provider/anthropic.rs)

### 3.1 Prompt Caching (`cache_control`) 自动注入
- **原生机制**：Anthropic 要求在特定块注入 `{"type": "ephemeral"}` 标记缓存断点。
- **适配器行为**：
  - 在全局系统提示词（System Prompt）尾部自动追加 `cache_control`；
  - 在长对话的历史消息关键分界点自动维护断点标记；
  - 响应报文中精准提取 `cache_creation_input_tokens` 与 `cache_read_input_tokens` 并写入 `AnthropicTurnMeta`，供计费引擎结算 90% 的折扣。

### 3.2 Thinking 思考块数字签名 (`signature`) 保持
- **防篡改签名要求**：Anthropic 启用了 Extended Thinking 的模型（如 Claude 3.7 Sonnet）在返回思考内容时会随附一段加密签名 `signature`。
- **严禁丢弃铁律**：后续轮次将对话历史传回 Anthropic 时，思考块必须严格原样回传 `{"type": "thinking", "thinking": "...", "signature": "..."}`。若丢失 signature，Anthropic API 将直接报错拒绝服务。
- **vva 落地**：`ContentPart::Reasoning { content, signature }` 原生保真存储并在请求序列化时 100% 还原。

---

## 4. Google Gemini API 适配器

代码位置：[`src/provider/gemini.rs`](../src/provider/gemini.rs)

### 4.1 原生 Google Search Grounding 投影
- 原生启用 `tools: [{ "google_search": {} }]`；
- **字符偏移量精确映射**：Gemini 返回的不是内嵌注脚，而是分离的 `groundingMetadata`（包含 `groundingChunks` 网页列表与 `groundingSupports` 字符索引范围）；
- **适配器层计算与投影**：
  - 适配器根据 `groundingSupports` 中声明的 `segment.startIndex` 与 `segment.endIndex`，对照 `groundingChunks` 索引，在内存中直接计算字符偏移量；
  - 将分离的引用数据统一映射为标准 `Annotation::UrlCitation` 并注入对应的 `ContentPart::Text` 中。

### 4.2 代码沙盒与安全评级
- 映射 Gemini 内置 Python 沙盒执行结果至 `GeminiTurnMeta.code_execution_result`；
- 映射安全拦截评级至 `GeminiTurnMeta.safety_ratings`。

---

## 5. 通用 SSE 流式事件解码器

代码位置：[`src/provider/sse.rs`](../src/provider/sse.rs)

- 提供通用标准 Server-Sent Events 流式解码器（`SseDecoder`）；
- 严格遵循 W3C 规范：支持分块 Chunked 输入拆包、多字节 UTF-8 跨包重组、CRLF/LF 换行规范化以及空行事件触发机制；
- 为 Responses、Anthropic 与 Gemini 三大协议的流式解析提供稳定底层支撑。
