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

/// I/O budget for a **peer probe**, as opposed to a real request.
///
/// Half a second, against `deadline::IO_TIMEOUT_US`'s five. The probe runs in
/// the raft thread, which is the thread that answers the hub, so this number is
/// how long this litter goes deaf each pulse when a peer does not respond. It
/// has to stay far below the pulse interval or two litters pointed at each
/// other spend all their time waiting on one another instead of serving.
const PROBE_TIMEOUT_US: u64 = 500_000;

/// How many pulses to skip after a failed probe, doubling to this cap.
///
/// A peer that answers is probed every pulse; one that does not is probed on
/// pulses 1, 2, 4, 8, 16 … so a dead or starved peer costs a bounded fraction of
/// this hub's serving time instead of all of it.
const PROBE_BACKOFF_MAX: u32 = 5;

/// How often the raft thread says it is alive, in ticks.
const HEARTBEAT_TICKS: u64 = 10;
/// History compaction cadence, in ticks (~30s).
const COMPACT_TICKS: u64 = 30;
/// How many newest messages per inbox the compaction marker spares.
const KEEP_RECENT: usize = serve::KEEP_RECENT;
/// History page size when a cold-starting agent walks back to the marker.
const HISTORY_PAGE: u32 = 32;
/// How old an outbound chat may be and still cross a litter boundary —
/// past this it's history, not news; the peer gets it via history, not
/// replay. Also the seen-table's memory: dedup metadata older than the
/// cutoff is forgotten (a replay past it is discarded by this same guard).
pub const RELAY_MAX_AGE_US: u64 = 60 * 1_000_000;

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
    /// Consecutive failed probes, capped at [`PROBE_BACKOFF_MAX`]. Only a peer
    /// that answers resets it.
    ///
    /// This exists because probing costs **this hub's ability to serve**. The
    /// probe runs in the raft thread, and the connect inside it is not bounded
    /// by anything meow controls — an unanswered SYN sits there for Akuma's
    /// `CONNECT_TIMEOUT_US`, ten seconds, which is longer than the pulse
    /// interval. Two litters listing each other therefore reach a stable state
    /// where each one's serve loop is permanently inside a connect to the other,
    /// neither answers, and both probes keep failing: measured 2026-09-20
    /// between `trashcan` and `ryzen`, where the hub port answered nobody at all
    /// — not each other, not a third machine — while ssh on the same box
    /// answered in 60 ms. Backing off a silent peer is what lets a serve window
    /// exist for the other side's probe to land in.
    pub probe_failures: u32,
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
                    out.push(StaticPeer { name: String::from(name.trim()), addr: String::from(addr.trim()), online: false, last_relay_ts: 0, probe_failures: 0 });
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

// ============================================================================
// Raft log: an append-only event file at `<MEOW_HOME>/var/raft.log` — state
// transitions, relay traffic, task churn, all with second timestamps. The
// stdout log is for whoever watches the yard; this one is for post-hoc
// debugging of WHY the swarm did what it did. Best-effort: a failing append
// is silently ignored, the raft never blocks on its log.
//
// It lives at the root of this agent's session tree, deliberately NOT inside
// one session directory. Session leaves are `<secs>-<pid>`, so a
// session-scoped raft log would be (a) identified by a pid rather than by
// who the agent is, and (b) a NEW file on every restart — and herd runs
// these with `restart = true`. An agent that crash-loops is precisely the
// one whose raft log you want, and that is the one that would be scattered
// across a dozen directories with the history in none of them. One file per
// agent, appended across restarts, is what makes it readable. The session
// root is itself `MEOW_HOME`-scoped (see `session::sessions_root`), which is
// what keeps several agents in one box from sharing this file.
// ============================================================================

/// The log path as **bytes in a fixed buffer**, not a `String`.
///
/// `raft_log` is called from the raft thread, and the point of this file's
/// logging is to work when the process is in trouble — so the path it writes to
/// must not be cloned (an allocation) on every line. 256 bytes covers
/// `MEOW_HOME`-scoped session roots with room to spare; a longer one simply
/// does not log, which is the right failure for a diagnostic.
static mut RAFT_LOG_PATH: [u8; 256] = [0u8; 256];
static mut RAFT_LOG_PATH_LEN: usize = 0;
static RAFT_LOG_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The log's fd, opened **once** and held for the life of the process.
///
/// It used to be opened and closed per line, which was wrong twice over: it is
/// three syscalls where one write would do, and it made the log itself the
/// thing under suspicion — on the Firecracker guest exactly one append per tick
/// landed and the rest vanished, which is the shape of descriptors not coming
/// back (see `docs/archive/AMD64_SPAWNED_THREAD_NEVER_RUNS.md`). A diagnostic that
/// consumes a scarce resource per line cannot be trusted to describe a process
/// running low on it.
static RAFT_LOG_FD: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(-1);

