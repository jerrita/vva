mod assembly;
pub use assembly::{
    AgentAssembly, ExecutionPlan, FixedSetup, PreparedInput, SessionServices, SessionSetup,
    ToolExecution,
};
pub mod session;
#[cfg(test)]
pub(crate) mod testing;

pub use crate::event::AgentEvent;
pub use session::{
    InputMode, ManagedSession, Notice, NoticeKind, PendingInput, RunState, SessionManager,
    SessionView,
};
