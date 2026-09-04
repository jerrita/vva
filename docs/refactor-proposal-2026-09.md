# vva 架构审查与重构提案（2026-09）

> **实施状态（2026-09-04 更新）**：P0 全部 7 项 ✅；P1 全部 ✅（新增 `provider/common.rs`、Provider trait 重设计、anthropic 统一解析、gemini 严格化、sse 清理）；P2 ✅（`Notice` 事件 + `supports_reasoning_effort` + 共享 `build_engine` 能力协商）；P3 ✅（`persist_header` 单点、`submit_prompt` 拆分与原子占位、`Aborted` 事件、锁外采集、`apply_overrides`/`new_session` 共享生命周期）；P4 的 a/b/c/e ✅，**P4d（markdown 解析/渲染分离 + 缓存）暂缓**（841 行渲染器无视觉回归测试护航，风险大于收益）；P5 ✅（死代码清理、`tools/shell.rs` 共享骨架 + 超时 + 输出上限、文档同步）。
> 未做项：P3 的"abort 时半持久化落盘"（需引擎级增量持久化架构调整）、P4d、`FilterableModal` 泛型 `FilteredList<T>` 化（收益小）。

> 审查范围：`src/` 全量（约 14,200 行）+ `docs/` + `Cargo.toml`。
> 结论形式：先列正确性问题（重构前先修），再按层列冗余/优化点，最后给分阶段重构方案。

---

## 0. 总体评价

架构方向是健康的：四层级联领域模型、JSONL 统一存储、`http.rs`/`sse.rs` 传输层共享、模态框状态机与视图分离——前几轮重构（`ff47ebc`、`833e6ea`）方向正确。

核心问题一句话：**抽象停在了传输层，协议语义层在三个适配器里各写了一遍且策略互不一致；"零兜底"纪律只在 `response.rs` 被认真执行，anthropic/gemini 的流式解析是全兜底风格。** 此外存在一批"声明了但没人用"的机制（`capabilities()`、`SseEvent.event`、死模型变体）和少量正确性 bug。

| 维度 | 评价 |
|---|---|
| 分层拓扑 | ✅ 清晰，无跨层泄漏大问题 |
| 传输层复用 | ✅ 三家适配器均复用 `post_json`/`post_sse`/`consume_sse` |
| 协议语义层 | ❌ 三份同构实现，严格度策略分裂 |
| 零兜底约束 | ⚠️ response 严格 / anthropic、gemini 宽松，约束执行不一致 |
| UI 状态 | ⚠️ App 36 字段上帝对象，状态双写 5 处散布 |
| 死代码 | ⚠️ 成批（能力协商、消息级 Meta、多模态变体、事件字段） |

---

## 1. 正确性问题（P0，重构前先修）

### 1.1 config 合并静默翻转 provider 协议 —— `src/config/model.rs:40`
`base_prov.provider_type = p_cfg.provider_type;` 无条件覆写。`ProviderType` 的 serde default 是 `Response`，因此项目配置对已存在的 provider 只覆写 `base_url` 而省略 `type` 字段时，全局的 anthropic/gemini provider 会被静默翻转为 response 协议。`base_url`/`api_key` 都有非空守卫，唯独枚举没有"未提供"哨兵。**修法**：合并源用 `Option<ProviderType>` 或自定义"未提供"哨兵，仅显式给出时覆写；补一条 merge 单测覆盖此场景。

### 1.2 anthropic/gemini 违反自身声明的背压契约 —— `src/provider/http.rs:13`
`SseEventHandler` 文档注释声明"保留 `send().await` 的背压语义，避免缓冲溢出丢事件"，但 `anthropic.rs:544,554` 与 `gemini.rs:467,487,493` 全部用 `tx.try_send`——UI 侧 mpsc 容量仅 64（`session_manager.rs:282`），渲染繁忙时通道满即**静默丢弃**思考/正文增量。仅 `response.rs` 遵守 await 语义。**修法**：统一改 `send().await`（handler 本身已是 async，无结构性障碍）。

