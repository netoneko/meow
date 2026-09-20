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
/// How old an outbound chat may be and still cross a litter boundary —
/// past this it's history, not news; the peer gets it via history, not replay.
const RELAY_MAX_AGE_US: u64 = 60 * 1_000_000;

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
    /// Relay cursor: every outbound chat with `ts` above this has been
    /// delivered to this peer's hub. Cursor only advances on confirmed
    /// `Sent`, so an unreachable peer re-drives its batch next tick.
    pub last_relay_ts: u64,
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
                    out.push(StaticPeer { name: String::from(name.trim()), addr: String::from(addr.trim()), online: false, last_relay_ts: 0 });
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
        let relay_jobs = {
            let mut st = state.lock();
            serve::drain(&listener, &mut st);
            if tick % PULSE_TICKS == 0 {
                st.task_tick();
                let mut peers = static_peers.lock();
                probe_static_peers(&mut st, &mut peers);
                relay_jobs(&mut st, &mut peers)
            } else {
                Vec::new()
            }
        };
        if tick % COMPACT_TICKS == 0 {
            state.lock().compact();
        }
        // Relay I/O runs OUTSIDE the state lock: a slow peer costs its own
        // send deadlines, never the hub (same rule as deadline-bounded
        // serve_one — see docs/LITTER_RELAY_TOPOLOGY.md for where this goes
        // long-term).
        if !relay_jobs.is_empty() {
            relay_send(&relay_jobs, static_peers);
        }
        libakuma::sleep(TICK_SECS);
    }
}

/// Snapshot the outbound relay batch for every online peer, oldest first.
/// Pure state read — the I/O happens in `relay_send`, outside the locks.
/// Loop guard: a message whose sender already carries any known litter
/// prefix (a peer's, or our own) was relayed IN — it never relays onward.
/// Age guard: chat older than RELAY_MAX_AGE_US is never relayed — a peer
/// that comes back after a long partition resyncs through history, it does
/// not get a replay of the debate it missed (see docs/LITTER_RELAY_TOPOLOGY.md).
fn relay_jobs(st: &mut HubState, peers: &mut [StaticPeer]) -> Vec<(String, String, String, String, i64, u64)> {
    let our = match super::litter_name() {
        Some(n) => n,
        None => return Vec::new(), // relay off: no litter identity configured
    };
    let now = crate::util::now_us();
    let fresh_enough = |ts: u64| now.saturating_sub(ts) <= RELAY_MAX_AGE_US;
    let mut jobs = Vec::new();
    for peer in peers.iter().filter(|p| p.online) {
        let peer_prefix = format!("{}-", peer.name);
        let our_prefix = format!("{}-", our);
        for entry in &st.relay_log {
            if entry.msg.ts <= peer.last_relay_ts || entry.msg.kind != MessageKind::Chat || !fresh_enough(entry.msg.ts) {
                continue;
            }
            if entry.msg.from.starts_with(&peer_prefix) || entry.msg.from.starts_with(&our_prefix) {
                continue;
            }
            // Routing: group broadcast stays group; direct traffic to
            // `<peer>-<agent>` is rewritten to the bare agent name on the
            // far side; everything else is litter-local and not relayed.
            let to = if entry.to == serve::GROUP_NAME {
                String::from(serve::GROUP_NAME)
            } else {
                match entry.to.strip_prefix(&peer_prefix) {
                    Some(bare) if litter_wire::is_valid_name(bare) => String::from(bare),
                    _ => continue,
                }
            };
            jobs.push((peer.addr.clone(), format!("{}-{}", our, entry.msg.from), to, entry.msg.body.clone(), entry.msg.round, entry.msg.ts));
        }
    }
    jobs
}

