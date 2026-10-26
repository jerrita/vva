//! Agent 执行过程的统一事件流词汇表。
//!
//! Provider 适配器产生流式内容事件（Thought / Text / Citation / WebSearchCall），
//! Harness 调度层在此之上补充工具执行与生命周期事件，
//! UI 与 CLI 均由该单一事件流驱动渲染。

use crate::model::{Annotation, ImageGenerationCall, UserQuestionRequest, WebSearchCall};

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
    AskUser(UserQuestionRequest),
    Text {
        /// Provider 分配的响应内文本部件标识，与引用共用。
        text_id: String,
        delta: String,
    },
    Citation {
        text_id: String,
        annotation: Annotation,
    },
    /// 原生搜索条目快照，同一 ID 的后续事件更新该条目。
    WebSearchCall(WebSearchCall),
    /// 原生图像生成调用，同一 ID 的后续事件更新该条目。
    ImageGenerationCall(ImageGenerationCall),
    /// 信息级通知（如能力协商降级说明），区别于 Error
    Notice(String),
    /// 当前配置下的能力提示全集。
    Capabilities(Vec<String>),
    /// 后台工具块的生命周期快照。
    BackgroundTaskUpdated(crate::model::BackgroundTask),
    ShellOutputUpdated {
        call_id: String,
        output: std::sync::Arc<crate::model::ShellOutput>,
    },
    Finished,
    /// 轮次被用户取消（区别于正常完成）
    Aborted,
    Error(String),
}
