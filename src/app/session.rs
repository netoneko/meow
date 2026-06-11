//! Session management.
//!
//! Each conversation lives in its own directory under `/tmp/meow/<id>/`
//! (sandbox-prefixed when running sandboxed). A session id is derived from the
//! wall clock and pid so concurrent `meow` invocations don't collide and the
//! id is easy to correlate with logs after the fact.

use alloc::format;
use alloc::string::String;

/// Sandbox-aware root directory holding every session.
pub fn sessions_root() -> String {
    let sandbox = crate::tools::get_sandbox_root();
    if sandbox == "/" {
        String::from("/tmp/meow")
    } else {
        format!("{}/tmp/meow", sandbox)
    }
}

/// Generate a reasonably-unique, filesystem-safe session id.
///
/// Combines a wall-clock stamp (seconds since epoch) with the pid. When the RTC
/// is unavailable (`time()` returns 0) we fall back to the monotonic uptime so
/// the id is still unique within a boot.
pub fn generate_session_id() -> String {
    let pid = libakuma::getpid();
    let micros = libakuma::time();
    let stamp = if micros != 0 {
        micros / 1_000_000
    } else {
        libakuma::uptime() / 1_000_000
    };
    format!("{:x}-{:x}", stamp, pid)
}

/// Directory that holds one session's files.
pub fn session_dir(id: &str) -> String {
    format!("{}/{}", sessions_root(), id)
}

/// On-disk conversation log path for a session.
pub fn conversation_path(id: &str) -> String {
    format!("{}/conversation.jsonl", session_dir(id))
}