### 1.3 工具参数解析失败被静默改写为 `{}` —— `anthropic.rs:179-181`、`gemini.rs:147-149`
`serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}))`：回传历史 ToolCall 时若 arguments 非法 JSON，静默改写成空对象发给 API，属于对协议 payload 的无声篡改。**修法**：解析失败返回 `ProviderError::Protocol`（contrast：`response.rs:172` 原样传字符串，是正确做法）。

### 1.4 Gemini 流式 grounding 多 chunk 覆盖丢失 —— `gemini.rs:440`
每次 `merge_grounding` 都 `self.search_call = Some(...)` 整体覆盖。Gemini 流式常在多个 chunk 上分别下发 groundingMetadata，前一 chunk 的 sources/queries 被静默覆盖，最终只留最后一份检索记录——这直接打击项目的核心卖点（Grounding 引用保真）。**修法**：改为合并语义（append sources/queries）。

### 1.5 `[DONE]` 哨兵 break 语义错误且位置不当 —— `src/provider/http.rs:64-66`
`break` 只跳出内层 `for ev in decoder.decode(&chunk)`，外层 `while let Some(chunk_res)` 继续读流。且 `[DONE]` 是 OpenAI 专有哨兵，放在通用层违反自家"通用层不含厂商语义"的分层。**修法**：哨兵判定移入 response.rs 的 handler（返回终止信号），通用层只认"流结束"；或至少用标志位正确终止外层循环。

### 1.6 全链路零超时 —— `src/provider/mod.rs:17-21`
`reqwest::Client::new()` 无任何超时；SSE 流挂起即整轮永久阻塞，harness 循环也无 timeout，仅 `max_iterations=15` 一道护栏。**修法**：`connect_timeout` + `read_timeout`（空闲超时）——**不要**设总超时，会误杀长 SSE 流。

### 1.7 零散小项
- `Cargo.toml:14` `anyhow` 全库零使用，直接删。
- 硬编码默认模型不一致：`main.rs:148` 回退 `"response/gpt-4.5-preview"`，`ui/mod.rs:121,131` 回退 `"openai/gpt-5.6-terra"`——两处不同、且 `"openai"`/`"response"` 前缀无法被示例配置（provider id 为 `"a"`）解析。
- `storage/jsonl.rs:141-144` `list_sessions` 静默跳过 header 损坏的会话，与 `docs/storage.md` "不吞没错误"的声明相悖（load 路径严格、list 路径吞错）。
- `system.rs` 的 `SystemInfo::collect()` 在持有 session 写锁时被调用（`session_manager.rs:186-188`），内部同步 spawn nvidia-smi/lspci 等（数百 ms），阻塞 executor 并卡住该会话全部并发操作。

---

## 2. Provider 层冗余（重构主战场）

### 2.1 三份结构同构的"构建器 + 模板 + 累积器"

| 重复内容 | 位置 | 相似度 |
|---|---|---|
| 构造器/端点/鉴权四件套 | `response.rs:22-52` / `anthropic.rs:20-51` / `gemini.rs:21-65` | ~85% |
| `complete()` 五行模板 | `response.rs:507-517` / `anthropic.rs:363-373` / `gemini.rs:295-310` | ~95% |
| `complete_stream()` 骨架（含逐字重复的 ACCEPT 头插入） | `response.rs:519-544` / `anthropic.rs:375-399` / `gemini.rs:312-333` | ~80% |
| 请求体构建器（turn→msg→part 三重循环骨架） | `response.rs:55-264` / `anthropic.rs:54-240` / `gemini.rs:68-255` | 60-70% |
| role 相邻合并闭包 | `anthropic.rs:82-99`（`append_blocks`） vs `gemini.rs:77-94`（`append_content`） | **~95% 逐行相同**，仅字段名 `content`/`parts` 不同 |
| 事件转发样板 `if let Some(tx)...` | 共 8 处（response×3 / anthropic×2 / gemini×3） | 100% 模板 |
| `ProviderResponse{...}` 终态组装 | 4 处 | 同构 |
| 工具定义转换 | 三家 ~90% 同构，仅字段名不同 | ~90% |

