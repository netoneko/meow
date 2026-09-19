//! `meow litter live`: a resident litter agent. One process, two pthread
//! threads (see `docs/LITTER_STATE_MACHINE.md` + `docs/LITTER_RAFT_LOOP.md`):
//!
//! - **raft/serve thread** (spawned here, only when this process won the
//!   bind race): owns its tick — drain the listener's backlog, task-table
//!   duties, static-peer discovery, compaction — and is never blocked by
//!   inference, which is the concrete "consensus networking strictly
//!   decoupled from model worker tasks".
//! - **agent loop** (the main thread): the LEADER/FOLLOWER/WAYWARD state
//!   machine. It ALSO drains (both threads serve the shared listener; more
//!   drain points never hurt), pulses `Peers` (the round-trip is the
//!   heartbeat observation and the event feed), polls its inbox, and wakes
//!   into a `chat_once` turn on new activity.
//!
//! Both threads share `PMutex<HubState>` + the listener behind `Arc`s;
//! critical sections are Vec operations. State is in-memory only — the
//! filesystem is not a transport, not a store (network-bound by design).
//!
//! Bootstrap is self-serve: join, page history back to the last compaction
//! marker, done. Nobody activates a joining agent from the outside.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use libakuma::net::TcpListener;

use litter_wire::{Message, MessageKind, Request, Response};

use crate::app::session;
use crate::app::{chat_once, Conversation, Message as ChatMessage};
use crate::config::Provider;
use crate::rt::{spawn_detached, PMutex};

use super::hub;
use super::serve::{self, HubState};

/// Poll cadence: one tick = one second of sleep.
const TICK_SECS: u64 = 1;
/// Pulse (Peers probe) and task-table cadence, in ticks.
const PULSE_TICKS: u64 = 5;
/// History compaction cadence, in ticks (~30s).
const COMPACT_TICKS: u64 = 30;
/// How many newest messages per inbox the compaction marker spares.
const KEEP_RECENT: usize = serve::KEEP_RECENT;
/// History page size when a cold-starting agent walks back to the marker.
const HISTORY_PAGE: u32 = 32;

/// One `name@host:port` entry from `litter_static_peers` in the config —
/// a peer we want to know about even before (or without) it ever joining
/// on its own: the trashcan/laptop split, where the other host's agent may
/// be down for a long time and should show up as "discovered"/"lost"
/// events rather than not exist.
pub struct StaticPeer {
    pub name: String,
    pub addr: String,
    /// Last probe reached it — drives discovered/lost event transitions.
    pub online: bool,
}

pub fn parse_static_peers(spec: Option<&str>) -> Vec<StaticPeer> {
    let mut out = Vec::new();
    if let Some(spec) = spec {
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if let Some((name, addr)) = entry.split_once('@') {
                if litter_wire::is_valid_name(name.trim()) && addr.contains(':') {
                    out.push(StaticPeer { name: String::from(name.trim()), addr: String::from(addr.trim()), online: false });
                }
            }
        }
    }
    out
}

/// The agent loop's state machine. `docs/LITTER_STATE_MACHINE.md` is the
/// prose; this enum is the code. LEADER is really "this process holds the
/// socket (and the raft thread)"; FOLLOWER and WAYWARD differ only in
/// whether the hub is currently answering.
enum Machine {
    Leader {
        listener: Arc<TcpListener>,
        state: Arc<PMutex<HubState>>,
    },
    /// Hub is up (someone else's process) and answering.
    Follower {
        epoch: u64,
        roster: Vec<String>,
        term: u64,
    },
    /// Hub went silent past the heartbeat timeout: no hub calls, tools
    /// fail fast (hub::unresponsive), re-race the bind every tick.
    Wayward {
        epoch: u64,
        roster: Vec<String>,
        term: u64,
    },
}

