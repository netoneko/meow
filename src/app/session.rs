//! Session management.
//!
//! Each conversation lives in its own directory under `<root>/tmp/meow/<id>/`,
//! where `<root>` carries both the sandbox prefix and the `MEOW_HOME` scope.
//! A session id is derived from the wall clock and pid so concurrent `meow`
//! invocations don't collide and the id is easy to correlate with logs.

use alloc::format;
use alloc::string::String;

/// Root directory holding every session, scoped two ways.
///
/// **`MEOW_HOME` first**, because sessions are per-agent state and several
/// resident agents share one box's filesystem: an unscoped `/tmp/meow` puts
/// every agent's conversations in one directory, told apart only by the
/// `<secs>-<pid>` leaf — i.e. by a pid, not by who the agent is. Scoping it
/// gives `/agents/panther/tmp/meow`, beside that agent's `etc/meow/config`,
/// which is where the rest of its state already lives.
///
/// Unlike `config::scoped()`, the leading slash is kept: that helper returns
/// a path relative to `/` and works only because the agents are launched with
/// `cd /`. A session root is handed to `mkdir_p` and printed in logs, so it
/// is built absolute here rather than inheriting that assumption.
pub fn sessions_root() -> String {
    let sandbox = crate::tools::get_sandbox_root();
    let scope = crate::config::scope();
    let mut root = String::new();
    if sandbox != "/" {
        root.push_str(&sandbox);
    }
    if !scope.is_empty() {
        root.push('/');
        root.push_str(scope);
    }
    root.push_str("/tmp/meow");
    root
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
        crate::util::now_us() / 1_000_000
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