/// Point the raft log at this agent's session root — `MEOW_HOME`-scoped, so
/// one file per agent, sitting beside that agent's session directories.
/// Returns the path so the caller can say where it went: an agent that
/// cannot tell you where its log is has a log nobody reads.
fn set_raft_log() -> String {
    let dir = session::sessions_root();
    libakuma::mkdir_p(&dir);
    let path = format!("{}/raft.log", dir);
    let bytes = path.as_bytes();
    if bytes.len() < 256 {
        // SAFETY: single-threaded here — `set_raft_log` runs before the raft
        // thread is spawned, and `RAFT_LOG_INIT` is the release that publishes
        // it. Nothing writes these after that store.
        unsafe {
            let dst = core::ptr::addr_of_mut!(RAFT_LOG_PATH).cast::<u8>();
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            *core::ptr::addr_of_mut!(RAFT_LOG_PATH_LEN) = bytes.len();
        }
        let fd = libakuma::open(
            &path,
            libakuma::open_flags::O_WRONLY | libakuma::open_flags::O_CREAT | libakuma::open_flags::O_APPEND,
        );
        if fd >= 0 {
            RAFT_LOG_FD.store(fd, core::sync::atomic::Ordering::Release);
            RAFT_LOG_INIT.store(true, core::sync::atomic::Ordering::Release);
        }
    }
    path
}

/// The held log fd, or `None` before it is opened.
fn raft_log_fd() -> Option<i32> {
    if !RAFT_LOG_INIT.load(core::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let fd = RAFT_LOG_FD.load(core::sync::atomic::Ordering::Acquire);
    if fd >= 0 { Some(fd) } else { None }
}

/// Append one formatted line to the raft log, allocating nothing.
///
/// **The raft thread logs here and nowhere else.** It used to `libakuma::print`
/// to the shared stdout that herd captures, and on the Firecracker guest that
/// output was provably unreliable: the loop printed `tick 1/2/3 start` and none
/// of the lines that sit between them in program order, which cannot happen in
/// one thread unless the writes themselves are being lost. Two threads
/// formatting into one captured fd is not a diagnostic you can trust when the
/// thing you are diagnosing is those two threads.
macro_rules! raft_logf {
    ($n:expr, $($arg:tt)*) => {{
        if let Some(fd) = raft_log_fd() {
            let stamp = crate::util::now_us() / 1_000_000;
            libakuma::safe_write!(fd, 32, "[{}] ", stamp);
            libakuma::safe_write!(fd, $n, $($arg)*);
            libakuma::write_fd(fd, b"\n");
        }
    }};
}

fn raft_log(line: &str) {
    raft_logf!(256, "{}", line);
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

    // Raft log rides this agent's own scope from here on, appended across
    // restarts (see the note above `set_raft_log`).
    let raft_log_path = set_raft_log();
    raft_log(&format!("start agent={} hub={} model={}", me, addr, model));
    libakuma::safe_print!(320, "[live] raft log: {}\n", raft_log_path);

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
            // **Not discarded.** This line used to print "raft thread up"
            // unconditionally, so a leader with no raft thread announced itself
            // exactly like a healthy one and the log was actively misleading
            // while the hub answered nobody.
            let raft_up = start_raft_thread(&listener, &state, static_peers);
            libakuma::safe_print!(
                192,
                "[live] {} holds the hub at {} (won the bind race) — raft thread {}\n",
                me,
                addr,
                if raft_up { "up" } else { "DOWN (serving from the agent loop only)" }
            );
            raft_log(&format!("state=leader term=1 hub={} raft={}", addr, if raft_up { "up" } else { "down" }));
            Machine::Leader { listener, state }
        }
        Err(_) => {
            libakuma::safe_print!(160, "[live] {} joined the litter at {} (hub already up)\n", me, addr);
            raft_log(&format!("state=follower hub={}", addr));
            Machine::Follower { epoch: 0, roster: Vec::new(), term: 0 }
        }
    };

    // Baseline: history predates us — wake only on what arrives from now.
    let mut seen = inbox_count(&addr, &me);
    libakuma::safe_print!(128, "[live] {} awake; {} message(s) already in history\n", me, seen);

    let mut tick: u64 = 0;
    let mut latest_term: u64 = 0; // highest term any pulse reported

    loop {
        tick += 1;
        // What has the raft thread managed? Reported from HERE, by the main
        // thread, because the child's own logging is one of the things under
        // suspicion — a counter it stores costs it one atomic and needs no
        // syscall, so a child that is alive but cannot do I/O still shows up.
        if tick % 10 == 0 {
            raft_logf!(
                96,
                "agent tick {} sees RAFT_TICKS={} alive={}",
                tick,
                RAFT_TICKS.load(core::sync::atomic::Ordering::Acquire),
                RAFT_ALIVE.load(core::sync::atomic::Ordering::Acquire)
            );
        }

        match &mut machine {
            Machine::Leader { listener, state } => {
                // The agent loop drains too: two serve points on one
                // listener, so requests never wait a full raft tick.
                serve::drain(&listener, &mut state.lock());
                // Read our OWN inbox straight out of the state we are
                // holding, never over a socket to ourselves.
                //
                // `inbox_count`'s client call is a TCP round trip to
                // `addr`, and when we are the leader that is our own
                // listener. The only code that can accept and answer it is
                // `serve::drain` — which runs on this thread. With a raft
                // thread alive the other thread answers and nobody notices;
                // with no raft thread (measured: Akuma/amd64, where
                // `spawn_detached` fails) this thread blocks waiting for a
                // reply only it could send, times out every tick, and reads
                // its own inbox as permanently empty. The leader then never
                // wakes, never runs a turn, and never serves anyone —
                // a litter with a leader that does nothing at all.
                let count = local_inbox_count(&mut state.lock(), &me);
                if count > seen {
                    run_turn(&me, &model, &provider, &system_prompt);
                    serve::drain(&listener, &mut state.lock());
                    seen = local_inbox_count(&mut state.lock(), &me);
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
                                libakuma::safe_print!(256, "[live] {} hears: {}\n", me, event);
                            }
                            *epoch = e;
                        }
                        Ok(_) => {}
                        Err(_) => { /* probe failed: liveness bookkeeping simply doesn't advance */ }
                    }
                    if hub::unresponsive() {
                        libakuma::safe_print!(128, "[live] {} is WAYWARD: hub silent for {}s\n", me, hub::silent_for_secs());
                        raft_log(&format!("state=wayward silent_for_s={}", hub::silent_for_secs()));
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
                        libakuma::safe_print!(160, "[live] {} re-raced the bind and WON — leader again (term {})\n", me, latest_term.max(*term) + 1);
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
                                libakuma::safe_print!(160, "[live] {} is FOLLOWER again (hub answered after {}s)\n", me, hub::silent_for_secs());
                                raft_log(&format!("state=follower recovered silent_for_s={}", hub::silent_for_secs()));
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

/// Flipped by the raft thread itself, as its first act.
///
/// `spawn_detached` returning `true` is **not** evidence the thread runs.
/// Measured 2026-09-20 on the Firecracker guest: it returned true, no warning
/// printed, and `/proc/<pid>/status` said `Threads: 1` — the hub then accepted
/// connections that nothing ever answered, and the agent loop parked in a futex
/// on a lock the absent thread was supposed to release. The clone reporting
/// success and the child never running are two different facts, so the parent
/// waits for the child to say so itself. Same shape as `rt.rs`'s own
/// `spawn_detached` test.
static RAFT_ALIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Bumped by the raft thread at the top of every tick, read by the **main**
/// thread. A counter, not a log line, because the question being asked is
/// whether the child can do anything at all — and the child's own logging is
/// one of the things under suspicion.
static RAFT_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// How long the parent waits for the child to announce itself. Generous: this
/// runs once, at startup, and a false "it did not start" would be worse than
/// the wait.
const RAFT_START_TIMEOUT_MS: u64 = 2000;

fn raft_entry() {
    RAFT_ALIVE.store(true, core::sync::atomic::Ordering::Release);
    raft_logf!(64, "raft_entry running");
    let ptr = RAFT_CTX.load(core::sync::atomic::Ordering::Acquire) as *const RaftCtx;
    if ptr.is_null() {
        raft_logf!(64, "raft_entry: RAFT_CTX is NULL, thread exits");
        return;
    }
    // SAFETY: the ctx was leaked by start_raft_thread before the child was
    // spawned; the child is its only reader and the process outlives it.
    let ctx = unsafe { &*ptr };
    raft_tick_loop(ctx.listener.clone(), ctx.state.clone(), &ctx.peers);
}

/// Leak the context and spawn the raft thread. Returns false when the thread is
/// not **running** — which is a stronger claim than "the clone succeeded", and
/// deliberately so (see [`RAFT_ALIVE`]). The caller keeps running as leader
/// with its own drain — degraded, not dead.
fn start_raft_thread(listener: &Arc<TcpListener>, state: &Arc<PMutex<HubState>>, static_peers: Vec<StaticPeer>) -> bool {
    let ctx: &'static RaftCtx = Box::leak(Box::new(RaftCtx {
        listener: listener.clone(),
        state: state.clone(),
        peers: PMutex::new(static_peers),
    }));
    RAFT_CTX.store(ctx as *const RaftCtx as u64, core::sync::atomic::Ordering::Release);
    let spawned = unsafe { crate::rt::spawn_detached(raft_entry) };
    // Wait for the child to say it is running, rather than trusting the clone's
    // return value.
    let mut alive = false;
    if spawned {
        let mut waited = 0;
        while waited < RAFT_START_TIMEOUT_MS {
            if RAFT_ALIVE.load(core::sync::atomic::Ordering::Acquire) {
                alive = true;
                break;
            }
            libakuma::sleep_ms(20);
            waited += 20;
        }
        if !alive {
            libakuma::print(
                "[live] WARNING: raft thread was created but never ran - this agent cannot serve the hub\n",
            );
            raft_log("raft thread created but never ran");
        }
    }
    if !spawned {
        // Loud, because everything downstream assumes this thread exists:
        // the hub is served from here, not from the agent loop, and an
        // agent whose raft thread never started is a leader that stops
        // answering the moment it starts thinking.
        libakuma::print("[live] WARNING: raft thread failed to spawn - this agent cannot serve the hub\n");
        raft_log("raft thread spawn FAILED");
    }
    alive
}