### 2.2 anthropic 流式/非流式双份解析 —— `anthropic.rs`
唯一没有统一解析路径的适配器（response 用 `parse_response` 复用于 `response.completed`，gemini 用 `GeminiAccumulator` 复用于两路）：
- 平行双枚举：`AnthropicContentBlockDto`（:318-335）与 `AnthropicBlock`（:464-477）字段结构完全相同；
- DTO→ContentPart 映射写两遍：:251-273（非流式）与 :411-434（流式）逐分支平行；
- usage + `AnthropicTurnMeta` 组装写两遍：:278-295 与 :436-451。

### 2.3 零兜底纪律执行分裂（这是设计约束层面的问题）
同一层级的协议字段，三种态度并存：
- **response.rs（严格，标杆）**：usage 字段缺失全部 `ok_or_else(Protocol)`（:422-461）；SSE 事件缺 `type` 返回 `Protocol` 错误（:557-559）。
- **anthropic.rs（全兜底）**：`json["type"].as_str().unwrap_or_default()`（:493）、`index...unwrap_or(0)`（:504，索引畸形时增量错写到 block 0 造成内容串扰）、tool_use id/name 兜底空串（:516-517，下游报 "Tool not found: " 掩盖真因）、未知 block 类型强转空 Text 块（:520-528，未来新增块类型被吞）、流式恒返回 `Some(usage)` 全零（:436-442，与非流式 `None` 语义不一致）。
- **gemini.rs（全兜底）**：usage 三字段 `unwrap_or(0)`（:550-552）、grounding uri/title 空串产出空 URL 引用（:422-423）、functionCall name 空串（:498）、functionResponse 查不到函数名时用 `tool_call_id` 冒充（:183，字段别名猜测链）、鉴权头构造 `if let Ok` 静默吞错（:61-63）。

按 AGENTS.md 自己的定义，这些大多是应上报 `Protocol` 错误的场景。

### 2.4 `capabilities()` 全库零消费 —— 死设计且掩盖 UX 失效
`provider/mod.rs:38-44,80` 定义的能力协商机制无任何调用方。直接后果：
- `enable_web_search=true` 时 response（:65-69）与 gemini（:215-219）注入原生搜索，**anthropic 静默忽略**（`anthropic.rs:54-240` 未引用该选项）——用户在 Claude 上开搜索无声失效；
- `reasoning_effort` 仅 response.rs:226-261 消费，anthropic/gemini 静默无效，而 `main.rs:291` / `session_manager.rs:270` 都会传入该值。

### 2.5 共享层小项
- `sse.rs` 的 `event:` 字段机制（:4-7,12,51-53,61-72）全链路死代码——`http.rs` 只读 `ev.data`，三个 handler 全按 `json["type"]` 分发。Anthropic 恰恰把事件名放 `event:` 字段，信息解析后被丢弃。
- `http.rs:67,75` 无法解析的事件行静默丢弃，无错误无日志，违背自家"解析失败即报协议错误"声明（注意保留对空行/keep-alive 的跳过）。
- `gemini.rs:51-53` API key 以查询参数拼进 URL（与 header 双通道），key 更容易进日志/错误消息。
- 适配器的 `build_request_body`/`parse_response` 标 `pub` 但仅本文件使用，API 面过宽。
- `gemini.rs:167-183` 手写三重 `find_map` 反查函数名，而 `session.rs:105-116` 已有 `find_tool_call`（response.rs:194 在用）。

---

## 3. 会话生命周期：双份（main.rs vs TUI）

| 重复逻辑 | main.rs | TUI 侧 |
|---|---|---|
| 会话恢复 + 三字段覆写 + save_header | :203-226 | `ui/mod.rs:89-116` |
| 新建会话 + System Turn + provisional title | :237-246 | `session_manager.rs:185-208`（语义还不一致：CLI 创建即设标题，TUI 延迟到首条消息） |
| 引擎组装（resolve→ToolRegistry→CompletionOptions→AgentEngine） | :256-297 | `session_manager.rs:261-276`（CompletionOptions 填充逐行相同） |

