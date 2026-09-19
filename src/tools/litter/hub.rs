//! Client half of the `litter-hub` TCP transport — the replacement for the
//! `/litter` filesystem mailbox described in `docs/LITTER_EXPERIMENT.md`
//! ("Where this is headed"). Opt-in via `litter_hub_addr` in
//! `/etc/meow/config` (see `Config`); when unset, `tools::litter::mod`'s
//! filesystem path is used instead, unchanged — this module is additive, not
//! a replacement of the working default.
//!
//! One request per connection: `TcpStream::connect`, write one framed
//! request, read one framed response, let `Drop` close the socket. Every
//! `meow` invocation that reaches this module is itself one-shot (`meow -c`),
//! so there's no connection to keep alive between calls, matching
//! `litter_wire`'s framing doc.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use libakuma::net::TcpStream;

use litter_wire::{decode_len_header, decode_response, encode_len_header, encode_request, encode_response, Message, Request, Response, MAX_FRAME_LEN};

use crate::tools::mod_types::ToolResult;

static HUB_ADDR_INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; the atomic flag guards initialization,
// same pattern as `tools::litter::AGENT_NAME`.
static mut HUB_ADDR: Option<String> = None;

/// Liveness bookkeeping for the WAYWARD state (`docs/LITTER_STATE_MACHINE.md`):
/// every successful hub round trip refreshes `LAST_ALIVE_US`; the agent loop
/// records a probe failure by *not* refreshing it. When `now - LAST_ALIVE`
/// passes `WAYWARD_TIMEOUT_US`, the tool wrappers fail fast instead of
/// letting the LLM's SendMessage hang forever in an undrained backlog.
static LAST_ALIVE_US: AtomicU64 = AtomicU64::new(0);
static LAST_ALIVE_INIT: AtomicBool = AtomicBool::new(false);
pub const WAYWARD_TIMEOUT_US: u64 = 15 * 1_000_000;

/// Called once at startup from `Config::litter_hub_addr`.
pub fn set_hub_addr(addr: Option<String>) {
    unsafe { *core::ptr::addr_of_mut!(HUB_ADDR) = addr; }
    HUB_ADDR_INIT.store(true, Ordering::Release);
    // Optimistically assume alive until the first probe proves otherwise —
    // a freshly started agent shouldn't refuse tools for 15s of stale-zero.
    mark_alive();
}

pub fn mark_alive() {
    LAST_ALIVE_US.store(crate::util::now_us(), Ordering::Release);
    LAST_ALIVE_INIT.store(true, Ordering::Release);
}

/// True when the hub has been silent past `WAYWARD_TIMEOUT_US`. The leader
/// (in-process state) never consults this — it can't be wayward about
/// itself.
pub fn unresponsive() -> bool {
    if !LAST_ALIVE_INIT.load(Ordering::Acquire) {
        return false;
    }
    crate::util::now_us().saturating_sub(LAST_ALIVE_US.load(Ordering::Acquire)) > WAYWARD_TIMEOUT_US
}

/// Seconds since the hub was last heard from — for fail-fast error text.
pub fn silent_for_secs() -> u64 {
    crate::util::now_us().saturating_sub(LAST_ALIVE_US.load(Ordering::Acquire)) / 1_000_000
}

/// `None` until `set_hub_addr` has run, then whatever it was set to
/// (including `None` if no hub is configured) — the same
/// "initialized vs. not yet" distinction `tools::litter::agent_name` makes,
/// even though this module only ever consults the value once initialized.
pub fn hub_addr() -> Option<String> {
    if HUB_ADDR_INIT.load(Ordering::Acquire) {
        unsafe { (*core::ptr::addr_of!(HUB_ADDR)).clone() }
    } else {
        None
    }
}

fn call(addr: &str, req: &Request) -> Result<Response, String> {
    call_addr(addr, req)
}

