#[cfg(all(
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
#[path = "test_session.rs"]
mod session_runtime;
#[allow(dead_code)]
#[cfg(all(
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
#[path = "../tests/support/session.rs"]
mod session_shared;
#[cfg(all(
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
pub(crate) mod session {
    pub(crate) use super::session_runtime::*;
    pub(crate) use super::session_shared::*;
}
#[allow(dead_code)]
#[cfg(feature = "response")]
#[path = "../tests/support/websocket.rs"]
pub(crate) mod websocket;

#[cfg(any(feature = "web-tools", feature = "response"))]
#[path = "../tests/support/http.rs"]
pub(crate) mod http;

#[cfg(any(feature = "response", feature = "anthropic", feature = "gemini"))]
#[path = "../tests/support/media.rs"]
pub(crate) mod media;
