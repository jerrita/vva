//! Agent 执行过程的统一事件流词汇表。
//!
//! Provider 适配器只产生流式增量事件（Thought / Text / Citation），
//! Harness 调度层在此之上补充工具执行与生命周期事件，
//! UI 与 CLI 均由该单一事件流驱动渲染。

/// 单个 Agent 事件的增量语义
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thought(String),
    ToolExecuting {
        name: String,
        args: String,
    },
    ToolFinished {
        name: String,
        result: String,
        is_error: bool,
    },
    Text(String),
    Citation {
        title: String,
        url: String,
    },
    Finished,
    Error(String),
}