/// One request/response round trip to an explicit address — the pulse's
/// static-peer probing targets peers other than the configured hub, so
/// the address can't always come from config. Deadline-bounded at BOTH
/// ends: a hub that accepts but never answers costs this client 5s, not
/// forever, so the tick loop stays alive long enough to go WAYWARD and
/// re-elect (docs/LITTER_STATE_MACHINE.md).
pub fn call_addr(addr: &str, req: &Request) -> Result<Response, String> {
    let stream = TcpStream::connect(addr).map_err(|e| format!("hub connect to '{}' failed: {:?}", addr, e.kind()))?;
    let _ = libakuma::set_nonblocking(stream.as_raw_fd(), true);

    let payload = encode_request(req);
    let framed = super::serve::deadline::frame(&payload);
    if !super::serve::deadline::write_all(&stream, &framed, super::serve::deadline::IO_TIMEOUT_US) {
        return Err(format!("hub at '{}' did not accept the request in time", addr));
    }

    let mut header = [0u8; 4];
    if !super::serve::deadline::read_exact(&stream, &mut header, super::serve::deadline::IO_TIMEOUT_US) {
        return Err(format!("hub at '{}' went silent before answering", addr));
    }
    let len = decode_len_header(header);
    if len > MAX_FRAME_LEN {
        return Err(String::from("hub response frame exceeds MAX_FRAME_LEN"));
    }

    let mut body = alloc::vec![0u8; len as usize];
    if !super::serve::deadline::read_exact(&stream, &mut body, super::serve::deadline::IO_TIMEOUT_US) {
        return Err(format!("hub at '{}' went silent mid-answer", addr));
    }
    let text = core::str::from_utf8(&body).map_err(|_| String::from("hub response is not valid UTF-8"))?;

    let response = decode_response(text).map_err(|e| format!("hub response decode failed: {:?}", e))?;
    mark_alive();
    Ok(response)
}

/// Shared with `tools::litter::mod`'s filesystem path so `ReadInbox`'s output
/// reads the same either way regardless of which transport is configured.
pub fn format_inbox(who: &str, messages: &[Message]) -> String {
    if messages.is_empty() {
        return format!("Inbox for '{}' is empty", who);
    }
    let mut out = format!("Inbox for '{}' ({} message(s)):\n", who, messages.len());
    for m in messages {
        out.push_str(&format!("\n[round {}] {}: {}\n", m.round, m.from, m.body));
    }
    out
}

/// Called once at startup (`main.rs`, right after `set_hub_addr`) when both
/// `litter_hub_addr` and `litter_agent_name` are configured — this IS the
/// bootstrap process for a new litter member: a `meow -c` invocation has no
/// separate "join the litter" step distinct from "start running" (it's
/// one-shot; see `docs/LITTER_EXPERIMENT.md`'s "Debate protocol"), so
/// ensuring the hub knows this agent happens right where that startup
/// already is. `Join` is idempotent, so paying this one round trip on every
/// invocation (not just literally the first) is what lets a hub started with
/// no `--roster` at all still answer `ListPeers` correctly from the very
/// first agent that ever runs.
///
/// Best-effort and non-fatal: a hub that's briefly unreachable shouldn't stop
/// an agent from doing whatever else it was invoked to do (which may not
/// touch the litter at all). If the hub is genuinely down, the *real* error
/// surfaces naturally the moment `SendMessage`/`ReadInbox`/`ListPeers` is
/// actually used — this only prints a diagnostic so a persistently-failing
/// join isn't silent.
pub fn bootstrap(addr: &str, name: &str) {
    match call(addr, &Request::Join { name: String::from(name) }) {
        Ok(Response::Joined) => {}
        Ok(Response::Error { message }) => {
            libakuma::print(&format!("litter: hub join failed: {}\n", message));
        }
        Ok(other) => {
            libakuma::print(&format!("litter: hub returned an unexpected response to 'join': {:?}\n", other));
        }
        Err(e) => {
            libakuma::print(&format!("litter: could not reach hub at '{}' to join: {}\n", addr, e));
        }
    }
}

/// Structured (not pre-formatted) reads, shared by the `tool_*` wrappers
/// below and by `meow litter observe` (`main.rs::run_litter_observe`), which
/// needs every participant's messages merged, not one agent's inbox
/// formatted for the LLM.
/// The pulse: send `Peers { since }`, get the raw wire response back.
/// No intermediate view struct, no second serializer — callers decode the
/// `Response` they need and `ListPeers` re-encodes it verbatim (one JSON
/// shape, one serializer, everywhere).
pub fn peers(addr: &str, since: u64) -> Result<Response, String> {
    call(addr, &Request::Peers { since })
}

