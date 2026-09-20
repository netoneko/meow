//! Optional inter-agent mailbox for a litter of meow instances. The
//! filesystem mailbox that once lived here is retired: the litter is
//! **network-bound** (see `docs/LITTER_STATE_MACHINE.md`), the hub socket
//! is the only channel, and all Litter* tools route through
//! `tools::litter::hub` — client side when this process doesn't hold the
//! socket, and (in-process, via the raft thread) when it does.
//!
//! All three tools refuse to run until `litter_agent_name` is set in the
//! config, which is what makes this "optional": an agent nobody configured
//! for the litter gets a clear error instead of silently reading or
//! writing someone else's mailbox.
//!
//! Failure behavior is the WAYWARD state's tool surface (`hub::unresponsive`):
//! a hub that went silent makes every tool fail fast with a clear message
//! rather than hanging the LLM's turn in an undrained backlog.

pub mod hub;
pub mod live;
pub mod membership;
pub mod observe;
pub mod raft;
pub mod record;
pub mod relay;
pub mod serve;
pub mod sig;
pub mod tasks;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::util::json_escape_to;
use super::mod_types::ToolResult;

static AGENT_NAME_INIT: AtomicBool = AtomicBool::new(false);
// Safety: single-threaded userspace; the atomic flag guards initialization,
// same pattern as tools::context's SANDBOX/CURRENT.
static mut AGENT_NAME: Option<String> = None;

/// `litter_static_peers` from the config, verbatim (`name@host:port,…`) —
/// peers the raft thread probes periodically so agents on the other host
/// (the trashcan/laptop split) show up as discovered/lost status changes
/// instead of not existing. Parsed in `live`.
static mut STATIC_PEERS_SPEC: Option<String> = None;
static STATIC_PEERS_INIT: AtomicBool = AtomicBool::new(false);

/// Called once at startup from `Config::litter_static_peers`.
pub fn set_static_peers_spec(spec: Option<String>) {
    unsafe { *core::ptr::addr_of_mut!(STATIC_PEERS_SPEC) = spec; }
    STATIC_PEERS_INIT.store(true, Ordering::Release);
}

pub fn static_peers_spec() -> Option<String> {
    if STATIC_PEERS_INIT.load(Ordering::Acquire) {
        unsafe { (*core::ptr::addr_of!(STATIC_PEERS_SPEC)).clone() }
    } else {
        None
    }
}

/// `litter_name` from the config — this litter's own identity on the relay
/// plane. Messages this hub forwards to a remote litter travel as
/// `<litter_name>-<agent>`; relay is off while this is unset.
static mut LITTER_NAME: Option<String> = None;
static LITTER_NAME_INIT: AtomicBool = AtomicBool::new(false);

/// Called once at startup from `Config::litter_name`.
pub fn set_litter_name(name: Option<String>) {
    unsafe { *core::ptr::addr_of_mut!(LITTER_NAME) = name; }
    LITTER_NAME_INIT.store(true, Ordering::Release);
}

pub fn litter_name() -> Option<String> {
    if LITTER_NAME_INIT.load(Ordering::Acquire) {
        unsafe { (*core::ptr::addr_of!(LITTER_NAME)).clone() }
    } else {
        None
    }
}

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

/// `to`/`from` are LLM-supplied and land directly in hub-side lookups, so
/// they must be bare tokens — the same rule litter-wire enforces on the
/// other end of the wire; kept here so a rejection is a clean tool error
/// instead of a round trip.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

const MAX_MESSAGE_SIZE: usize = 32 * 1024;

fn require_agent_name() -> Result<String, ToolResult> {
    agent_name().ok_or_else(|| {
        ToolResult::err("Litter not configured: set litter_agent_name in the config")
    })
}