TUI 内部还有：
- **"写 session → 若 is_persisted 则 save_header"散布 5 处**（`ui/mod.rs:108-114`、`ui/mod.rs:462-469`、`app/session.rs:119-130`、`app/session.rs:211-222`、`app/dispatch.rs:82-89`）——新增可持久化字段要同步改 5 处。
- `submit_prompt` 约 170 行承担 5 个职责（`session_manager.rs:171-339`），且 :179-181 的 `is_running` check-then-act 在多个 await 之后才置位，并发提交可双跑同一会话。
- abort 用 `AgentEvent::Finished` 冒充完成（`session_manager.rs:71`），与正常完成不可区分；abort 时正在流式输出的 assistant turn 完全不落盘（半持久化状态）。

---

## 4. UI 层冗余

### 4.1 事件循环 `ui/mod.rs`（925 行）
- **Config modal 的 Activate 分支内联完整业务逻辑**（:444-511）：WebSearch/ThinkingDisplay toggle 与 `app/dispatch.rs:74-118` 的 `/search`、`/thinking` 命令处理逐行重复——事件循环承载了领域行为。
- Sessions/Models/Variants 三个 modal 分支结构完全相同（:363-434），可表驱动。
- `handle_crossterm_event` 仍约 420 行（:305-723）。
- 选区优先级级联链（input→text→sidebar）在 copy/escape/quit 三处重复实现（:738-784）。
- 每次循环克隆 5 个 String + 100ms tick 强制全量重绘，无脏标记。

### 4.2 modal 体系：交互已统一，数据与渲染未统一
- 4 份 `apply_filter` 是同一模板 copy-paste（`modals/models.rs:43-56`、`variants.rs:55-70`、`config.rs:86-99`、`sessions.rs:54-80`）。
- 4 份 FilterableModal impl 共 20 行纯样板转发 ×4。
- **4 个渲染函数各自重复约 40 行相同骨架**（`views/models_modal.rs:14-39`、`sessions_modal.rs:14-39`、`variants_modal.rs:13-37`、`config_modal.rs:13-37`）：`centered_rect` + `Clear` + `modal_block` + 三段垂直 Layout。`modal_shell.rs` 只抽了叶子原语，没抽布局骨架——这是 views/ 最大的剩余重复（约 160 行可收敛）。
- 列表渲染循环（表头 + `visible_window` + skip/take + 选中样式）在 4 个文件重复。

### 4.3 App 上帝对象 —— `ui/app/mod.rs:29-65`
36 个 pub 字段混合 9 类职责。最典型的冗余：三组 `Option<XSelection>` + `is_selecting_x` bool 成对存在（不变量冗余，6 字段 + 12 个三联重复方法在 `selection.rs:71-176`）。`enable_web_search` 与 `session.web_search` 双写镜像，需要专门测试守护同步（`app/tests.rs:224-292`）——双状态负担已被测试证实。
渲染函数反向写 App 状态（`chat.rs:330`、`sidebar.rs:133-138`、`input_area.rs:85-90` 写 `chat_area`/`sidebar_area` 等布局缓存），render 带 `&mut App` 副作用。

### 4.4 markdown：每帧全量重解析 —— 性能
`chat.rs:164` 每帧对全部 Assistant 消息执行完整 `preprocess_cjk_markdown` + pulldown-cmark 解析 + 折行；`selection.rs:132` 复制选区时又重建一遍。根因是解析与渲染耦合（无中间表示），无法缓存。`render_markdown` 单函数约 370 行（`markdown.rs:257-624`），`flush_paragraph` 以 5 个裸参数在约 10 个分支重复调用。
CJK 宽度计算三套机制并存（`UnicodeWidthStr` / `UnicodeWidthChar` / 每字符构造 `Span` 测宽）；`text.rs:4-20` `truncate` 按字符数而非显示宽度截断（CJK 标题会溢出 sidebar）。

### 4.5 history 投影分叉
`ChatEntry::ToolResult.name` 在持久化投影中填 `tool_call_id`（`history.rs:164-169`）、流式路径填真实工具名（`dispatch.rs:188-197`），渲染时以 `name: _` 丢弃（`chat.rs:115`）——字段实际无用。流式累积条目与重载后 `entries_from_turns` 产出是同一段对话的两套表示，Thinking 合并/citations 归并行为存在分叉。

---

## 5. 死代码清单（建议成批删除）