fn raft_tick_loop(listener: Arc<TcpListener>, state: Arc<PMutex<HubState>>, static_peers: &PMutex<Vec<StaticPeer>>) {
    raft_logf!(64, "raft loop entered");
    let mut tick: u64 = 0;
    // Heartbeat counters. A hub that answers nobody is indistinguishable from
    // outside from one whose thread never runs, whose probe never returns, and
    // whose drain never accepts — three different faults that all present as
    // "went silent before answering". These tell them apart from the log alone.
    let mut served_total: usize = 0;
    let mut probes_ok: u64 = 0;
    let mut probes_fail: u64 = 0;
    loop {
        tick += 1;
        RAFT_TICKS.store(tick, core::sync::atomic::Ordering::Release);
        if tick <= 8 {
            raft_logf!(64, "raft tick {} top", tick);
        }
        // Probe I/O first, holding NOTHING. `probe_static_peers` used to run
        // inside the state lock below, which put a cross-host TCP round trip
        // — one per peer, at that peer's timeout — in the middle of the
        // hub's critical section. Two litters pointed at each other then
        // deadlock each other in slow motion: our probe waits on their hub,
        // which is locked waiting on its probe of ours. It looks exactly
        // like "the leader goes deaf during an LLM turn" and has nothing to
        // do with the LLM; it appears the moment a peer is configured.
        let pulse = tick % PULSE_TICKS == 0;
        let probe_results = if pulse {
            // Only the peers due this pulse. A peer that answered is due every
            // time; a silent one is due on a doubling schedule, because probing
            // it is paid for out of this hub's own serving time (see
            // `StaticPeer::probe_failures`).
            let pulse_no = tick / PULSE_TICKS;
            let targets: Vec<(String, String)> = static_peers
                .lock()
                .iter()
                .filter(|p| {
                    let every = 1u64 << p.probe_failures.min(PROBE_BACKOFF_MAX);
                    pulse_no.is_multiple_of(every)
                })
                .map(|p| (p.name.clone(), p.addr.clone()))
                .collect();
            probe_peers_io(&targets)
        } else {
            Vec::new()
        };

        if tick <= 8 {
            raft_logf!(64, "tick {} probed ({} results)", tick, probe_results.len());
        }
        for (_, _, reachable) in &probe_results {
            if *reachable { probes_ok += 1 } else { probes_fail += 1 }
        }
        let batch = {
            let mut st = state.lock();
            served_total += serve::drain(&listener, &mut st);
            if pulse {
                st.task_tick();
                let mut peers = static_peers.lock();
                apply_probe_results(&mut st, &mut peers, &probe_results);
                relay_jobs(&mut st, &mut peers)
            } else {
                Vec::new()
            }
        };
        if tick <= 8 {
            raft_logf!(64, "tick {} drained (served={})", tick, served_total);
        }
        // The heartbeat: enough to tell a loop that is not running from one
        // whose probe never returns from one whose drain never accepts. All
        // three present identically from outside as "went silent before
        // answering".
        if tick % HEARTBEAT_TICKS == 0 {
            raft_logf!(
                128,
                "tick={} served={} probes ok={} fail={}",
                tick, served_total, probes_ok, probes_fail
            );
        }
        if tick % COMPACT_TICKS == 0 {
            state.lock().compact();
        }
        // Relay I/O runs OUTSIDE the state lock: a slow peer costs its own
        // send deadlines, never the hub (same rule as deadline-bounded
        // serve_one — see docs/LITTER_RELAY_TOPOLOGY.md for where this goes
        // long-term).
        if !batch.is_empty() {
            relay_send(&batch, static_peers);
        }
        libakuma::sleep(TICK_SECS);
    }
}