/// Route the Litter* tools to the configured hub. No staleness gate here,
/// deliberately: a long LLM turn is NOT hub silence, and gating on
/// time-since-last-call latched the whole turn shut the first time a model
/// thought for 15 seconds (observed live). Every client call is
/// deadline-bounded (5s) in `hub::call_addr`, so a genuinely dead hub
/// costs each tool call 5s and a clean error — and the tick loop's pulse
/// is what drives the WAYWARD transition, not the tools.
fn hub_gate() -> Result<String, ToolResult> {
    hub::hub_addr().ok_or_else(|| {
        ToolResult::err("Litter hub not configured: set litter_hub_addr in the config")
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
    let addr = match hub_gate() {
        Ok(a) => a,
        Err(e) => return e,
    };
    hub::tool_send_message(&addr, &from, to, body, round)
}

/// `TaskUpdate` — the single public verb for every per-sub-task act.
///
/// One tool with a `status` enum rather than five (`claim`/`done`/`failed`/
/// `clear`/`reopen`/`artifact`): the acts share their arguments, a small
/// model picks a value more reliably than it picks among near-identical
/// tool names, and a new act — `failed` was the first — costs a value
/// rather than new surface.
pub fn tool_task_update(task: &str, status: &str, text: &str) -> ToolResult {
    let from = match require_agent_name() {
        Ok(n) => n,
        Err(e) => return e,
    };
    let Some(act) = litter_wire::TaskAct::parse(status) else {
        return ToolResult::err(
            "'status' must be one of: claim, done, failed, clear, reopen, artifact",
        );
    };
    if matches!(act, litter_wire::TaskAct::Plan) {
        return ToolResult::err("use the TaskPlan tool to plan a task");
    }
    if text.len() > MAX_MESSAGE_SIZE {
        return ToolResult::err("'text' too large (max 32KB)");
    }
    let addr = match hub_gate() {
        Ok(a) => a,
        Err(e) => return e,
    };
    hub::tool_task(&addr, &from, litter_wire::TaskOp::new(act, String::from(task), String::from(text)))
}

/// `TaskPlan` — the leader splitting a parent into directed sub-tasks.
///
/// Separate from `TaskUpdate` because it is the one act with a different
/// shape: a list of (assignee, brief) pairs, submitted **atomically**. A
/// plan that arrived in pieces would leave the table unable to tell that
/// planning had finished, and "all sub-tasks cleared" — the trigger for the
/// final artifact — would never fire.
pub fn tool_task_plan(task: &str, assignments: &[(String, String)]) -> ToolResult {
    let from = match require_agent_name() {
        Ok(n) => n,
        Err(e) => return e,
    };
    if assignments.is_empty() {
        return ToolResult::err("TaskPlan needs at least one assignment");
    }
    let addr = match hub_gate() {
        Ok(a) => a,
        Err(e) => return e,
    };
    let op = litter_wire::TaskOp {
        act: litter_wire::TaskAct::Plan,
        id: String::from(task),
        text: alloc::string::String::new(),
        plan: assignments.to_vec(),
    };
    hub::tool_task(&addr, &from, op)
}

pub fn tool_read_inbox() -> ToolResult {
    let me = match require_agent_name() {
        Ok(n) => n,
        Err(e) => return e,
    };
    let addr = match hub_gate() {
        Ok(a) => a,
        Err(e) => return e,
    };
    hub::tool_read_inbox(&addr, &me)
}

pub fn tool_list_peers() -> ToolResult {
    let addr = match hub_gate() {
        Ok(a) => a,
        Err(e) => return e,
    };
    hub::tool_list_peers(&addr)
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter tests ---\n");

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
                libakuma::print(&alloc::format!("  [!] is_valid_name({:?}): want {}\n", name, want));
            }
        }
        if ok {
            passed += 1;
        }
    }

    // static peers spec parsing (via live::parse_static_peers) — smoke:
    // valid entries survive, garbage is dropped
    total += 1;
    {
        let spec = Some(String::from("trash@192.168.1.20:7700, garbage, laptop@[fd00::1]:7700, noaddr"));
        let peers = live::parse_static_peers(spec.as_deref());
        if peers.len() == 2 && peers[0].name == "trash" && peers[1].name == "laptop" && peers.iter().all(|p| !p.online) {
            passed += 1;
        } else {
            libakuma::print(&alloc::format!("  [!] static peers: {:?}\n", peers.iter().map(|p| (p.name.clone(), p.addr.clone())).collect::<alloc::vec::Vec<_>>()));
        }
    }

    // Litter* tools refuse to run before an agent name is configured. This
    // must be the LAST test in the module: it does not reset AGENT_NAME_INIT
    // afterward (there is no reset primitive, by design — see set_agent_name),
    // so any test added below this point would observe a configured agent.
    total += 1;
    {
        if !AGENT_NAME_INIT.load(Ordering::Acquire) {
            let r = tool_read_inbox();
            if !r.success && r.output.contains("not configured") {
                passed += 1;
            } else {
                libakuma::print(&alloc::format!("  [!] unconfigured ReadInbox should fail clearly: success={} {:?}\n", r.success, r.output));
            }
        } else {
            libakuma::print("  [!] skipped unconfigured-agent test: an earlier test already set an agent name\n");
        }
    }

    libakuma::print(&alloc::format!("  result: {}/{}\n", passed, total));
    let mailbox_failures = if passed == total { 0 } else { 1 };
    mailbox_failures + hub::run_tests() + observe::run_tests() + serve::run_tests() + tasks::run_tests() + live::sim::run_tests()
}