pub fn run(model: String, provider: Provider, system_prompt: String) -> ! {
    let me = match super::agent_name() {
        Some(n) => n,
        None => {
            libakuma::print("meow litter live: litter_agent_name must be set in the config\n");
            libakuma::exit(1);
        }
    };
    let addr = hub::hub_addr().unwrap_or_else(|| String::from("127.0.0.1:7700"));

    // ---- Bootstrap (self-serve): join, then walk history back to the
    // marker. Both are plain client calls; if the hub isn't up, they fail
    // and the bind race below decides who becomes it.
    hub::bootstrap(&addr, &me);
    load_history(&addr, &me, &system_prompt);

    // ---- The bind race: connect failed at bootstrap ⇒ either nobody holds
    // the socket (we try to win it) or someone won it microseconds ago
    // (our bind fails and we're a plain follower — correct either way).
    let mut machine = match TcpListener::bind(&addr) {
        Ok(listener) => {
            // try_accept is only non-blocking when the socket is: without
            // this, the first drain parks in accept() holding the state
            // lock and the hub freezes for the whole litter.
            let _ = listener.set_nonblocking(true);
            let listener = Arc::new(listener);
            let state = Arc::new(PMutex::new(HubState::new()));
            {
                let mut st = state.lock();
                st.handle(Request::Join { name: me.clone() });
                // Fresh litter = term 1. (A takeover re-race bumps to
                // last_seen_term + 1 — see WAYWARD handling below.)
                st.set_leader(&me, 1);
            }
            // Static peers from the config: discovered (or declared lost)
            // by the raft thread; this is how the trashcan/laptop split
            // shows up as status changes instead of absence.
            let static_peers = parse_static_peers(super::static_peers_spec().as_deref());
            start_raft_thread(&listener, &state, static_peers);
            libakuma::print(&format!("[live] {} holds the hub at {} (won the bind race) — raft thread up\n", me, addr));
            Machine::Leader { listener, state }
        }
        Err(_) => {
            libakuma::print(&format!("[live] {} joined the litter at {} (hub already up)\n", me, addr));
            Machine::Follower { epoch: 0, roster: Vec::new(), term: 0 }
        }
    };

    // Baseline: history predates us — wake only on what arrives from now.
    let mut seen = inbox_count(&addr, &me);
    libakuma::print(&format!("[live] {} awake; {} message(s) already in history\n", me, seen));

    let mut tick: u64 = 0;
    let mut latest_term: u64 = 0; // highest term any pulse reported
    loop {
        tick += 1;

        match &mut machine {
            Machine::Leader { listener, state } => {
                // The agent loop drains too: two serve points on one
                // listener, so requests never wait a full raft tick.
                serve::drain(&listener, &mut state.lock());
                let count = inbox_count(&addr, &me);
                if count > seen {
                    run_turn(&me, &model, &provider, &system_prompt);
                    seen = inbox_count(&addr, &me);
                }
            }
            Machine::Follower { epoch, roster, term } => {
                if tick % PULSE_TICKS == 0 {
                    match hub::peers(&addr, *epoch) {
                        Ok(Response::Peers { names, term: t, epoch: e, events, .. }) => {
                            hub::mark_alive();
                            latest_term = latest_term.max(t);
                            *term = t;
                            *roster = names;
                            for event in events {
                                libakuma::print(&format!("[live] {} hears: {}\n", me, event));
                            }
                            *epoch = e;
                        }
                        Ok(_) => {}
                        Err(_) => { /* probe failed: liveness bookkeeping simply doesn't advance */ }
                    }
                    if hub::unresponsive() {
                        libakuma::print(&format!("[live] {} is WAYWARD: hub silent for {}s\n", me, hub::silent_for_secs()));
                        machine = Machine::Wayward { epoch: *epoch, roster: core::mem::take(roster), term: *term };
                        continue;
                    }
                }
                let count = inbox_count(&addr, &me);
                if count > seen {
                    run_turn(&me, &model, &provider, &system_prompt);
                    seen = inbox_count(&addr, &me);
                }
            }
            Machine::Wayward { epoch, roster, term } => {
                // No hub calls except the one that matters: can we take the
                // socket? The dead leader's port is free; a merely-hung
                // leader still holds it and this bind keeps failing.
                match TcpListener::bind(&addr) {
                    Ok(listener) => {
                        let _ = listener.set_nonblocking(true);
                        let listener = Arc::new(listener);
                        let state = Arc::new(PMutex::new(HubState::new()));
                        {
                            let mut st = state.lock();
                            st.handle(Request::Join { name: me.clone() });
                            let new_term = latest_term.max(*term) + 1;
                            st.set_leader(&me, new_term);
                        }
                        let static_peers = parse_static_peers(super::static_peers_spec().as_deref());
                        start_raft_thread(&listener, &state, static_peers);
                        libakuma::print(&format!("[live] {} re-raced the bind and WON — leader again (term {})\n", me, latest_term.max(*term) + 1));
                        machine = Machine::Leader { listener, state };
                        // State we served before is gone (in-memory by
                        // design); survivors re-register through their own
                        // pulses, and history below the last marker is
                        // compacted knowledge anyway.
                    }
                    Err(_) => {
                        // Socket still held: the old leader hangs on. Keep
                        // the cached roster, fail fast on tools, keep
                        // probing the bind. Also re-try a plain Peers: if
                        // the hub starts answering again we go straight
                        // back to FOLLOWER.
                        if tick % PULSE_TICKS == 0 {
                            if let Ok(Response::Peers { names, term: t, epoch: e, .. }) = hub::peers(&addr, *epoch) {
                                hub::mark_alive();
                                libakuma::print(&format!("[live] {} is FOLLOWER again (hub answered after {}s)\n", me, hub::silent_for_secs()));
                                machine = Machine::Follower { epoch: e, roster: core::mem::take(roster), term: t.max(*term) };
                                let _ = names;
                                continue;
                            }
                            *roster = core::mem::take(roster); // keep cache; nothing new
                            let _ = (*epoch, *term);
                        }
                    }
                }
            }
        }

        libakuma::sleep(TICK_SECS);
    }
}

