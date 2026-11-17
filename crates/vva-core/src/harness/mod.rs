mod assembly;
pub use assembly::{
    AgentAssembly, ExecutionPlan, FixedSetup, PreparedInput, SessionServices, SessionSetup,
    ToolExecution,
};
pub mod session;

pub use crate::event::AgentEvent;
pub use session::{
    InputMode, ManagedSession, Notice, NoticeKind, PendingInput, RunState, SessionManager,
    SessionView,
};
