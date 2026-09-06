# vva 技术架构与设计文档库

本文档库记录 **vva (Vendor-Versatile Agent Harness)** 的技术设计、核心领域模型、存储引擎规范、供应商原生适配协议以及终端交互架构。

---

## 核心设计理念

通用 AI Coding Harness 存在**“最小公分母”（Lowest Common Denominator, LCD）**问题：
为了兼容简单协议，把模型退化为无状态的 `/v1/chat/completions`，丢失了 OpenAI Responses API 服务端上下文链、Anthropic Prompt Caching 与 Thinking 签名、Gemini 原生 Grounding 搜索引文等供应商特性。

**vva** 的设计目标是在保持统一 Session 交互抽象的同时，**支持各供应商原生特性**，并执行**“统一存储与零兜底”原则**。

---

## 文档目录索引

1. **[核心架构与设计原则 (architecture.md)](architecture.md)**
   - 痛点分析与“最小公分母”（LCD）陷阱
   - “统一存储与零兜底”原则 (Zero Fallback Rule)
   - 系统整体分层与数据流转拓扑
   - 强类型供应商元数据设计

2. **[领域模型与强类型体系 (domain_model.md)](domain_model.md)**
   - 四层级联领域实体：`Session` $\to$ `Turn` $\to$ `Message` $\to$ `ContentPart`
   - 多模态一等公民设计（文本、深度思考、文件/图像/音视频、工具调用与结果、网络检索）
   - 结构化引用注脚系统 (`Annotation::UrlCitation`, `Annotation::FileCitation`)
   - OpenAI Responses / Anthropic / Gemini 专属强类型元数据定义

3. **[JSONL 追加写存储引擎 (storage.md)](storage.md)**
   - 存储文件规范：`~/.vva/sessions/{project_key}/{session_id}.jsonl`，项目级 `scratch/`
   - `JsonlRecord::Header` 与 `JsonlRecord::Turn` 双记录布局
   - 追加写安全性、进程中断容灾与流式加载

4. **[原生供应商适配与转换规范 (providers.md)](providers.md)**
   - 适配器零兜底与单测 Mock 真实性约束
   - OpenAI Responses API：`previous_response_id` 链式状态继承与增量传输
   - Anthropic Messages API：Prompt Caching (`cache_control`) 与 Thinking 块数字签名 (`signature`)
   - Google Gemini API：原生 `google_search` Grounding 字符偏移量投影与代码沙盒结果映射
   - 通用 SSE 流式事件解码器 (`sse.rs`)

5. **[统一配置与模型寻址系统 (configuration.md)](configuration.md)**
   - JSON 与 JSONC 语法支持及词法注释剥离
   - 系统环境变量插值展开 (`${VAR}`)
   - `{provider_id}/{model_id}` 模型寻址与协议覆写机制
   - 推理强度档位（variants）校验与 URL 端点自动规范化
   - 内置供应商预设（`glm-coding-plan` 仅填 api_key）

6. **[现代化终端用户界面设计 (ui.md)](ui.md)**
   - 会话任务单一所有权与 `SessionView` 快照订阅
   - `App` 顶层响应式状态机与非阻塞事件主循环
   - 独立的 `InputBuffer` 多行文本编辑器（CJK宽字符视觉对齐、Unicode边界保护、视口滚动）
   - 模态弹窗状态机与视图层解耦（`sessions`, `models`, `variants`, `config`）
