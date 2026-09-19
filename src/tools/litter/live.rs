//! `meow litter live`: a *resident* litter agent — the thing that turns the
//! litter from "a batch script that runs and dies" into "agents that exist".
//! The process never exits on its own: it sleeps in a low-frequency poll
//! loop (one second per tick, no busy-waiting), wakes to run a normal
//! chat-with-tools turn whenever something lands in its inbox, and doubles
//! as the hub when it wins the bind race:
//!
//! - At startup it tries to bind the hub socket (`litter_hub_addr`). Winner
//!   becomes the coordinator: it carries the whole litter's state in-process
//!   (`serve::HubState`, the no_std twin of `litter-hub`) and drains waiting
//!   hub clients between its own duties. Losers (and every one-shot `meow`
//!   invocation) are plain hub clients — the same code path as before.
//!   There is no dedicated hub *process* to babysit: assume good will among
//!   agents, and if the coordinator dies, the next agent to start simply
//!   wins the race instead; the roster re-seeds itself through the same
//!   bootstrap `Join` every agent already sends at startup.
//! - Everyone polls their own inbox once per tick through the normal client
//!   path (the leader polls itself over loopback through its own backlog —
//!   one code path for both roles). New messages wake the agent: one
//!   `chat_once` turn, persona + tools, exactly like `meow litter chase`,
//!   where the LLM reads its inbox and answers via `SendMessage`.
//! - The coordinator additionally checks its *task memory* every
//!   `TASK_TICKS` ticks: files dropped into `/litter/tasks` by the operator
//!   (`yard.sh task "..."`). Each still-pending file is broadcast to the
//!   `litter` group (which fans out to every member's inbox — `serve`) and
//!   the file is renamed `<name>.dispatched` so it is never sent twice.
//!   Unfinished tasks simply survive until some leader gets to them.
//!
//! Single-threaded reality, spelled out: while an agent is mid-turn (an LLM
//! call can take minutes) the leader is not draining its listener, so hub
//! clients queue in the kernel's listen backlog and are served on the next
//! drain — nothing is lost, just delayed. One turn at a time per agent, and
//! agents wake on the same group message at slightly staggered offsets
//! (their name hashed into the tick count) so a four-model debate doesn't
//! stampede the Ollama host in lockstep.

use alloc::format;
use alloc::string::String;

use libakuma::net::TcpListener;

use litter_wire::Request;

use crate::app::{chat_once, Conversation, Message};
use crate::config::Provider;
use crate::app::session;

use super::hub;
use super::serve;

/// Poll cadence in seconds. One tick = one `sleep(1)`; the loop does at most
/// two cheap operations per tick (drain the hub backlog, count the inbox),
/// so an idle litter costs effectively zero CPU.
const TICK_SECS: u64 = 1;

/// Task-memory scan every N ticks (30s at the default cadence). Only the
/// current coordinator does this, and only between turns.
const TASK_TICKS: u64 = 30;

/// Where the operator drops task files; deliberately NOT scoped by
/// `MEOW_HOME` — it is shared litter memory, not per-agent state, and only
/// the current coordinator ever reads it.
const TASKS_DIR: &str = "/litter/tasks";

/// Run forever. `model`/`provider`/`system_prompt` come from the caller in
/// `main.rs`, already resolved from config exactly the way `litter chase`
/// resolves them — a live turn IS a chase turn, just triggered by inbox
/// activity instead of a command line.
pub fn run(model: String, provider: Provider, system_prompt: String) -> ! {
    let me = match super::agent_name() {
        Some(n) => n,
        None => {
            libakuma::print("meow litter live: litter_agent_name must be set in the config\n");
            libakuma::exit(1);
        }
    };
    let addr = hub::hub_addr().unwrap_or_else(|| String::from("127.0.0.1:7700"));

    // The race: whoever binds the socket IS the hub. (At this point the
    // normal startup bootstrap in `main.rs` has already run — if a hub were
    // up, we joined it there and this bind loses; if not, we win and serve.)
    let mut state = serve::HubState::new();
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => {
            state.handle(Request::Join { name: me.clone() });
            libakuma::print(&format!("[live] {} is the hub at {} (won the bind race)\n", me, addr));
            Some(l)
        }
        Err(_) => {
            libakuma::print(&format!("[live] {} joined the litter at {} (hub already up)\n", me, addr));
            None
        }
    };

    // Baseline: whatever is already in the inbox predates us — never wake on
    // history, only on messages that arrive from now on.
    let mut seen = if listener.is_some() {
        state_inbox_len(&mut state, &me)
    } else {
        inbox_len(&addr, &me)
    };
    libakuma::print(&format!("[live] {} awake; {} message(s) already in history\n", me, seen));

    let mut tick: u64 = 0;
    loop {
        if let Some(l) = &listener {
            serve::drain(l, &mut state);
        }

        // Task memory: coordinator-only, spread across ticks.
        if listener.is_some() && tick % TASK_TICKS == 0 {
            dispatch_pending_tasks(&mut state);
        }

        // IMPORTANT: the leader must count its inbox from its own in-memory
        // state, never via a loopback client call. A loopback call is served
        // by *this same loop's* drain — it would sit in our own backlog
        // waiting for a drain that can't happen until the call returns. That
        // deadlock is why `state_inbox_len` exists.
        let total = if listener.is_some() {
            state_inbox_len(&mut state, &me)
        } else {
            inbox_len(&addr, &me)
        };
        if total > seen {
            run_turn(&me, &model, &provider, &system_prompt);
            // The turn is done; anything that landed in the inbox while we
            // were thinking (replies addressed to us included) counts as
            // seen from here — it shaped the transcript the turn already
            // read, and re-waking on it would just re-hash the same round.
            seen = if listener.is_some() {
                state_inbox_len(&mut state, &me)
            } else {
                inbox_len(&addr, &me)
            };
        }

        tick += 1;
        libakuma::sleep(TICK_SECS);
    }
}