/// Snapshot the outbound relay batch for every online peer, oldest first.
/// Pure state read — the I/O happens in `relay_send`, outside the locks.
/// Entries whose message already carries an envelope (`ol`) are relayed-in
/// copies and can't occur in the log (capture rejects them upstream), but
/// the guard stays as belt-and-braces: relay is exactly one hop.
/// Age guard: chat older than RELAY_MAX_AGE_US is never relayed — a peer
/// that comes back after a long partition resyncs through history, it does
/// not get a replay of the debate it missed (see docs/LITTER_RELAY_TOPOLOGY.md).
/// One message queued for one peer hub. A struct rather than a tuple
/// because two timestamps travel here and they are NOT interchangeable:
/// `ot` is the sender's clock (what its signature commits to, what goes on
/// the wire) and `cursor_ts` is our hub's own monotonic stamp (what the
/// peer cursor is measured in). They were the same number back when the
/// hub minted the signature; conflating them now would compare a remote
/// agent's clock against a local cursor and skip or re-send traffic.
pub struct RelayJob {
    pub addr: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub round: i64,
    pub ol: String,
    pub ot: u64,
    pub sig: String,
    pub cursor_ts: u64,
}

fn relay_jobs(st: &mut HubState, peers: &mut [StaticPeer]) -> Vec<RelayJob> {
    if super::sig::our_litter_name().is_none() {
        return Vec::new(); // relay off: no litter identity configured
    }
    let now = crate::util::now_us();
    let fresh_enough = |ts: u64| now.saturating_sub(ts) <= RELAY_MAX_AGE_US;
    let mut jobs = Vec::new();
    for peer in peers.iter().filter(|p| p.online) {
        for entry in &st.relay_log {
            if entry.msg.ts <= peer.last_relay_ts
                || entry.msg.kind != MessageKind::Chat
                || entry.msg.ol.is_some()
                || !fresh_enough(entry.msg.ts)
            {
                continue;
            }
            // Routing: group broadcast stays group; direct traffic to a
            // peer's agent keeps the bare name (the far side delivers to
            // its own agent); everything else is litter-local, not relayed.
            let to = if entry.to == serve::GROUP_NAME {
                String::from(serve::GROUP_NAME)
            } else if litter_wire::is_valid_name(&entry.to) {
                entry.to.clone()
            } else {
                continue;
            };
            jobs.push(RelayJob {
                addr: peer.addr.clone(),
                from: entry.msg.from.clone(),
                to,
                body: entry.msg.body.clone(),
                round: entry.msg.round,
                ol: entry.origin_litter.clone(),
                ot: entry.origin_ts,
                sig: entry.origin_sig.clone(),
                cursor_ts: entry.msg.ts,
            });
        }
    }
    jobs
}