| 项 | 位置 | 说明 |
|---|---|---|
| `capabilities()` / `ProviderCapabilities` | `provider/mod.rs:38-44,80` | 零消费（见 2.4——先激活再谈，或删） |
| `SseEvent.event` / `current_event` | `sse.rs:4-7,12,51-53` | 全链路死机制 |
| `AnthropicMessageMeta` / `GeminiMessageMeta` | `meta.rs:205,231` | 零构造点；`anthropic.rs:103-108` 读取的 `ephemeral_cache` 恒 false |
| `ContentPart::Document/Audio/Video` | `content.rs:31-50` | 零 provider 实现，唯一"消费"是 inspect 的 Debug 打印 |
| `ImageDetail` 三变体 / `Annotation::FileCitation` / `SearchStatus::Pending/Failed` | `content.rs:121-125,137,183-188` | 从未构造 |
| `ContentPart::image_path/document_path` / `MediaSource::Path` | `content.rs:89-107,116` | 零调用 |
| `Session::last_turn()` | `session.rs:68` | 零调用 |
| `ProviderError::Serialization` | `provider/mod.rs:33` | 无构造路径 |
| `FilterableModal::selected_index()` | `modals/mod.rs:29` | 无调用方 |
| `AnthropicTurnMeta.stop_reason` / `ResponseTurnMeta.service_tier` 等 | `meta.rs:55-57,201,227` | 只写不读 |
| `anyhow` 依赖 | `Cargo.toml:14` | 零使用 |

注意：删除 `AnthropicMessageMeta`/`GeminiMessageMeta` 与 `docs/domain_model.md` 的记载冲突，需同步更新文档；或者反过来把 `ephemeral_cache`/`thought_signature` 真正实现掉——这是一个功能决策点。

---

## 6. 重构提案（分阶段）

> 通用方法论：每阶段先为现有行为补**请求 payload 快照测试**（固定 fixture session → 断言逐字节 JSON），重构后保持绿色。零兜底约束限制的是"不猜测协议字段"，**不是**"不能共享机械代码"——共享结构、保留语义。

### 阶段 P0：修正确性（约 1 天，无 API 变更）
1. config merge：`provider_type` 仅显式提供时覆写 + 单测。
2. anthropic/gemini 统一 `send().await`。
3. anthropic/gemini arguments 解析失败报 `Protocol`。
4. gemini `merge_grounding` 改合并语义。
5. `[DONE]` 处理移出通用层并正确终止流。
6. reqwest `connect_timeout` + `read_timeout`（禁止总超时）。
7. 删 `anyhow`；统一默认模型回退到单一常量（或直接要求配置 default）；`list_sessions` 对损坏 header 至少 eprintln 告警。

### 阶段 P1：Provider 语义收敛（约 2-3 天）
在 `provider/` 增加 `common.rs`（或并入 http.rs），抽取**机械重复**，不动各家专有语义：
1. `EventForwarder`：封装 8 处 `if let Some(tx)...` 转发样板，内置 send().await。
2. **anthropic 解析统一**（最高价值）：流式累积器产出与非流式 DTO 相同的块类型，`blocks_to_parts` + `build_turn_meta` 单份实现，删除平行枚举与双份映射（约 -80 行）。目标形态：每个适配器"一个解析器、两个入口"（gemini 模式）。
3. `Provider` trait 增加 `endpoint_url`/`auth_headers`，`complete()` 变 trait 默认方法（三份五行模板归一）。
4. role 相邻合并泛型化：`anthropic.rs:82-99` 与 `gemini.rs:77-94` 的 95% 逐行相同闭包 → 单一 `merge_adjacent(blocks, content_key)`。
5. 工具定义转换辅助（三份 90% 同构 → 参数化字段名）。
6. usage 解析策略统一到 response.rs 的严格标准（`require_u64` 辅助 + `Protocol` 错误），清理 2.3 列出的 anthropic/gemini 兜底群。
7. **明确不做**：不把三个请求体构建器统一成一个带钩子的泛型序列化器——`previous_response_id` 裁剪、cache_control、systemInstruction 等专有逻辑必须留在各自文件，强行统一会重蹈 LCD 覆辙。
8. `sse.rs` 删除 `event:` 死机制（或留给 anthropic 事件名消费，二选一）。