/// The raft thread: pure servo, never blocked by inference. Drains the
/// backlog, runs the task table, probes static peers, compacts history —
/// on its own 1s cadence.
/// The raft thread's context, leaked once at spawn: a `fn()`-pointer
/// spawn can't capture, and the thread lives exactly as long as the
/// process, so a one-time leak IS the sane ownership story here.
struct RaftCtx {
    listener: Arc<TcpListener>,
    state: Arc<PMutex<HubState>>,
    /// Behind the same coarse PMutex type as the state: the probe mutates
    /// `online` transitions, and a fn()-pointer spawn can't hand a `&mut`
    /// across the clone.
    peers: PMutex<Vec<StaticPeer>>,
}

static RAFT_CTX: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn raft_entry() {
    let ptr = RAFT_CTX.load(core::sync::atomic::Ordering::Acquire) as *const RaftCtx;
    if ptr.is_null() {
        return;
    }
    // SAFETY: the ctx was leaked by start_raft_thread before the child was
    // spawned; the child is its only reader and the process outlives it.
    let ctx = unsafe { &*ptr };
    raft_tick_loop(ctx.listener.clone(), ctx.state.clone(), &ctx.peers);
}

/// Leak the context and spawn the raft thread. Returns false when the
/// spawn failed (the caller keeps running as leader with its own drain —
/// degraded, not dead).
fn start_raft_thread(listener: &Arc<TcpListener>, state: &Arc<PMutex<HubState>>, static_peers: Vec<StaticPeer>) -> bool {
    let ctx: &'static RaftCtx = Box::leak(Box::new(RaftCtx {
        listener: listener.clone(),
        state: state.clone(),
        peers: PMutex::new(static_peers),
    }));
    RAFT_CTX.store(ctx as *const RaftCtx as u64, core::sync::atomic::Ordering::Release);
    unsafe { crate::rt::spawn_detached(raft_entry) }
}

fn raft_tick_loop(listener: Arc<TcpListener>, state: Arc<PMutex<HubState>>, static_peers: &PMutex<Vec<StaticPeer>>) {
    let mut tick: u64 = 0;
    loop {
        tick += 1;
        {
            let mut st = state.lock();
            serve::drain(&listener, &mut st);
            if tick % PULSE_TICKS == 0 {
                st.task_tick();
                let mut peers = static_peers.lock();
                probe_static_peers(&mut st, &mut peers);
            }
            if tick % COMPACT_TICKS == 0 {
                st.compact();
            }
        }
        libakuma::sleep(TICK_SECS);
    }
}