/// The leader's inbox count, read directly from the state it serves — never
/// over loopback (see the deadlock note in `run`).
fn state_inbox_len(state: &mut serve::HubState, me: &str) -> usize {
    match state.handle(Request::Inbox { name: String::from(me) }) {
        litter_wire::Response::Inbox { messages } => messages.len(),
        _ => 0,
    }
}

/// Count messages waiting for `me` through the ordinary client path — the
/// leader polls itself over loopback through its own listener backlog, so
/// both roles share this one code path. An unreachable hub returns 0 and the
/// caller's watermark logic simply retries next tick.
fn inbox_len(addr: &str, me: &str) -> usize {
    match hub::inbox_messages(addr, me) {
        Ok(messages) => messages.len(),
        Err(_) => 0,
    }
}

/// One chat-with-tools turn — the same shape as `litter chase` (fresh
/// conversation, persona system prompt, full tool loop), but the user
/// message is the wake-up instruction instead of a task.
fn run_turn(me: &str, model: &str, provider: &Provider, system_prompt: &str) {
    let session_id = session::generate_session_id();
    let mut conversation = Conversation::new_session(session_id);
    conversation.append(&Message::new("system", system_prompt));
    conversation.append(&Message::new("user", "[System Context] Current working directory: /\nNo sandbox restrictions."));
    conversation.append(&Message::new("assistant", "Understood."));

    libakuma::print(&format!("\n[live] {} wakes on new inbox activity\n", me));
    let wake = format!(
        "You are '{}' in a litter of agents. Your inbox has message(s) you haven't seen. \
         Use ReadInbox to catch up (ListPeers shows who's here), then do whatever the \
         newest messages ask of you and reply with SendMessage — to a specific peer, \
         or to 'litter' to reach everyone. If there is genuinely nothing worth \
         responding to, just finish without sending anything.",
        me
    );
    if let Err(e) = chat_once(model, provider, &wake, &mut conversation, None, system_prompt) {
        libakuma::print(&format!("[live] {}'s turn failed: {}\n", me, e));
    }
}

/// The coordinator's task memory: every file in `/litter/tasks` that doesn't
/// end in `.dispatched` gets broadcast to the litter group, then renamed so
/// it is dispatched exactly once no matter how many coordinators come and
/// go. Best-effort on purpose — an unreadable file is skipped, not fatal.
fn dispatch_pending_tasks(state: &mut serve::HubState) {
    let entries = match libakuma::read_dir(TASKS_DIR) {
        Some(e) => e,
        None => return, // no task dir yet = no tasks; not an error
    };
    for entry in entries {
        if entry.is_dir || entry.name.ends_with(".dispatched") {
            continue;
        }
        let path = format!("{}/{}", TASKS_DIR, entry.name);
        let body = match read_small_file(&path) {
            Some(b) => b,
            None => continue,
        };
        if body.trim().is_empty() {
            continue;
        }
        let sent = state.handle(Request::Send {
            from: String::from("root"),
            to: String::from(serve::GROUP_NAME),
            body: format!("[task: {}] {}", entry.name, body.trim()),
            round: 0,
        });
        if matches!(sent, litter_wire::Response::Sent { .. }) {
            let done = format!("{}.dispatched", path);
            if libakuma::rename(&path, &done) < 0 {
                libakuma::print(&format!("[live] warning: dispatched '{}' but couldn't mark it\n", path));
            }
        }
    }
}

fn read_small_file(path: &str) -> Option<String> {
    let fd = libakuma::open(path, libakuma::open_flags::O_RDONLY);
    if fd < 0 {
        return None;
    }
    let stat = match libakuma::fstat(fd) {
        Ok(s) => s,
        Err(_) => {
            libakuma::close(fd);
            return None;
        }
    };
    let size = stat.st_size as usize;
    if size == 0 || size > 32 * 1024 {
        libakuma::close(fd);
        return None;
    }
    let mut buf = alloc::vec![0u8; size];
    let n = libakuma::read_fd(fd, &mut buf);
    libakuma::close(fd);
    if n <= 0 {
        return None;
    }
    String::from_utf8(buf[..n as usize].to_vec()).ok()
}
