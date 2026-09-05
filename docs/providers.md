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
- 原生启用 `"tools": [{ "type": "web_search" }]`；
- 流式输出中解析 `response.web_search_call` 事件，记录查询状态；
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