/// One page of an inbox, read backwards: messages with `ts < before`
/// (0 = from the newest end), at most `limit`, delivered ascending. A
/// cold-starting agent walks these batches back until it hits a `Marker`
/// kind message — the protocol IS the history source.
pub fn history(addr: &str, name: &str, before: u64, limit: u32) -> Result<Vec<Message>, String> {
    match call(addr, &Request::History { name: String::from(name), before, limit }) {
        Ok(Response::Inbox { messages }) => Ok(messages),
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("hub returned an unexpected response to 'history': {:?}", other)),
        Err(e) => Err(e),
    }
}

pub fn inbox_messages(addr: &str, name: &str) -> Result<Vec<Message>, String> {
    match call(addr, &Request::Inbox { name: String::from(name) }) {
        Ok(Response::Inbox { messages }) => Ok(messages),
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("hub returned an unexpected response to 'inbox': {:?}", other)),
        Err(e) => Err(e),
    }
}

pub fn tool_send_message(addr: &str, from: &str, to: &str, body: &str, round: i64) -> ToolResult {
    let req = Request::Send {
        from: String::from(from),
        to: String::from(to),
        body: String::from(body),
        round,
    };
    match call(addr, &req) {
        Ok(Response::Sent { bytes }) => ToolResult::ok(format!("Sent to '{}' ({} bytes)", to, bytes)),
        Ok(Response::Error { message }) => ToolResult::err(message),
        Ok(other) => ToolResult::err(format!("hub returned an unexpected response to 'send': {:?}", other)),
        Err(e) => ToolResult::err(e),
    }
}

pub fn tool_read_inbox(addr: &str, me: &str) -> ToolResult {
    match inbox_messages(addr, me) {
        Ok(messages) => ToolResult::ok(format_inbox(me, &messages)),
        Err(e) => ToolResult::err(e),
    }
}

pub fn tool_list_peers(addr: &str) -> ToolResult {
    match peers(addr, 0) {
        // The pulse response re-encoded verbatim: roster, leader/term,
        // epoch, and fresh cluster events — same shape, same serializer as
        // the wire itself.
        Ok(response @ Response::Peers { .. }) => ToolResult::ok(encode_response(&response)),
        Ok(_) => ToolResult::err("hub returned an unexpected response to 'peers'"),
        Err(e) => ToolResult::err(e),
    }
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter hub client tests ---\n");

    // hub_addr: set/get round-trips, and `None` is a legitimate *configured*
    // state once `set_hub_addr` has run at all (unlike `agent_name`,
    // `set_hub_addr` is called unconditionally at startup — see `main.rs` —
    // so by the time `meow test` reaches this suite `HUB_ADDR_INIT` is
    // already true; this only checks the round trip, not the pre-init state).
    total += 1;
    {
        set_hub_addr(Some(String::from("192.168.65.254:7700")));
        let after_set = hub_addr().as_deref() == Some("192.168.65.254:7700");
        set_hub_addr(None);
        let after_none = hub_addr().is_none() && HUB_ADDR_INIT.load(Ordering::Acquire);
        if after_set && after_none {
            passed += 1;
        } else {
            libakuma::print(&format!(
                "  [!] hub_addr: after_set={} after_none={}\n",
                after_set, after_none
            ));
        }
    }

    // format_inbox: empty vs. populated, matching the filesystem path's
    // "Inbox for 'x' is empty" / "[round R] from: body" shape exactly, so
    // ReadInbox reads the same regardless of which transport is configured.
    total += 1;
    {
        let empty = format_inbox("hercules", &[]);
        let populated = format_inbox(
            "hercules",
            &[Message::chat(
                String::from("sherlock"),
                2,
                String::from("what have you found?"),
                123,
            )],
        );
        if empty == "Inbox for 'hercules' is empty"
            && populated.contains("[round 2] sherlock: what have you found?")
        {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] format_inbox: empty={:?} populated={:?}\n", empty, populated));
        }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