/// Drive one relay batch. The relayer signs the same canonical payload the
/// sender signed (`rs`) and stamps its own **agent** name (`rl`) — that is
/// what "who carried it here" means; the sender's `sig` is passed through
/// untouched. The cursor only advances on a confirmed `Sent`, in our hub's
/// clock, so an unreachable peer simply re-drives its batch on a later
/// tick (bounded by the relay log's cap and the age guard, never queued
/// unboundedly).
fn relay_send(jobs: &[RelayJob], static_peers: &PMutex<Vec<StaticPeer>>) {
    let Some(me) = super::agent_name() else { return };
    for job in jobs {
        let payload = super::sig::relay_payload(&job.ol, &job.from, &job.to, &job.body, job.ot);
        let Some(rs) = super::sig::sign_payload(&payload) else { return };
        let req = Request::Send {
            from: job.from.clone(),
            to: job.to.clone(),
            body: job.body.clone(),
            round: job.round,
            ol: Some(job.ol.clone()),
            ot: Some(job.ot),
            sig: Some(job.sig.clone()),
            rl: Some(me.clone()),
            rs: Some(rs),
        };
        match hub::call_addr(&job.addr, &req) {
            Ok(Response::Sent { .. }) => {
                raft_log(&format!("relay out to={} from={} round={} ot={}", job.addr, job.from, job.round, job.ot));
                let mut peers = static_peers.lock();
                if let Some(p) = peers.iter_mut().find(|p| p.addr == job.addr) {
                    p.last_relay_ts = p.last_relay_ts.max(job.cursor_ts);
                }
            }
            _ => raft_log(&format!("relay fail to={} from={} ot={}", job.addr, job.from, job.ot)),
        }
    }
}

/// Ask each configured static peer "who's there?" — the trashcan/laptop
/// discovery loop. First answer registers the peer in the roster (with an
/// event, so every agent hears about it); silence after being online
/// raises a "lost" event. The peer's own litter is unaffected: we only
/// read its public pulse.
/// The probe's **network half**: ask each peer "who's there?" holding no
/// lock at all. Returns one `(name, addr, reachable)` per target, in the
/// order given. This is a cross-host round trip per peer and it is allowed
/// to be slow — that is precisely why nothing may be held while it runs.
fn probe_peers_io(targets: &[(String, String)]) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for (name, addr) in targets {
        // `hub::peers` targets `hub_addr()`, so for a remote peer issue the
        // request directly through a one-shot connect on its address.
        //
        // **On a probe budget, not a request budget** — see
        // `hub::call_addr_timeout`. This call is inline in the thread that
        // serves the hub, so its timeout is how long this litter stops
        // answering; at the request budget two litters that list each other
        // starve each other's serve loops indefinitely.
        let reachable = matches!(
            super::hub::call_addr_timeout(addr, &Request::Peers { since: 0 }, PROBE_TIMEOUT_US),
            Ok(Response::Peers { .. })
        );
        out.push((name.clone(), addr.clone(), reachable));
    }
    out
}