### 阶段 P2：能力协商激活（约 0.5 天，二选一）
- **方案 A（推荐）**：激活 `capabilities()`——`session_manager`/`dispatch` 在用户开启 web_search / 设置 reasoning_effort 前检查能力，不支持时发 `SystemNotice` 显式告知降级，而非静默失效。三个实现已存在，只差消费方。
- 方案 B：删除整套机制（YAGNI），anthropic 静默忽略的行为至少要在 UI 文案中说明。

### 阶段 P3：会话生命周期收敛（约 1-2 天）
1. `ManagedSession` 增加 `persist_header()` 方法：5 处"写 session → save_header"归一。
2. session_manager 暴露三个生命周期函数：`create_session`（system prompt + 标题策略统一）、`restore_session`（字段覆写）、`build_engine`——main.rs 与 TUI 共用，删除双份实现。
3. `AgentEvent` 增加 `Aborted` 变体，abort 不再冒充 `Finished`；abort 时将已累积的 assistant 部分内容落盘（半持久化修复）。
4. `submit_prompt` 拆分（锁外采集 SystemInfo，消除 check-then-act 竞态：提交前先原子置位 running 标志）。

### 阶段 P4：App 与 UI 渲染收敛（约 2-3 天）
1. `SelectionTracker { selection: Option<TextSelection>, selecting: bool }` ×3 用途：6 字段 + 12 方法 → 3 字段 + 1 impl。
2. 布局缓存（`chat_area`/`sidebar_area`/`input_inner_area`/`sidebar_lines`/viewport 系列）分组为 `ViewLayout` 子结构；中期目标：render 返回布局而非反写 App。
3. 4 个 modal Option 字段收进 `ModalStates` 子结构；命令浮层（`matched_commands` + index）封装为 `CommandPalette`。
4. `enable_web_search` 单一真相源 = session 字段，删除 App 镜像与同步测试。
5. modal 渲染骨架上提 `modal_shell::render_modal_shell(...)`：4×40 行 → 1 份（保留各 modal 的行渲染闭包）。
6. Config modal 的 Activate 业务逻辑移入 `app/`（与 `/search`、`/thinking` 共用同一函数）；Sessions/Models/Variants 分支表驱动。
7. markdown 解析/渲染分离：`parse_markdown(&str) -> MarkdownDoc`（IR），App 按 entry 身份缓存 IR，复制路径复用缓存；`render_markdown` 按 IR 分发拆小。
8. CJK 宽度统一到单一模块（`UnicodeWidthChar`），`truncate` 改按显示宽度。

### 阶段 P5：清理与文档（约 1 天）
1. 按 §5 清单成批删死代码（payload 快照测试兜底）。
2. bash/powershell 抽共享 `run_shell` 骨架 + 命令超时 + 输出截断（当前无上限，模型可执行无限期命令/撑爆上下文）。
3. 文档同步：AGENTS.md 目录树补 `system.rs`/`markdown.rs`/`app/` 细化；`docs/ui.md` 中 `app.rs` 引用已过时；若删除 MessageMeta 需同步 `domain_model.md`。

### 优先级总览

| 阶段 | 价值 | 风险 | 工作量 |
|---|---|---|---|
| P0 修 bug | 高（正确性） | 低 | ~1 天 |
| P1 provider 收敛 | 高（-300 行级重复 + 策略统一） | 中（需快照测试护航） | ~2-3 天 |
| P2 能力协商 | 高（消除静默失效 UX bug） | 低 | ~0.5 天 |
| P3 生命周期 | 中高（消除 5 处双写） | 中 | ~1-2 天 |
| P4 App/UI | 中（可维护性 + 渲染性能） | 中 | ~2-3 天 |
| P5 清理 | 中（-200 行死代码） | 低 | ~1 天 |

**明确不做**（避免过度重构）：三个请求体构建器的完全泛型统一；meta.rs 序列化 derive 的宏抽象；`FilterableModal` 的泛型 `FilteredList<T>` 化（收益小，等 P4 顺手做）。
