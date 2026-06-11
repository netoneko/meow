use alloc::string::String;
use libakuma::{open, close, write_fd, open_flags};
use crate::util::json_escape_to;

#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// JSON array string of tool calls, set on assistant messages that invoke tools
    pub tool_calls_json: Option<String>,
    /// Tool call ID, set on role:"tool" result messages
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: String::from(role),
            content: String::from(content),
            tool_calls_json: None,
            tool_call_id: None,
        }
    }

    pub fn write_json(&self, out: &mut String) {
        out.push_str("{\"role\":\"");
        out.push_str(&self.role);
        out.push('"');

        if let Some(ref tc_json) = self.tool_calls_json {
            out.push_str(",\"content\":null,\"tool_calls\":");
            out.push_str(tc_json);
        } else {
            out.push_str(",\"content\":\"");
            json_escape_to(&self.content, out);
            out.push('"');
        }

        if let Some(ref tc_id) = self.tool_call_id {
            out.push_str(",\"tool_call_id\":\"");
            out.push_str(tc_id);
            out.push('"');
        }

        out.push('}');
    }
}

pub const MAX_HISTORY_SIZE: usize = 100;

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Rough token cost of a single message, matching the per-message accounting the
/// old in-heap `calculate_history_tokens` used (content + role + tool_calls + 4).
fn message_tokens(msg: &Message) -> usize {
    let tc_tokens = msg.tool_calls_json.as_deref().map(estimate_tokens).unwrap_or(0);
    estimate_tokens(&msg.content) + estimate_tokens(&msg.role) + tc_tokens + 4
}

/// The conversation, backed by an on-disk JSONL log (one message object per
/// line) rather than an in-heap `Vec<Message>`. On a 6 MB box the resident
/// `Vec` was the one thing that grew turn-over-turn; the file is the source of
/// truth and only two small aggregates (`count`, `tokens`) stay in RAM.
///
/// Access pattern is append-only with an occasional truncate-and-reseed (on
/// `/clear` and context compaction) — nothing ever reads a message by index, so
/// the log never needs to be materialized back into memory. The request body is
/// streamed straight from this file (see api::client), one line at a time.
pub struct Conversation {
    path: String,
    session_id: String,
    count: usize,
    tokens: usize,
}

impl Conversation {
    /// Open a fresh conversation for `session_id`, creating its session
    /// directory under `/tmp/meow/<id>/` and truncating any prior log.
    pub fn new_session(session_id: String) -> Self {
        let dir = crate::app::session::session_dir(&session_id);
        libakuma::mkdir_p(&dir);
        let path = crate::app::session::conversation_path(&session_id);
        let fd = open(&path, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
        if fd >= 0 { close(fd); }
        Conversation { path, session_id, count: 0, tokens: 0 }
    }

    /// Allocate a brand-new session: pick a fresh id, create its directory,
    /// repoint the log there, and reseed it with `msgs`. Returns the new id.
    pub fn start_new(&mut self, msgs: &[Message]) -> String {
        let id = crate::app::session::generate_session_id();
        let dir = crate::app::session::session_dir(&id);
        libakuma::mkdir_p(&dir);
        self.path = crate::app::session::conversation_path(&id);
        self.session_id = id.clone();
        self.count = 0;
        self.tokens = 0;
        self.reseed(msgs);
        id
    }

    pub fn path(&self) -> &str { &self.path }
    pub fn session_id(&self) -> &str { &self.session_id }
    pub fn len(&self) -> usize { self.count }
    pub fn tokens(&self) -> usize { self.tokens }

    /// Append one message as a JSONL line. Returns false on write failure (a
    /// short/torn write leaves a line without a trailing '\n', which the request
    /// builder drops on read).
    pub fn append(&mut self, msg: &Message) -> bool {
        let fd = open(&self.path, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_APPEND);
        if fd < 0 { return false; }
        let mut line = String::new();
        msg.write_json(&mut line);
        line.push('\n');
        let n = write_fd(fd, line.as_bytes());
        close(fd);
        if n < 0 || n as usize != line.len() { return false; }
        self.count += 1;
        self.tokens += message_tokens(msg);
        true
    }

    /// Replace the entire conversation with `msgs` (truncate + rewrite). Used by
    /// `/clear` and context compaction.
    pub fn reseed(&mut self, msgs: &[Message]) -> bool {
        let fd = open(&self.path, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
        if fd < 0 { return false; }
        let mut count = 0usize;
        let mut tokens = 0usize;
        let mut line = String::new();
        let mut ok = true;
        for m in msgs {
            line.clear();
            m.write_json(&mut line);
            line.push('\n');
            let n = write_fd(fd, line.as_bytes());
            if n < 0 || n as usize != line.len() { ok = false; break; }
            count += 1;
            tokens += message_tokens(m);
        }
        close(fd);
        if ok {
            self.count = count;
            self.tokens = tokens;
        }
        ok
    }
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::format;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- history tests ---\n");

    // estimate_tokens
    let token_cases: &[(&str, usize)] = &[
        ("", 0), ("abcd", 1), ("abcde", 2), ("hello world!", 3),
    ];
    for (input, expected) in token_cases {
        total += 1;
        let got = estimate_tokens(input);
        if got == *expected { passed += 1; }
        else { libakuma::print(&format!("  [!] estimate_tokens({:?}): got {} want {}\n", input, got, expected)); }
    }

    // write_json basic
    total += 1;
    {
        let msg = Message::new("user", "hello");
        let mut out = String::new();
        msg.write_json(&mut out);
        let want = "{\"role\":\"user\",\"content\":\"hello\"}";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] write_json: got {:?}\n", out)); }
    }

    // write_json with escape sequences
    total += 1;
    {
        let msg = Message::new("assistant", "line1\nline2\ttab\"quote");
        let mut out = String::new();
        msg.write_json(&mut out);
        let want = "{\"role\":\"assistant\",\"content\":\"line1\\nline2\\ttab\\\"quote\"}";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] write_json escape: got {:?}\n", out)); }
    }

    // message_tokens returns > 0 for a non-empty message
    total += 1;
    {
        let tokens = message_tokens(&Message::new("user", "hello world"));
        if tokens > 0 { passed += 1; }
        else { libakuma::print("  [!] message_tokens returned 0\n"); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

