use alloc::string::String;
use alloc::format;
use alloc::vec;
use libakuma::{open, close, read_fd, write_fd, fstat, unlink, open_flags};
use crate::util::json_escape_to;
use crate::config::TOKEN_LIMIT_FOR_COMPACTION;
use crate::tools::context;

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
            // tc_id came from the model provider's SSE response (see
            // api::client::accumulate_tool_call_delta), not a compile-time
            // string, so it must be escaped like `content` above — an
            // unescaped `"` here would corrupt this JSONL line.
            out.push_str(",\"tool_call_id\":\"");
            json_escape_to(tc_id, out);
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

/// `resume_or_new`'s scan: `(line count, approximate tokens)` for an existing,
/// non-empty conversation file, or `None` if there is nothing to resume (file
/// absent, empty, or too large to trust — see below).
fn scan_existing(path: &str) -> Option<(usize, usize)> {
    let fd = open(path, open_flags::O_RDONLY);
    if fd < 0 {
        return None;
    }
    let stat = fstat(fd);
    let size = match stat {
        Ok(s) if s.st_size > 0 => s.st_size as usize,
        _ => {
            close(fd);
            return None;
        }
    };
    // A conversation worth resuming was left under the compaction cap by
    // whichever process wrote it last. One well past it (10x the token
    // limit's rough byte equivalent) is either a first run of this feature
    // against an old unbounded log, or a bug — either way, starting fresh is
    // the safe read, not trusting a number this scan cannot afford to
    // recompute exactly.
    if size > TOKEN_LIMIT_FOR_COMPACTION * 40 {
        close(fd);
        return None;
    }
    let mut buf = vec![0u8; size];
    let n = read_fd(fd, &mut buf);
    close(fd);
    if n <= 0 {
        return None;
    }
    let bytes = &buf[..n as usize];
    let mut count = 0usize;
    let mut tokens = 0usize;
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        count += 1;
        tokens += line.len().div_ceil(4);
    }
    if count == 0 {
        return None;
    }
    Some((count, tokens))
}

/// How many `tool_<n>.txt` spill files already exist in `dir`, for
/// `resume_or_new` to continue numbering past rather than overwrite. Probes
/// sequentially (`tool_1.txt`, `tool_2.txt`, …) rather than listing the
/// directory, because this target has no `readdir` — the names are
/// deterministic specifically so this works. Capped well above anything a
/// bounded, per-generation session should ever produce, so a bug elsewhere
/// shows up as a wrong count rather than an unbounded probe loop.
fn probe_tool_output_count(dir: &str) -> u64 {
    const PROBE_CAP: u64 = 100_000;
    let mut n = 0u64;
    while n < PROBE_CAP {
        let candidate = format!("{}/tool_{}.txt", dir, n + 1);
        let fd = open(&candidate, open_flags::O_RDONLY);
        if fd < 0 {
            break;
        }
        close(fd);
        n += 1;
    }
    n
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
        context::set_tool_output_dir(&dir, 0);
        Conversation { path, session_id, count: 0, tokens: 0 }
    }

    /// Reopen `session_id`'s conversation if one already has content —
    /// continuing across a process restart — or seed a fresh one with `seed`
    /// if there is nothing there yet.
    ///
    /// This is the resume path for a long-running identity (the live litter
    /// agent), where "restart" should mean "picked back up", not "amnesia" —
    /// every wake used to call `new_session` with a fresh id, which is
    /// correct for the interactive/CLI path (each invocation is its own
    /// conversation) but meant a resident agent that gets nudged, restarted,
    /// or rebooted relearned everything from zero every single time (see
    /// `LITTER_EXPERIMENT_PHASE_4.md` §8). Local `auto_compact_if_needed` is
    /// still what decides when *this* conversation resets — an existing file
    /// is trusted and continued exactly as `MAX_HISTORY_SIZE`/
    /// `TOKEN_LIMIT_FOR_COMPACTION` already govern it turn to turn; a
    /// cluster-level compaction signal, if one ever reaches this loop, would
    /// be an input to that same local decision, not a separate authority
    /// that truncates out from under it.
    ///
    /// The existing file is scanned once, here, to reconstruct the two
    /// in-memory aggregates (`count`, `tokens`) a resumed conversation needs
    /// before its first `append` — never re-parsed into `Message`s (see the
    /// struct's own note on why not). `tokens` is approximated from each
    /// line's raw byte length rather than reconstructed field-by-field: close
    /// enough to drive the same threshold `estimate_tokens` already
    /// approximates elsewhere, and parsing every line's JSON here would be
    /// exactly the "materialize it all back into memory" cost this type
    /// exists to avoid. The scan itself is bounded by the same compaction
    /// that bounds normal operation — a conversation this reads back is one
    /// that was already kept under `MAX_HISTORY_SIZE`/token cap by whichever
    /// process wrote it last.
    pub fn resume_or_new(session_id: String, seed: &[Message]) -> Self {
        let dir = crate::app::session::session_dir(&session_id);
        libakuma::mkdir_p(&dir);
        let path = crate::app::session::conversation_path(&session_id);

        if let Some((count, tokens)) = scan_existing(&path) {
            // Resuming with tool-output numbering reset to 0 would let the
            // next call's spill overwrite `tool_1.txt` while this resumed
            // history still points an earlier turn at it — probe for what is
            // already there (no `readdir` on this target) and continue past
            // it instead.
            context::set_tool_output_dir(&dir, probe_tool_output_count(&dir));
            return Conversation { path, session_id, count, tokens };
        }

        context::set_tool_output_dir(&dir, 0);
        let mut c = Conversation { path, session_id, count: 0, tokens: 0 };
        c.reseed(seed);
        c
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
        // A genuinely new directory — nothing to clean up in it, unlike
        // `reseed`, which repoints at the same session it was already in.
        context::set_tool_output_dir(&dir, 0);
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
        // Every tool-output spill file numbered under this session belonged
        // to the history this call is about to overwrite — delete them by
        // their deterministic names (no `readdir` on this target, so this is
        // the only way) and start the next generation's numbering at 0. This
        // is what actually bounds tool-output growth: files live exactly as
        // long as the conversation section that references them, not
        // forever (see `tools::mod_types::create_tool_tempfile`).
        if let Some(dir) = context::tool_output_dir() {
            for seq in 1..=context::tool_output_seq_count() {
                let _ = unlink(&format!("{}/tool_{}.txt", dir, seq));
            }
            context::set_tool_output_dir(&dir, 0);
        }

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

    // write_json escapes a provider-supplied tool_call_id containing a quote
    total += 1;
    {
        let mut msg = Message::new("tool", "result");
        msg.tool_call_id = Some(String::from("call\"1"));
        let mut out = String::new();
        msg.write_json(&mut out);
        let want = "{\"role\":\"tool\",\"content\":\"result\",\"tool_call_id\":\"call\\\"1\"}";
        if out == want { passed += 1; }
        else { libakuma::print(&format!("  [!] write_json tool_call_id escaping: got {:?}\n", out)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