/// The probe's **state half**: fold the results in and emit the
/// discovered/lost transitions. Pure bookkeeping, no I/O — safe to run
/// under the locks.
///
/// Results are matched back by address rather than by index: the peer list
/// is re-locked between the two halves, so position is not a stable key.
fn apply_probe_results(st: &mut HubState, peers: &mut [StaticPeer], results: &[(String, String, bool)]) {
    for (_, addr, reachable) in results {
        let Some(peer) = peers.iter_mut().find(|p| &p.addr == addr) else { continue };
        // Backoff bookkeeping first: an answer clears it outright, silence
        // doubles the interval up to the cap.
        if *reachable {
            peer.probe_failures = 0;
        } else {
            peer.probe_failures = peer.probe_failures.saturating_add(1).min(PROBE_BACKOFF_MAX);
        }
        if *reachable && !peer.online {
            peer.online = true;
            st.event(format!("[event] static peer {} discovered at {}", peer.name, peer.addr));
        } else if !*reachable && peer.online {
            peer.online = false;
            st.event(format!("[event] static peer {} lost ({})", peer.name, peer.addr));
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

/// Count wakeable messages in our own inbox from the hub state we already
/// hold. The leader's version of `inbox_count`, with no socket in it — see
/// the note in the `Machine::Leader` arm for why that distinction is
/// load-bearing rather than an optimization.
fn local_inbox_count(st: &mut HubState, me: &str) -> usize {
    match st.handle(Request::Inbox { name: alloc::string::String::from(me) }) {
        Response::Inbox { messages } => messages.iter().filter(|m| wakeable(m)).count(),
        _ => 0,
    }
}

/// Count wakeable messages by asking the hub over the network. For
/// FOLLOWERs and WAYWARD agents, where the hub really is another process.
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

    libakuma::safe_print!(128, "\n[live] {} wakes on new inbox activity\n", me);
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
        libakuma::safe_print!(256, "[live] {}'s turn failed: {}\n", me, e);
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
    use alloc::vec;
    use alloc::vec::Vec;

    use litter_wire::{Request, Response};

    use super::{parse_static_peers, relay_jobs, StaticPeer};
    use crate::tools::litter::serve::{HubState, RelayEntry, GROUP_NAME};
    use crate::util::now_us;

    struct Litter {
        name: &'static str,
        /// This litter's own hub address (what peers aim at).
        addr: &'static str,
        /// The agents living here. Each one is a signing identity of its
        /// own — a litter does not sign, its agents do.
        agents: Vec<&'static str>,
        hub: HubState,
        peers: Vec<StaticPeer>,
    }

    /// A deterministic 32-byte seed per AGENT name, derivable by every
    /// participant in the sim. Mixing the index in matters: a seed built
    /// from the first byte alone gave `al` and `amber` the same key, which
    /// would have made a per-agent identity test pass for the wrong reason.
    fn seed_for(name: &str) -> [u8; 32] {
        let b = name.as_bytes();
        core::array::from_fn(|i| b[i % b.len()].wrapping_add(i as u8))
    }

    fn key_hex_for(name: &str) -> String {
        hex(&seed_for(name))
    }

    fn pub_hex_for(name: &str) -> String {
        use ed25519_dalek::SigningKey;
        hex(&SigningKey::from_bytes(&seed_for(name)).verifying_key().to_bytes())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("")
    }

    impl Litter {
        /// A litter with `agents` joined and one leader, peering at `peers`
        /// (links simulated up).
        fn seed(name: &'static str, addr: &'static str, agents: &[&'static str], peers_spec: &str) -> Self {
            let mut hub = HubState::new();
            hub.set_leader(agents[0], 1);
            for a in agents {
                hub.handle(Request::Join { name: String::from(*a) });
            }
            let mut peers = parse_static_peers(Some(peers_spec));
            for p in peers.iter_mut() {
                p.online = true;
            }
            Litter { name, addr, agents: agents.to_vec(), hub, peers }
        }

        fn inbox_has(&mut self, agent: &str, body: &str) -> bool {
            matches!(
                self.hub.handle(Request::Inbox { name: String::from(agent) }),
                Response::Inbox { messages } if messages.iter().any(|m| m.body == body)
            )
        }

        fn inbox_from_has(&mut self, agent: &str, from: &str) -> bool {
            matches!(
                self.hub.handle(Request::Inbox { name: String::from(agent) }),
                Response::Inbox { messages } if messages.iter().any(|m| m.from == from)
            )
        }
    }

    /// The guest list every simulated node pins: the public key of every
    /// agent in the world. Keys belong to agents, so this is a list of
    /// agents, not of litters (`verify_payload` ignores the names anyway —
    /// it is a guest list, not a binding — but building it this way keeps
    /// the sim honest about what is being pinned).
    fn guest_list(world: &[(&'static str, Litter)]) -> String {
        let mut out = Vec::new();
        for (_, l) in world {
            for a in &l.agents {
                out.push(format!("{}:{}", a, pub_hex_for(a)));
            }
        }
        out.join(",")
    }

    /// Become `agent`, a member of litter `l`: its litter identity, that
    /// AGENT's signing key, and the world's guest list.
    fn become_agent(l: &Litter, agent: &str, guests: &str) {
        crate::tools::litter::set_litter_name(Some(String::from(l.name)));
        crate::tools::litter::sig::set_our_key(Some(&key_hex_for(agent)));
        crate::tools::litter::sig::set_peer_keys(Some(guests));
    }

    /// One agent says something into its own hub — signed in "its process"
    /// (i.e. under its own key), exactly as `hub::tool_send_message` does.
    fn say(world: &mut [(&'static str, Litter)], litter: &str, agent: &str, to: &str, body: &str, round: i64) -> Response {
        let guests = guest_list(world);
        let i = world.iter().position(|(n, _)| *n == litter).expect("litter");
        become_agent(&world[i].1, agent, &guests);
        let req = crate::tools::litter::sig::signed_send(agent, to, body, round);
        world[i].1.hub.handle(req)
    }

    /// The mocked transport: one relay tick. The relaying agent is the
    /// litter's leader (whoever holds the hub), so the hop is signed with
    /// ITS key and stamped with ITS name — the sender's `sig` rides along
    /// untouched. Delivery happens under the receiving agent's identity,
    /// since that is whose guest list must verify the envelope. Returns
    /// how many frames crossed a link.
    fn relay_tick(world: &mut [(&'static str, Litter)], from: &str) -> usize {
        let guests = guest_list(world);
        let i = world.iter().position(|(n, _)| *n == from).expect("litter");
        let relayer = world[i].1.agents[0]; // the leader holds the hub
        become_agent(&world[i].1, relayer, &guests);
        let jobs = relay_jobs(&mut world[i].1.hub, &mut world[i].1.peers);
        let mut crossed = 0usize;
        for job in jobs {
            let dest = world.iter().position(|(_, l)| l.addr == job.addr).expect("peer litter");
            let payload = crate::tools::litter::sig::relay_payload(&job.ol, &job.from, &job.to, &job.body, job.ot);
            let rs = crate::tools::litter::sig::sign_payload(&payload).expect("signer");
            let req = Request::Send {
                from: job.from.clone(),
                to: job.to.clone(),
                body: job.body.clone(),
                round: job.round,
                ol: Some(job.ol.clone()),
                ot: Some(job.ot),
                sig: Some(job.sig.clone()),
                rl: Some(String::from(relayer)),
                rs: Some(rs),
            };
            let receiver = world[dest].1.agents[0];
            become_agent(&world[dest].1, receiver, &guests);
            let accepted = matches!(world[dest].1.hub.handle(req), Response::Sent { .. });
            become_agent(&world[i].1, relayer, &guests);
            if accepted {
                let peer = world[i].1.peers.iter_mut().find(|p| p.addr == job.addr).unwrap();
                peer.last_relay_ts = peer.last_relay_ts.max(job.cursor_ts);
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
        //    island inbox, from-line prefixed with the origin litter
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al", "amber"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob", "bella"], "yard@10.0.0.1:7700")),
            ];
            say(&mut sw, "yard", "al", GROUP_NAME, "hello island", 1);
            let crossed = relay_tick(&mut sw, "yard");
            let ok = crossed == 1
                && sw[1].1.inbox_has("bob", "hello island")
                && sw[1].1.inbox_has("bella", "hello island")
                && sw[1].1.inbox_from_has("bob", "al"); // sender never rewritten
            if ok { passed += 1; } else { libakuma::print(&format!("  [!] join: crossed={}\n", crossed)); }
        }

        // 2. storm bound: relayed-in traffic is never re-captured — one
        //    message crosses one link exactly once, no matter the ticks
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            say(&mut sw, "yard", "al", GROUP_NAME, "once only", 1);
            let first = relay_tick(&mut sw, "yard");
            let again = relay_tick(&mut sw, "yard") + relay_tick(&mut sw, "island") + relay_tick(&mut sw, "island");
            if first == 1 && again == 0 { passed += 1; }
            else { libakuma::print(&format!("  [!] storm bound: first={} again={}\n", first, again)); }
        }

        // 3. direct cross-litter reply: island's bob → yard's al arrives
        //    only in al's inbox. The name on the envelope is BARE (`al`,
        //    not `yard-al`): provenance rides `ol`/`rl`, never the name,
        //    so the far hub delivers to its own agent of that name.
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al", "amber"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            say(&mut sw, "island", "bob", "al", "reply to yard", 2);
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
                msg: litter_wire::Message::chat(String::from("al"), 1, String::from("stale news"), old),
                origin_litter: String::from("yard"),
                origin_ts: old,
                origin_sig: hex(&[0u8; 64]),
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
            say(&mut sw, "yard", "al", GROUP_NAME, "across the flake", 1);
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
            say(&mut sw, "a", "x", GROUP_NAME, "mesh news", 1);
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

        // 7. replay dedup: the same envelope (same ol/ot) delivered twice
        //    is accepted twice at the wire but lands in inboxes ONCE
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            let guests = guest_list(&sw);
            say(&mut sw, "yard", "al", GROUP_NAME, "no double", 1);
            assert_eq!(relay_tick(&mut sw, "yard"), 1);
            let first = sw[1].1.inbox_has("bob", "no double");
            let before = count(&mut sw[1].1, "bob");

            // Replay the exact envelope that just crossed, by hand and with
            // a FRESH relayer signature — the cursor cannot help here, so
            // this is the seen-set doing the work.
            let entry = &sw[0].1.hub.relay_log[0];
            let (ol, ot, sig) = (entry.origin_litter.clone(), entry.origin_ts, entry.origin_sig.clone());
            let body = entry.msg.body.clone();
            become_agent(&sw[0].1, "al", &guests);
            let payload = crate::tools::litter::sig::relay_payload(&ol, "al", GROUP_NAME, &body, ot);
            let rs = crate::tools::litter::sig::sign_payload(&payload).expect("signer");
            become_agent(&sw[1].1, "bob", &guests);
            let replayed = sw[1].1.hub.handle(Request::Send {
                from: String::from("al"),
                to: String::from(GROUP_NAME),
                body,
                round: 1,
                ol: Some(ol),
                ot: Some(ot),
                sig: Some(sig),
                rl: Some(String::from("al")),
                rs: Some(rs),
            });
            let after = count(&mut sw[1].1, "bob");
            // Accepted at the wire (it is well-formed and verifies), but
            // delivered exactly once.
            let ok = first && matches!(replayed, Response::Sent { .. }) && before == after;
            if ok { passed += 1; }
            else { libakuma::print(&format!("  [!] replay: first={} before={} after={}\n", first, before, after)); }
        }

        // 8. forged envelope: a body tampered after signing is refused
        //    outright (Error, nothing delivered)
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            // al signs the REAL payload with al's own (guest-listed) key,
            // then the body is swapped underneath it. The relayer signature
            // is made over the TAMPERED payload and is therefore valid, so
            // the only thing wrong with this envelope is the origin
            // signature — which is exactly what must reject it.
            let guests = guest_list(&sw);
            become_agent(&sw[0].1, "al", &guests);
            let real = crate::tools::litter::sig::relay_payload("yard", "al", GROUP_NAME, "signed as-is", 42);
            let sig = crate::tools::litter::sig::sign_payload(&real).expect("signer");
            let tampered = crate::tools::litter::sig::relay_payload("yard", "al", GROUP_NAME, "TAMPERED", 42);
            let rs = crate::tools::litter::sig::sign_payload(&tampered).expect("signer");
            become_agent(&sw[1].1, "bob", &guests);
            let refused = matches!(
                sw[1].1.hub.handle(Request::Send {
                    from: String::from("al"),
                    to: String::from(GROUP_NAME),
                    body: String::from("TAMPERED"),
                    round: 1,
                    ol: Some(String::from("yard")),
                    ot: Some(42),
                    sig: Some(sig),
                    rl: Some(String::from("al")),
                    rs: Some(rs),
                }),
                Response::Error { .. }
            );
            let quiet = !sw[1].1.inbox_has("bob", "TAMPERED") && !sw[1].1.inbox_has("bob", "signed as-is");
            if refused && quiet { passed += 1; }
            else { libakuma::print(&format!("  [!] forged: refused={} quiet={}\n", refused, quiet)); }
        }

        // 9. permissive default: with NO guest list configured, an envelope
        //    from a litter we have never heard of is accepted and delivered.
        //    This is the shipped default (`litter_peer_keys` unset) — two
        //    fresh litters join by pointing at each other and nothing else.
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            // An agent nobody has a key for signs a well-formed envelope.
            crate::tools::litter::set_litter_name(Some(String::from("stranger")));
            crate::tools::litter::sig::set_our_key(Some(&key_hex_for("zed")));
            let payload = crate::tools::litter::sig::relay_payload("stranger", "zed", GROUP_NAME, "knock knock", 77);
            let sig = crate::tools::litter::sig::sign_payload(&payload).expect("signer");
            let rs = crate::tools::litter::sig::sign_payload(&payload).expect("signer");

            // Receiver: island's own identity, guest list explicitly unset.
            become_agent(&sw[1].1, "bob", "");
            crate::tools::litter::sig::set_peer_keys(None);
            let accepted = matches!(
                sw[1].1.hub.handle(Request::Send {
                    from: String::from("zed"),
                    to: String::from(GROUP_NAME),
                    body: String::from("knock knock"),
                    round: 1,
                    ol: Some(String::from("stranger")),
                    ot: Some(77),
                    sig: Some(sig),
                    rl: Some(String::from("zed")),
                    rs: Some(rs),
                }),
                Response::Sent { .. }
            );
            // A broken signature is still refused, guest list or not.
            let bad_refused = matches!(
                sw[1].1.hub.handle(Request::Send {
                    from: String::from("zed"),
                    to: String::from(GROUP_NAME),
                    body: String::from("garbage"),
                    round: 1,
                    ol: Some(String::from("stranger")),
                    ot: Some(78),
                    sig: Some(String::from("00")),
                    rl: Some(String::from("zed")),
                    rs: Some(String::from("00")),
                }),
                Response::Error { .. }
            );
            let ok = accepted
                && sw[1].1.inbox_has("bob", "knock knock")
                && sw[1].1.inbox_from_has("bob", "zed")
                && bad_refused
                && !sw[1].1.inbox_has("bob", "garbage");
            if ok { passed += 1; }
            else { libakuma::print(&format!("  [!] permissive: accepted={} bad_refused={}\n", accepted, bad_refused)); }
        }

        // 10. direct-message wake: a message addressed to ONE agent (not the
        //     group) is exactly what `live::run()`'s agent loop polls for via
        //     `local_inbox_count` — reproduces the trashcan->ryzen shape
        //     (kirill's chat relayed to a resident live process) and checks
        //     the piece observe can't see: whether the receiving side's own
        //     wake predicate actually flips. Three roles, each exercised
        //     through its real, unmodified function rather than a
        //     reimplementation: the sending litter's raft thread
        //     (`relay_jobs`/`relay_send`, mocked here as `relay_tick`, same
        //     as every other cross-litter test above), the receiving
        //     litter's raft thread accepting the `Request::Send` into
        //     `HubState::handle`, and the receiving agent loop's own wake
        //     check (`super::local_inbox_count` + `super::wakeable`, copied
        //     from nowhere — it's the literal fn `run()`'s Leader/Follower
        //     arms call). A regression here reads exactly like the live bug
        //     that motivated it: "a direct message arrived and nobody
        //     answered."
        total += 1;
        {
            let mut sw = vec![
                ("yard", Litter::seed("yard", "10.0.0.1:7700", &["al"], "island@10.0.0.2:7700")),
                ("island", Litter::seed("island", "10.0.0.2:7700", &["bob"], "yard@10.0.0.1:7700")),
            ];
            let i_yard = sw.iter().position(|(n, _)| *n == "yard").unwrap();
            // Baseline the receiving agent's own loop would have captured at
            // its own startup, before anything arrived (`live::run`'s `seen`).
            let seen = super::local_inbox_count(&mut sw[i_yard].1.hub, "al");

            say(&mut sw, "island", "bob", "al", "direct hello", 3);
            let crossed = relay_tick(&mut sw, "island");

            let count = super::local_inbox_count(&mut sw[i_yard].1.hub, "al");
            let woke = count > seen;
            let landed_wakeably = matches!(
                sw[i_yard].1.hub.handle(Request::Inbox { name: String::from("al") }),
                Response::Inbox { messages }
                    if messages.iter().any(|m| m.from == "bob" && m.body == "direct hello" && super::wakeable(m))
            );
            let ok = crossed == 1 && seen == 0 && woke && landed_wakeably;
            if ok { passed += 1; }
            else {
                libakuma::print(&format!(
                    "  [!] direct wake: crossed={} seen={} count={} woke={} landed_wakeably={}\n",
                    crossed, seen, count, woke, landed_wakeably
                ));
            }
        }

        libakuma::print(&format!("  result: {}/{}\n", passed, total));
        if passed == total { 0 } else { 1 }
    }

    fn count(l: &mut Litter, agent: &str) -> usize {
        match l.hub.handle(Request::Inbox { name: String::from(agent) }) {
            Response::Inbox { messages } => messages.len(),
            _ => 0,
        }
    }
}
