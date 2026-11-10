#[allow(dead_code)]
#[cfg(all(
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
#[path = "../tests/support/session.rs"]
pub(crate) mod session;
#[allow(dead_code)]
#[cfg(feature = "response")]
#[path = "../tests/support/websocket.rs"]
pub(crate) mod websocket;

#[cfg(feature = "web-tools")]
#[path = "../tests/support/http.rs"]
pub(crate) mod http;

#[cfg(any(feature = "response", feature = "anthropic", feature = "gemini"))]
#[path = "../tests/support/media.rs"]
pub(crate) mod media;
