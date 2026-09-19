//! Optional inter-agent mailbox for a litter of meow instances sharing one
//! filesystem (e.g. a mounted Docker volume). This IS the whole "network":
//! no sockets, no discovery protocol of our own — it deliberately mirrors the
//! property that gave rise to it, that Akuma boxes on the same host do not
//! get network isolation from each other (see docs/LITTER_EXPERIMENT.md).
//!
//! `SendMessage` drops a JSON envelope into a peer's inbox directory.
//! `ReadInbox` reads every envelope currently in your own and leaves them in
//! place (unlike `fs::tool_file_delete`, which does remove files — see
//! `docs/LITTER_EXPERIMENT.md` for why this module doesn't reuse that path),
//! so a round's messages just accumulate and the caller judges what's new
//! from the `round` field it wrote. `ListPeers` reads a roster file that an
//! external launcher — not meow — is responsible for writing; meow never
//! resolves a peer name to an address itself.
//!
//! All three refuse to run until `litter_agent_name` is set in
//! `/etc/meow/config`, which is what makes this "optional": an agent nobody
//! configured for the litter gets a clear error instead of silently reading or
//! writing someone else's mailbox.
//!
//! If `litter_hub_addr` is also set, all three transparently switch to the
//! `litter-hub` TCP relay (`hub` submodule) instead of this filesystem
//! mailbox — same tool surface, same `LitterMessage`-shaped output, only the
//! transport underneath changes. See `docs/LITTER_EXPERIMENT.md` "Where this
//! is headed" for why: a shared Docker volume works for agents on one host,
//! but the filesystem mailbox has no answer for agents that aren't sharing
//! one, and it was always the v0.

pub mod hub;
pub mod raft;

use alloc::string::String;
use alloc::format;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::{open, close, read_fd, write_fd, fstat, read_dir, mkdir_p, open_flags};

use crate::util::json_escape_to;
use super::mod_types::ToolResult;

pub const LITTER_ROOT: &str = "/litter";
const MAX_MESSAGE_SIZE: usize = 32 * 1024;

static AGENT_NAME_INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; the atomic flag guards initialization,
// same pattern as tools::context's SANDBOX/CURRENT.
static mut AGENT_NAME: Option<String> = None;

/// Called once at startup from `Config::litter_agent_name`.
pub fn set_agent_name(name: String) {
    unsafe { *core::ptr::addr_of_mut!(AGENT_NAME) = Some(name); }
    AGENT_NAME_INIT.store(true, Ordering::Release);
}

pub fn agent_name() -> Option<String> {
    if AGENT_NAME_INIT.load(Ordering::Acquire) {
        unsafe { (*core::ptr::addr_of!(AGENT_NAME)).clone() }
    } else {
        None
    }
}

/// A single mailbox entry. `round` is caller-supplied context (which debate
/// round produced it), not a sequence number the mailbox itself assigns.
pub struct LitterMessage {
    pub from: String,
    pub round: i64,
    pub body: String,
}

impl LitterMessage {
    pub fn write_json(&self, out: &mut String) {
        out.push_str("{\"from\":\"");
        json_escape_to(&self.from, out);
        out.push_str("\",\"round\":");
        out.push_str(&format!("{}", self.round));
        out.push_str(",\"body\":\"");
        json_escape_to(&self.body, out);
        out.push_str("\"}");
    }

    pub fn parse(json: &str) -> Option<LitterMessage> {
        let from = crate::json::string_at(json, &["from"])?;
        let body = crate::json::string_at(json, &["body"])?;
        let round = crate::json::number_at(json, &["round"]).unwrap_or(0);
        Some(LitterMessage { from, round, body })
    }
}

/// `to` is LLM-supplied and never sandbox-checked the way `tools::fs` paths
/// are (this module intentionally lives outside that sandbox, in `/litter`),
/// so it must be a bare token — without this, `to = "../../etc"` would let
/// SendMessage write anywhere `mkdir_p`/`open` can reach.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn inbox_dir(agent: &str) -> String {
    format!("{}/inbox/{}", LITTER_ROOT, agent)
}