/// Drive one relay batch: cursor only advances on a confirmed `Sent`, so an
/// unreachable peer simply re-drives its batch on a later tick (bounded by
/// the relay log's cap, never queued unboundedly).
fn relay_send(jobs: &[(String, String, String, String, i64, u64)], static_peers: &PMutex<Vec<StaticPeer>>) {
    for (addr, from, to, body, round, ts) in jobs {
        let req = Request::Send { from: from.clone(), to: to.clone(), body: body.clone(), round: *round };
        if matches!(hub::call_addr(addr, &req), Ok(Response::Sent { .. })) {
            let mut peers = static_peers.lock();
            if let Some(p) = peers.iter_mut().find(|p| &p.addr == addr) {
                p.last_relay_ts = p.last_relay_ts.max(*ts);
            }
        }
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

// ============================================================================
// Swarm simulation (`meow test`) — docs/LITTER_RELAY_TOPOLOGY.md's "part of
// the raft traffic simulation" requirement, stopgap-relay edition. Two and
// three litter topologies over a MOCKED transport: the real `HubState` state
// machine and the real `relay_jobs` snapshot run, but `relay_send`'s socket
// hop is replaced by applying the job's `Request::Send` to the peer hub
// directly (the litter-raft sim's trick). What this buys: storm-bound,
// dedup, age-discard and cursor guarantees are asserted against the real
// code paths, not a reimplementation of them.
// ============================================================================
#[cfg(feature = "tests")]
pub mod sim {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use alloc::vec;

    use litter_wire::{Request, Response, SenderRole};

    use super::{parse_static_peers, relay_jobs, StaticPeer};
    use crate::tools::litter::serve::{HubState, RelayEntry, GROUP_NAME};
    use crate::util::now_us;

    struct Litter {
        name: &'static str,
        /// This litter's own hub address (what peers aim at).
        addr: &'static str,
        hub: HubState,
        peers: Vec<StaticPeer>,
    }

    impl Litter {
        /// A litter with `agents` joined and one leader, peering at `peers`
        /// (all links simulated up — tests toggle them explicitly).
        fn seed(name: &'static str, addr: &'static str, agents: &[&str], peers_spec: &str) -> Self {
            let mut hub = HubState::new();
            hub.set_leader(agents[0], 1);
            for a in agents {
                hub.handle(Request::Join { name: String::from(*a) });
            }
            let mut peers = parse_static_peers(Some(peers_spec));
            for p in peers.iter_mut() {
                p.online = true;
            }
            Litter { name, addr, hub, peers }
        }

        fn inbox_has(&mut self, agent: &str, body: &str) -> bool {
            matches!(
                self.hub.handle(Request::Inbox { name: String::from(agent) }),
                Response::Inbox { messages } if messages.iter().any(|m| m.body == body)
            )
        }
    }

    /// The mocked transport: one relay tick, exactly what `relay_send` does
    /// on `Ok(Sent)` — apply the job to the destination hub, advance the
    /// cursor. Returns how many frames crossed a link.
    fn relay_tick(litters: &mut [( &'static str, Litter )], from: &str) -> usize {
        let i = litters.iter().position(|(n, _)| *n == from).expect("litter");
        crate::tools::litter::set_litter_name(Some(String::from(litters[i].0)));
        let jobs = {
            let (_, litter) = &mut litters[i];
            relay_jobs(&mut litter.hub, &mut litter.peers)
        };
        let mut crossed = 0usize;
        for (addr, from, to, body, round, ts) in jobs {
            // resolve destination: the litter whose HUB listens on this addr
            let dest = litters.iter().position(|(_, l)| l.addr == addr).expect("peer litter");
            let req = Request::Send { from, to, body, round };
            if matches!(litters[dest].1.hub.handle(req), Response::Sent { .. }) {
                let peer = litters[i].1.peers.iter_mut().find(|p| p.addr == addr).unwrap();
                peer.last_relay_ts = peer.last_relay_ts.max(ts);
                crossed += 1;
            }
        }
        crossed
    }

    pub fn run_tests() -> i32 {
        let mut passed = 0usize;
        let mut total = 0usize;
        libakuma::print("--- litter swarm sim tests ---\n");

        // 1. relay joins two litters: yard's broadcast lands in every
        //    island inbox, prefixed with the origin litter's name
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al", "amber"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob", "bella"], "yard@10.0.0.1:7700")),
            ];
            sw[0].1.hub.handle(Request::Send { from: String::from("al"), to: String::from(GROUP_NAME), body: String::from("hello island"), round: 1 });
            let crossed = relay_tick(&mut sw, "yard");
            let ok = crossed == 1
                && sw[1].1.inbox_has("bob", "hello island")
                && sw[1].1.inbox_has("bella", "hello island");
            if ok { passed += 1; } else {
            let dump = |l: &mut Litter, a: &str| match l.hub.handle(Request::Inbox { name: String::from(a) }) {
                Response::Inbox { messages } => messages.iter().map(|m| (m.from.clone(), m.body.clone())).collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            libakuma::print(&format!("  [!] join: crossed={} bob={:?} bella={:?}\n", crossed, dump(&mut sw[1].1, "bob"), dump(&mut sw[1].1, "bella")));
        }
        }

        // 2. storm bound: the relayed-in copy never relays onward — one
        //    message crosses one link exactly once, no matter how many ticks
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            sw[0].1.hub.handle(Request::Send { from: String::from("al"), to: String::from(GROUP_NAME), body: String::from("once only"), round: 1 });
            let first = relay_tick(&mut sw, "yard");
            let again = relay_tick(&mut sw, "yard") + relay_tick(&mut sw, "island") + relay_tick(&mut sw, "island");
            if first == 1 && again == 0 { passed += 1; }
            else { libakuma::print(&format!("  [!] storm bound: first={} again={}\n", first, again)); }
        }

        // 3. direct cross-litter reply: island's bob → yard's al arrives
        //    only in al's inbox, still prefixed with the origin litter
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al", "amber"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            sw[1].1.hub.handle(Request::Send { from: String::from("bob"), to: String::from("yard-al"), body: String::from("reply to yard"), round: 2 });
            let crossed = relay_tick(&mut sw, "island");
            let ok = crossed == 1
                && sw[0].1.inbox_has("al", "reply to yard")
                && !sw[0].1.inbox_has("amber", "reply to yard");
            if ok { passed += 1; } else { libakuma::print(&format!("  [!] direct: crossed={}\n", crossed)); }
        }

        // 4. age discard: an old unseen message is never relayed — a peer
        //    back after a long partition resyncs via history, not replay
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            let old = now_us().saturating_sub(super::RELAY_MAX_AGE_US + 1_000_000);
            sw[0].1.hub.relay_log.push(RelayEntry {
                to: String::from(GROUP_NAME),
                msg: litter_wire::Message {
                    from: String::from("al"),
                    round: 1,
                    body: String::from("stale news"),
                    ts: old,
                    kind: litter_wire::MessageKind::Chat,
                    role: SenderRole::Peer,
                },
            });
            let crossed = relay_tick(&mut sw, "yard");
            if crossed == 0 && !sw[1].1.inbox_has("bob", "stale news") { passed += 1; }
            else { libakuma::print(&format!("  [!] age discard: crossed={}\n", crossed)); }
        }

        // 5. flake + cursor: an offline peer drops nothing permanently but
        //    delivers nothing twice — offline ticks skip, one online tick
        //    delivers, the cursor keeps every later tick silent
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            sw[0].1.hub.handle(Request::Send { from: String::from("al"), to: String::from(GROUP_NAME), body: String::from("across the flake"), round: 1 });
            sw[0].1.peers[0].online = false;
            let while_down = relay_tick(&mut sw, "yard");
            sw[0].1.peers[0].online = true;
            let recovered = relay_tick(&mut sw, "yard");
            let after = relay_tick(&mut sw, "yard");
            let ok = while_down == 0 && recovered == 1 && after == 0
                && sw[1].1.inbox_has("bob", "across the flake");
            if ok { passed += 1; }
            else { libakuma::print(&format!("  [!] flake: down={} recovered={} after={}\n", while_down, recovered, after)); }
        }

        // 6. three-litter mesh: A's message reaches B and C exactly once
        //    each (B does NOT transit A's traffic to C — relay is one hop,
        //    the anti-storm property the target topology must preserve)
        total += 1;
        {
            let mut sw = vec![
                ("a", Litter::seed("a", "10.0.0.1:7700", &["x"], "b@10.0.0.2:7700,c@10.0.0.3:7700")),
                ("b", Litter::seed("b", "10.0.0.2:7700", &["y"], "a@10.0.0.1:7700,c@10.0.0.3:7700")),
                ("c", Litter::seed("c", "10.0.0.3:7700", &["z"], "a@10.0.0.1:7700,b@10.0.0.2:7700")),
            ];
            sw[0].1.hub.handle(Request::Send { from: String::from("x"), to: String::from(GROUP_NAME), body: String::from("mesh news"), round: 1 });
            let mut crossings = 0;
            for _ in 0..3 {
                crossings += relay_tick(&mut sw, "a");
                crossings += relay_tick(&mut sw, "b");
                crossings += relay_tick(&mut sw, "c");
            }
            let ok = crossings == 2
                && sw[1].1.inbox_has("y", "mesh news")
                && sw[2].1.inbox_has("z", "mesh news");
            if ok { passed += 1; }
            else { libakuma::print(&format!("  [!] mesh: crossings={} (want 2)\n", crossings)); }
        }

        libakuma::print(&format!("  result: {}/{}\n", passed, total));
        if passed == total { 0 } else { 1 }
    }
}
