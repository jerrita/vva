# 核心架构

## 1. 问题：最小公分母（LCD）

通用 coding harness 为抹平供应商差异，把请求退化为无状态的 `/v1/chat/completions`，丢失各家原生能力：

| 供应商 | 丢失的能力 |
|---|---|
| OpenAI Responses | 服务端链式 `response_id`，多轮需重复上传历史 |
| Anthropic | Prompt Caching（`cache_control`）；Thinking 块 `signature`，思考链无法原样回传而触发校验错误 |
| Gemini | 原生 Google Search Grounding 的字符偏移引用 |
| 跨供应商 | 切换模型时上下文格式割裂，无法接力 |

vva 在统一 Session 抽象之上保留各供应商原生特性。设计约束（统一存储、零兜底、强类型元数据）见 [AGENTS.md §2](../AGENTS.md#2-设计约束)。

## 2. 分层

```
+--------------------------------------------------------------+
|  交互层   main.rs · ui/ · web/ · agent_space/                  |
+------------------------------+-------------------------------+
                               v
+--------------------------------------------------------------+
|  调度层   harness/ (SessionManager · SessionTask · AgentEngine)|
|           tools/                                              |
+---------------+----------------------------+-----------------+
                v                            v
+-----------------------------+  +-----------------------------+
|  供应商适配层  provider/       |  |  存储层  storage/ (JSONL)    |
|  Responses·Anthropic·Gemini |  |  Session→Turn→Message→Part  |
+-----------------------------+  +-----------------------------+
          领域模型 model/ 与配置 config/ 贯穿各层
```

| 层 | 职责 | 详述 |
|---|---|---|
| 配置 `config/` | 分层加载、JSONC / 环境变量展开、`{provider}/{model}` 寻址。`resolve_model` 只做解析并返回 `ResolvedModel`，Provider 由 `create_provider()` 在发起调用时单独构造，统计价格、列模型清单等只读路径不创建适配器 | [config-reference](config-reference.md) |
| 领域模型 `model/` | 四层实体、多模态内容、引用注脚、轮次级强类型元数据 | [domain_model](domain_model.md) |
| 供应商适配 `provider/` | 实现 `Provider` trait 并声明 `ProviderCapabilities`；协议转换后经 SSE 输出标准 `AgentEvent` | [providers](providers.md) |
| 调度 `harness/` `tools/` | `AgentEngine` 组装 Provider、工具注册表与补全参数；`SessionTask` 是会话唯一所有者，顺序处理提交、配置变更与取消，驱动 Agent Loop 并落盘；`SessionManager` 只维护句柄表，CLI / TUI / Web / agent space 共用 | [background_tasks](background_tasks.md) |
| 存储 `storage/` | `JsonlSessionStore` 行级追加写，崩溃容灾，流式读取 | [storage](storage.md) |
| 交互 | CLI 单次执行与会话检查；TUI 与 Web 订阅 `SessionView`；agent space 驱动 IM | [ui](ui.md) · [web_sync](web_sync.md) · [agent](agent.md) |

## 3. 数据流

1. 交互层向 `ManagedSession` 提交输入，`SessionTask` 展开 `@` 引用并调用 `AgentEngine`；
2. Provider 以原生协议请求，流式增量转为 `AgentEvent`；
3. 事件驱动 `SessionTask` 更新草稿，并以 `SessionView` 快照经 `watch` 发布（33ms 节流）；
4. 工具调用由 `ToolRegistry` 执行，结果配对后进入下一次请求；
5. 每个完成的 Turn 追加写入 JSONL，供应商强类型元数据随 Turn 保存，下一次请求按原生协议回传。

后续新增供应商的步骤见 [AGENTS.md §4](../AGENTS.md#4-开发指南)。