fn require_agent_name() -> Result<String, ToolResult> {
    agent_name().ok_or_else(|| {
        ToolResult::err("Litter not configured: set litter_agent_name in /etc/meow/config")
    })
}

pub fn tool_send_message(to: &str, body: &str, round: i64) -> ToolResult {
    let from = match require_agent_name() {
        Ok(n) => n,
        Err(e) => return e,
    };
    if !is_valid_name(to) {
        return ToolResult::err("'to' must be a plain agent name (letters, digits, '-' or '_')");
    }
    if body.is_empty() {
        return ToolResult::err("SendMessage requires a non-empty body");
    }
    if body.len() > MAX_MESSAGE_SIZE {
        return ToolResult::err("Message body too large (max 32KB)");
    }

    if let Some(addr) = hub::hub_addr() {
        return hub::tool_send_message(&addr, &from, to, body, round);
    }

    let dir = inbox_dir(to);
    mkdir_p(&dir);

    let msg = LitterMessage { from: from.clone(), round, body: String::from(body) };
    let mut json = String::new();
    msg.write_json(&mut json);

    // `<uptime-us>-<sender>.json` sorts chronologically within one inbox and
    // can't collide between senders writing in the same tick.
    let ts = crate::util::now_us();
    let path = format!("{}/{}-{}.json", dir, ts, from);
    let fd = open(&path, open_flags::O_WRONLY | open_flags::O_CREAT | open_flags::O_TRUNC);
    if fd < 0 {
        return ToolResult::err(format!("Failed to open mailbox file: {}", path));
    }
    let n = write_fd(fd, json.as_bytes());
    close(fd);
    if n < 0 || n as usize != json.len() {
        return ToolResult::err("Failed to write mailbox message");
    }
    ToolResult::ok(format!("Sent to '{}' ({} bytes)", to, json.len()))
}

pub fn tool_read_inbox() -> ToolResult {
    let me = match require_agent_name() {
        Ok(n) => n,
        Err(e) => return e,
    };

    if let Some(addr) = hub::hub_addr() {
        return hub::tool_read_inbox(&addr, &me);
    }

    let dir = inbox_dir(&me);
    mkdir_p(&dir);

    let entries = match read_dir(&dir) {
        Some(e) => e,
        None => return ToolResult::err(format!("Failed to list inbox: {}", dir)),
    };

    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".json"))
        .map(|e| e.name)
        .collect();
    names.sort();

    if names.is_empty() {
        return ToolResult::ok(format!("Inbox for '{}' is empty", me));
    }

    let mut out = format!("Inbox for '{}' ({} message(s)):\n", me, names.len());
    for name in &names {
        let path = format!("{}/{}", dir, name);
        let fd = open(&path, open_flags::O_RDONLY);
        if fd < 0 {
            continue;
        }
        let size = match fstat(fd) {
            Ok(s) => s.st_size as usize,
            Err(_) => {
                close(fd);
                continue;
            }
        };
        if size == 0 || size > MAX_MESSAGE_SIZE {
            close(fd);
            continue;
        }
        let mut buf = alloc::vec![0u8; size];
        let n = read_fd(fd, &mut buf);
        close(fd);
        if n <= 0 {
            continue;
        }
        let text = match core::str::from_utf8(&buf[..n as usize]) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(m) = LitterMessage::parse(text) {
            out.push_str(&format!("\n[round {}] {}: {}\n", m.round, m.from, m.body));
        }
    }
    ToolResult::ok(out)
}

