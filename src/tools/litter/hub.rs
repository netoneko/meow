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
use core::sync::atomic::{AtomicBool, Ordering};

use libakuma::net::TcpStream;

use litter_wire::{decode_len_header, decode_response, encode_len_header, encode_request, Message, Request, Response, MAX_FRAME_LEN};

use crate::tools::mod_types::ToolResult;

static HUB_ADDR_INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; the atomic flag guards initialization,
// same pattern as `tools::litter::AGENT_NAME`.
static mut HUB_ADDR: Option<String> = None;

/// Called once at startup from `Config::litter_hub_addr`.
pub fn set_hub_addr(addr: Option<String>) {
    unsafe { *core::ptr::addr_of_mut!(HUB_ADDR) = addr; }
    HUB_ADDR_INIT.store(true, Ordering::Release);
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
    let stream = TcpStream::connect(addr).map_err(|e| format!("hub connect to '{}' failed: {:?}", addr, e.kind()))?;

    let payload = encode_request(req);
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&encode_len_header(payload.len() as u32));
    framed.extend_from_slice(payload.as_bytes());
    stream.write_all(&framed).map_err(|e| format!("hub write failed: {:?}", e.kind()))?;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header).map_err(|e| format!("hub read (header) failed: {:?}", e.kind()))?;
    let len = decode_len_header(header);
    if len > MAX_FRAME_LEN {
        return Err(String::from("hub response frame exceeds MAX_FRAME_LEN"));
    }

    let mut body = alloc::vec![0u8; len as usize];
    stream.read_exact(&mut body).map_err(|e| format!("hub read (body) failed: {:?}", e.kind()))?;
    let text = core::str::from_utf8(&body).map_err(|_| String::from("hub response is not valid UTF-8"))?;

    decode_response(text).map_err(|e| format!("hub response decode failed: {:?}", e))
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
    let req = Request::Inbox { name: String::from(me) };
    match call(addr, &req) {
        Ok(Response::Inbox { messages }) => ToolResult::ok(format_inbox(me, &messages)),
        Ok(Response::Error { message }) => ToolResult::err(message),
        Ok(other) => ToolResult::err(format!("hub returned an unexpected response to 'inbox': {:?}", other)),
        Err(e) => ToolResult::err(e),
    }
}

pub fn tool_list_peers(addr: &str) -> ToolResult {
    match call(addr, &Request::Peers) {
        Ok(Response::Peers { names }) => {
            let mut out = String::from("{\"agents\":[");
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(n);
                out.push('"');
            }
            out.push_str("]}");
            ToolResult::ok(out)
        }
        Ok(Response::Error { message }) => ToolResult::err(message),
        Ok(other) => ToolResult::err(format!("hub returned an unexpected response to 'peers': {:?}", other)),
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
        let populated = format_inbox("hercules", &[Message {
            from: String::from("sherlock"),
            round: 2,
            body: String::from("what have you found?"),
            ts: 123,
        }]);
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