/// Ask each configured static peer "who's there?" — the trashcan/laptop
/// discovery loop. First answer registers the peer in the roster (with an
/// event, so every agent hears about it); silence after being online
/// raises a "lost" event. The peer's own litter is unaffected: we only
/// read its public pulse.
fn probe_static_peers(st: &mut HubState, peers: &mut [StaticPeer]) {
    // To actually reach a static peer we need a client call; `hub::peers`
    // targets `hub_addr()`, so for remote peers issue the request directly
    // through a one-shot connect on the peer's address.
    for peer in peers {
        let view = super::hub::call_addr(&peer.addr, &Request::Peers { since: 0 });
        match view {
            Ok(Response::Peers { .. }) => {
                if !peer.online {
                    peer.online = true;
                    st.event(format!("[event] static peer {} discovered at {}", peer.name, peer.addr));
                }
            }
            _ => {
                if peer.online {
                    peer.online = false;
                    st.event(format!("[event] static peer {} lost ({})", peer.name, peer.addr));
                }
            }
        }
    }
}

/// Cold-start history walk: page back in batches until a `Marker` kind
/// message shows up (or history ends). The marker plus the live tail is
/// everything a fresh agent needs; nothing older is ever pulled.
fn load_history(addr: &str, me: &str, system_prompt: &str) {
    // The prompt is built by the caller before we run; the walk's purpose
    // is to touch the protocol once so a joining agent starts warm — and
    // to surface the marker summary in the log for the operator.
    let mut before: u64 = 0;
    let mut batches = 0;
    loop {
        match hub::history(addr, me, before, HISTORY_PAGE) {
            Ok(messages) if messages.is_empty() => break,
            Ok(messages) => {
                batches += 1;
                let oldest = messages.first().map(|m: &Message| m.ts).unwrap_or(0);
                let hit_marker = messages.iter().any(|m| m.kind == MessageKind::Marker);
                if hit_marker {
                    libakuma::print(&format!(
                        "[live] {} sourced history: {} batch(es), reached the compaction marker\n",
                        me, batches
                    ));
                    return;
                }
                before = oldest;
            }
            Err(_) => return, // hub not up yet; the bind race decides next
        }
    }
    let _ = system_prompt;
}

fn inbox_count(addr: &str, me: &str) -> usize {
    match hub::inbox_messages(addr, me) {
        Ok(messages) => messages.iter().filter(|m| wakeable(m)).count(),
        Err(_) => 0, // unreachable hub (or WAYWARD) — count simply stalls
    }
}

/// What wakes an agent: real conversation and task assignments. Compaction
/// markers are bookkeeping (they carry the folded summary the agent will
/// read on its next real wake); anything else protocol-shaped isn't chat.
fn wakeable(m: &Message) -> bool {
    !matches!(m.kind, MessageKind::Marker | MessageKind::Done)
}

/// One chat-with-tools turn — the same shape as `litter chase` (fresh
/// conversation, persona system prompt, full tool loop), but the user
/// message is the wake-up instruction plus fresh cluster context.
fn run_turn(me: &str, model: &str, provider: &Provider, system_prompt: &str) {
    let session_id = session::generate_session_id();
    let mut conversation = Conversation::new_session(session_id);
    conversation.append(&ChatMessage::new("system", system_prompt));
    conversation.append(&ChatMessage::new("user", "[System Context] Current working directory: /\nNo sandbox restrictions."));
    conversation.append(&ChatMessage::new("assistant", "Understood."));

    libakuma::print(&format!("\n[live] {} wakes on new inbox activity\n", me));
    let wake = format!(
        "You are '{}' in a litter of agents. Your inbox has message(s) you haven't seen. \
         Use ListPeers to see who's here (the response also carries recent cluster events), \
         ReadInbox to catch up, then do whatever the newest messages ask of you and reply \
         with SendMessage — to a specific peer, or to 'litter' to reach everyone. \
         Start a message body with `[task] …` to open a tracked task, and answer an \
         assignment with `[done: tN] …` when finished. If there is genuinely nothing \
         worth responding to, just finish without sending anything.",
        me
    );
    if let Err(e) = chat_once(model, provider, &wake, &mut conversation, None, system_prompt) {
        libakuma::print(&format!("[live] {}'s turn failed: {}\n", me, e));
    }
}