pub fn tool_list_peers() -> ToolResult {
    if let Some(addr) = hub::hub_addr() {
        return hub::tool_list_peers(&addr);
    }

    let path = format!("{}/roster.json", LITTER_ROOT);
    let fd = open(&path, open_flags::O_RDONLY);
    if fd < 0 {
        return ToolResult::err(format!(
            "No roster at '{}' \u{2014} the launcher hasn't written one yet",
            path
        ));
    }
    let size = match fstat(fd) {
        Ok(s) => s.st_size as usize,
        Err(_) => {
            close(fd);
            return ToolResult::err("Failed to stat roster");
        }
    };
    if size > MAX_MESSAGE_SIZE {
        close(fd);
        return ToolResult::err("Roster file too large");
    }
    let mut buf = alloc::vec![0u8; size];
    let n = read_fd(fd, &mut buf);
    close(fd);
    if n <= 0 {
        return ToolResult::err("Failed to read roster");
    }
    match core::str::from_utf8(&buf[..n as usize]) {
        Ok(s) => ToolResult::ok(String::from(s)),
        Err(_) => ToolResult::err("Roster file is not valid UTF-8"),
    }
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter tests ---\n");

    // write_json / parse round-trip
    total += 1;
    {
        let msg = LitterMessage { from: String::from("meow-a"), round: 3, body: String::from("hello peer") };
        let mut json = String::new();
        msg.write_json(&mut json);
        match LitterMessage::parse(&json) {
            Some(back) if back.from == "meow-a" && back.round == 3 && back.body == "hello peer" => passed += 1,
            other => libakuma::print(&format!("  [!] round-trip: got {:?}\n", other.map(|m| (m.from, m.round, m.body)))),
        }
    }

    // parse escapes/unescapes a body containing quotes and newlines
    total += 1;
    {
        let msg = LitterMessage { from: String::from("meow-b"), round: 0, body: String::from("line1\nline2 \"quoted\"") };
        let mut json = String::new();
        msg.write_json(&mut json);
        match LitterMessage::parse(&json) {
            Some(back) if back.body == "line1\nline2 \"quoted\"" => passed += 1,
            other => libakuma::print(&format!("  [!] escaping round-trip: got {:?}\n", other.map(|m| m.body))),
        }
    }

    // parse rejects a document missing required fields
    total += 1;
    {
        if LitterMessage::parse("{\"from\":\"meow-a\"}").is_none() { passed += 1; }
        else { libakuma::print("  [!] parse should reject a message with no body\n"); }
    }

    // parse defaults a missing round to 0 rather than failing
    total += 1;
    {
        match LitterMessage::parse("{\"from\":\"meow-a\",\"body\":\"hi\"}") {
            Some(m) if m.round == 0 => passed += 1,
            other => libakuma::print(&format!("  [!] missing round should default to 0: {:?}\n", other.map(|m| m.round))),
        }
    }

    // is_valid_name accepts plain tokens and rejects path traversal / separators
    total += 1;
    {
        let cases: &[(&str, bool)] = &[
            ("meow-a", true),
            ("meow_b2", true),
            ("", false),
            ("../etc", false),
            ("a/b", false),
            ("has space", false),
        ];
        let mut ok = true;
        for (name, want) in cases {
            if is_valid_name(name) != *want {
                ok = false;
                libakuma::print(&format!("  [!] is_valid_name({:?}): want {}\n", name, want));
            }
        }
        if ok { passed += 1; }
    }

    // Litter* tools refuse to run before an agent name is configured. This
    // must be the LAST test in the module: it does not reset AGENT_NAME_INIT
    // afterward (there is no reset primitive, by design — see set_agent_name),
    // so any test added below this point would observe a configured agent.
    total += 1;
    {
        if !AGENT_NAME_INIT.load(Ordering::Acquire) {
            let r = tool_read_inbox();
            if !r.success && r.output.contains("not configured") { passed += 1; }
            else { libakuma::print(&format!("  [!] unconfigured ReadInbox should fail clearly: success={} {:?}\n", r.success, r.output)); }
        } else {
            libakuma::print("  [!] skipped unconfigured-agent test: an earlier test already set an agent name\n");
        }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    let mailbox_failures = if passed == total { 0 } else { 1 };
    mailbox_failures + hub::run_tests()
}
