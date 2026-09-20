//! The hub, as a state machine any meow can carry — the no_std twin of
//! `crates/litter-hub`'s `HubState`, extended to the full coordinator role
//! (see `docs/LITTER_STATE_MACHINE.md`, `docs/LITTER_RAFT_LOOP.md`):
//! roster + inboxes, the cluster event log, term/leader status, the task
//! table (`tasks`), and marker-based history compaction.
//!
//! Threading: when this process wins the bind race, exactly ONE thread
//! owns this state — the owner loop (`live::owner_tick_loop`). Every method
//! takes `&mut self` and there is no lock anywhere, because there is no
//! sharing: the agent loop reads its own inbox over the socket like any
//! other client. See `docs/LITTER_WORKFLOW.md` § "Ownership".
//!
//! Request handling is a superset of `litter-hub`'s (join/send/inbox/peers,
//! name validation, roster cap), plus:
//! - `Send` bodies starting `[task]` / `[done: tN]` feed the task table;
//!   assignments and completions become cluster events.
//! - `Peers` is the pulse: roster + `term`/`leader` + event-log entries
//!   newer than the caller's `since` cursor. Elections, joins, leaves and
//!   task requeues all reach every agent through this one channel.
//! - `History { name, before, limit }` pages one inbox backwards so a
//!   cold-starting agent can walk up to the last `"[compacted: …]"` marker
//!   without ever pulling more than `limit` messages per round trip.
//! - `compact()` prunes each inbox past `KEEP_RECENT` recent messages and
//!   leaves ONE marker message summarizing what was folded — history below
//!   the marker is gone, and the marker is the boundary every `History`
//!   walk stops at.
//!
//! Like `litter-hub`, one connection carries exactly one framed
//! request/response pair; inboxes are non-destructive (`Inbox` peeks).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libakuma::net::{TcpListener, TcpStream};

use litter_wire::{
    decode_len_header, decode_request, encode_len_header, encode_response, is_valid_name,
    Message, Request, Response, MAX_FRAME_LEN,
};

use super::tasks::TableEvent;

pub use super::membership::{GROUP_NAME, KEEP_RECENT};
pub use super::relay::{RelayEntry, Seen};

use super::membership::Membership;
use super::record::Record;
use super::relay::RelayPlane;

/// The hub's whole state, as three parts that do not share invariants.
///
/// The split (2026-09-20) is by *concern*, not by size:
///
/// - [`Membership`] is the peer layer — who is here and what is in each
///   mailbox. It moves bytes and has no opinion about them.
/// - [`Record`] is the ordered log plus the application state machine it
///   drives (`tasks::TaskTable`). It decides what a message *means* and
///   what follows from it.
/// - [`RelayPlane`] is the cross-litter courier — neither local delivery
///   nor local consensus.
///
/// `handle` below is dispatch and orchestration only: it validates, works
/// out authority, and asks each part to do its own job. That is what makes
/// the whole thing a pure transition with no I/O in it — the property the
/// serving path depends on to keep the lock down to microseconds, and the
/// property a single-owner loop will depend on to need no lock at all.
pub struct HubState {
    pub members: Membership,
    pub record: Record,
    pub relay: RelayPlane,
}

impl HubState {
    /// A hub that knows nobody yet — the "empty seed roster" shape; the
    /// litter bootstraps itself.
    pub fn new() -> Self {
        Self { members: Membership::new(), record: Record::new(), relay: RelayPlane::new() }
    }

    /// A fresh hub whose binder has already joined and announced itself.
    /// Built by the winner of the bind race and handed straight to the
    /// owner thread, so the state is never shared even for a moment.
    pub fn new_seeded(leader: &str, term: u64) -> Self {
        let mut st = Self::new();
        // Emit the join event too, not just the roster entry: the binder
        // arrives in the litter the same way everyone else does, and an
        // event log that skips its own leader's arrival reads as a gap.
        if st.members.join(leader) {
            st.record.event(format!("[event] {} joined the litter", leader));
        }
        st.set_leader(leader, term);
        st
    }

    /// The bind-race winner announces itself.
    pub fn set_leader(&mut self, name: &str, new_term: u64) {
        self.record.set_leader(name, new_term);
    }

    /// Append one cluster event. Kept on the façade because callers
    /// outside this module (the peer probe, in `live.rs`) legitimately
    /// report status changes without caring how the log is stored.
    pub fn event(&mut self, text: String) {
        self.record.event(text);
    }

    fn now_us() -> u64 {
        crate::util::now_us()
    }

    /// Handle one already-decoded request and produce the response. Pure
    /// state transition, no I/O — the half the test suite drives directly.
    ///
    /// `now` is read ONCE here and threaded down, rather than each callee
    /// reading the clock: within one request every stamp should come from
    /// one instant, and a state machine that reads a clock in three places
    /// is three times as hard to test.
    pub fn handle(&mut self, req: Request) -> Response {
        let now = Self::now_us();
        match req {
            Request::Join { name } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                if !self.members.contains(&name) {
                    if self.members.is_full() {
                        return Response::Error { message: String::from("roster is full") };
                    }
                    self.members.join(&name);
                    self.record.event(format!("[event] {} joined the litter", name));
                }
                Response::Joined
            }
            Request::Send { from, to, body, round, ol, ot, sig, rl, rs } => {
                if !is_valid_name(&from) {
                    return Response::Error { message: String::from("'from' must be a plain agent name") };
                }
                if !is_valid_name(&to) {
                    return Response::Error { message: String::from("'to' must be a plain agent name") };
                }
                if body.is_empty() {
                    return Response::Error { message: String::from("send requires a non-empty body") };
                }
                let bytes = body.len();

                // Relay-plane split, first: is this relayed-in traffic?
                // `rl` is the discriminator, not `sig` — locally originated
                // messages are signed too now (the SENDER signs them, in
                // its own process), and only a message some agent actually
                // carried across a litter boundary has a relayer.
                //
                // Both signatures are checked against the same canonical
                // payload: `sig` proves which agent said it, `rs` which
                // agent brought it. Dedup is metadata-only, so a repeat is
                // dropped without touching any inbox, and `ol == our
                // litter` means our own words bounced back.
                if rl.is_some() {
                    let (Some(ol), Some(ot), Some(sig)) = (&ol, &ot, &sig) else {
                        return Response::Error { message: String::from("relayed send is missing its origin envelope") };
                    };
                    let Some(rs) = &rs else {
                        return Response::Error { message: String::from("relayed send is missing its relayer signature") };
                    };
                    let rl = match &rl { Some(rl) => rl, None => unreachable!() };
                    let payload = super::sig::relay_payload(ol, &from, &to, &body, *ot);
                    if !super::sig::verify_payload(&from, &payload, sig) {
                        return Response::Error { message: format!("origin signature from '{}' failed verification", from) };
                    }
                    if !super::sig::verify_payload(rl, &payload, rs) {
                        return Response::Error { message: format!("relayer signature from '{}' failed verification", rl) };
                    }
                    // Our own litter's words coming back to us: never
                    // deliver them a second time, whoever carried them.
                    if super::sig::our_litter_name().as_deref() == Some(ol.as_str()) {
                        return Response::Sent { bytes };
                    }
                    self.relay.prune_seen(now, super::live::RELAY_MAX_AGE_US);
                    if !self.relay.is_duplicate(ol, &from, *ot) {
                        self.relay.remember(Seen { ol: ol.clone(), from: from.clone(), ot: *ot, rl: rl.clone() });
                        // The sender is the sender: `from` stays the bare
                        // origin agent name — provenance rides the envelope
                        // (ol/rl), it never rewrites who said it.
                        let ts = self.members.stamp(now);
                        let msg = Message::chat(from.clone(), round, body.clone(), ts);
                        if to == GROUP_NAME {
                            self.members.broadcast(&msg);
                        } else {
                            self.members.deliver(&to, msg);
                        }
                    }
                    return Response::Sent { bytes };
                }

                // Sender role is hub-authoritative, stamped at delivery:
                // the leader's own messages are leader-traffic, the fixed
                // `root` identity is the operator, everyone else is a peer.
                //
                // NOTE: this trusts `from`. See `LITTER_WORKFLOW.md`
                // § "Future work" — authority should be recovered from the
                // signature, not read off a name the sender chose.
                let role = if self.record.is_leader(&from) {
                    litter_wire::SenderRole::Leader
                } else if from == "root" {
                    litter_wire::SenderRole::Root
                } else {
                    litter_wire::SenderRole::Peer
                };
                let ts = self.members.stamp(now);
                let msg = Message { role, ..Message::chat(from, round, body, ts) };

                // Relay-plane capture: every locally-originated outbound
                // chat is recorded (with its original `to` and its origin
                // signature, signed at the sender exactly once) for the
                // raft thread to forward. Envelope-carrying traffic never
                // reaches this branch, so no hub is ever a transit node:
                // relay is exactly one hop, by construction. Task/system
                // traffic (assignments, markers, done lines) stays local.
                if msg.kind == litter_wire::MessageKind::Chat {
                    // An unsigned message (no litter identity at the
                    // sender, or a client from before the envelope) is
                    // simply not relayable: it stays litter-local rather
                    // than going out over someone else's name.
                    if let (Some(ol), Some(ot), Some(sig)) = (&ol, &ot, &sig) {
                        self.relay.capture(RelayEntry {
                            to: to.clone(),
                            msg: msg.clone(),
                            origin_litter: ol.clone(),
                            origin_ts: *ot,
                            origin_sig: sig.clone(),
                        });
                    }
                }

                // Task-table hooks. Only the operator (root) or the leader
                // may OPEN tracked work — otherwise every peer chat that
                // starts with "[task]" mints work for the whole litter
                // (observed live: an agent tasking the litter to police the
                // kernel). Any holder may still close their own.
                let can_open = matches!(msg.role, litter_wire::SenderRole::Root | litter_wire::SenderRole::Leader);
                match self.record.tasks.note_message(&msg.from, &msg.body, msg.ts, can_open) {
                    TableEvent::Done(event_text, summary) => {
                        self.record.event(event_text);
                        // Completion knowledge belongs in history: a short
                        // system line under the group name.
                        let ts = self.members.stamp(now);
                        let done_line = Message {
                            kind: litter_wire::MessageKind::Done,
                            role: litter_wire::SenderRole::Leader,
                            ..Message::chat(String::from(GROUP_NAME), msg.round, format!("[done] {}", summary), ts)
                        };
                        self.members.broadcast(&done_line);
                    }
                    TableEvent::Noted => {}
                    TableEvent::Requeued => {}
                }

                if to == GROUP_NAME {
                    let echo = Message::chat(msg.from.clone(), msg.round, msg.body.clone(), msg.ts);
                    self.members.broadcast(&echo);
                } else {
                    self.members.deliver(&to, msg);
                }
                Response::Sent { bytes }
            }
            Request::Inbox { name } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                Response::Inbox { messages: self.members.inbox(&name) }
            }
            Request::History { name, before, limit } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                Response::Inbox { messages: self.members.history(&name, before, limit as usize) }
            }
            Request::Peers { since } => Response::Peers {
                names: self.members.roster().to_vec(),
                term: self.record.term,
                leader: self.record.leader.clone(),
                epoch: self.record.epoch(),
                events: self.record.events_since(since),
            },
        }
    }

    /// One coordinator tick over the task table: requeue expired leases,
    /// assign unassigned tasks, deliver the resulting assignment messages,
    /// and log the events. Cheap; called every few ticks by the raft
    /// thread.
    pub fn task_tick(&mut self) {
        let now = Self::now_us();
        let roster = self.members.roster().to_vec();
        let (messages, events) = self.record.tasks.tick(&roster, now);
        for (to, body) in messages {
            if !self.members.contains(&to) {
                continue;
            }
            let ts = self.members.stamp(now);
            self.members.deliver(&to, Message {
                kind: litter_wire::MessageKind::Assignment,
                role: litter_wire::SenderRole::Leader,
                ..Message::chat(String::from(GROUP_NAME), 0, body, ts)
            });
        }
        for e in events {
            self.record.event(e);
        }
    }

    /// Fold inbox history into one marker per inbox, carrying the open
    /// work across the boundary. Returns how many messages were folded
    /// (0 = nothing to do; the call is idempotent).
    pub fn compact(&mut self) -> usize {
        let now = Self::now_us();
        // The record says what is still open; membership makes it survive
        // the prune. Neither half knows the other's business.
        let carry = self.record.tasks.open_work_lines();
        let folded = self.members.compact(&carry, now);
        if folded > 0 {
            self.record.event(format!("[event] history compacted ({} message(s) folded)", folded));
        }
        folded
    }

    /// Serve exactly one framed request/response pair over an already
    /// accepted connection, holding `&mut self` throughout. Returns `false`
    /// on a transport error; protocol problems still get a well-formed
    /// `Response::Error` back.
    ///
    /// **This spans I/O and so must never be called with the state lock
    /// held** — that is what `drain` is for. It survives for the test
    /// suite, which owns its `HubState` outright and drives a socket pair
    /// with no second thread in sight.
    pub fn serve_one(&mut self, stream: &mut TcpStream) -> bool {
        let req = match read_request(stream) {
            Ok(req) => req,
            Err(Some(err)) => return write_frame(stream, &encode_response(&err)),
            Err(None) => return false,
        };
        let response = self.handle(req);
        write_frame(stream, &encode_response(&response))
    }
}

/// Read and decode one framed request. The **I/O half** of serving, and
/// the half that is allowed to wait: a client that dribbles its frame
/// costs this function up to `IO_TIMEOUT_US`, so no lock may be held
/// across it (see `LITTER_STATE_MACHINE.md` § "Lock discipline").
///
/// `Err(Some(response))` is a protocol problem the client should hear
/// about; `Err(None)` is a transport failure, where there is nothing left
/// to say it on.
fn read_request(stream: &mut TcpStream) -> Result<Request, Option<Response>> {
    // The stream was set nonblocking by drain(); every read here is
    // deadline-bounded so a half-open client can't park the caller.
    let mut header = [0u8; 4];
    if !deadline::read_exact(stream, &mut header, deadline::IO_TIMEOUT_US) {
        return Err(None);
    }
    let len = decode_len_header(header);
    if len > MAX_FRAME_LEN {
        return Err(Some(Response::Error {
            message: String::from("request frame too large"),
        }));
    }

    let mut body = alloc::vec![0u8; len as usize];
    if !deadline::read_exact(stream, &mut body, deadline::IO_TIMEOUT_US) {
        return Err(None);
    }

    match core::str::from_utf8(&body) {
        Ok(text) => match decode_request(text) {
            Ok(req) => Ok(req),
            Err(litter_wire::WireError::UnsupportedVersion(v)) => Err(Some(Response::Error {
                message: format!(
                    "unsupported protocol version {} (hub speaks {})",
                    v,
                    litter_wire::PROTOCOL_VERSION
                ),
            })),
            Err(litter_wire::WireError::Parse(reason)) => Err(Some(Response::Error {
                message: format!("malformed request: {}", reason),
            })),
        },
        Err(_) => Err(Some(Response::Error {
            message: String::from("request is not valid UTF-8"),
        })),
    }
}

/// Accept-and-serve every connection already waiting in the backlog.
/// Non-blocking (`TcpListener::try_accept`). Returns how many frames were
/// served (0 = quiet litter, no cost).
///
/// Takes `&mut HubState` because there is exactly **one owner**: whichever
/// thread holds the state holds all of it, and no other thread may touch
/// it. That is the whole synchronization story for the litter — there is
/// no lock here, and there is none to forget.
///
/// It did not start out that way. The state used to be an
/// `Arc<PMutex<HubState>>` driven by two threads, because the agent loop
/// read its own inbox directly while the raft thread served everyone else.
/// The lock that arbitrated them was held across every accept and every
/// byte read, which let the deadline poll hook re-enter it and wedge the
/// whole litter permanently (`LITTER_EXPERIMENT_PHASE_3.md` § 2). The fix
/// is not a better lock: it is that the agent loop stopped being a second
/// driver and now reads its inbox over the socket like any other client,
/// so the owner is the only writer and the socket is the queue.
pub fn drain(listener: &TcpListener, state: &mut HubState) -> usize {
    let mut served = 0usize;
    while let Ok((mut stream, _peer)) = listener.try_accept() {
        // Deadline-bounded serving: a client that never finishes its
        // request costs this loop its own timeout and nothing else. It
        // costs the client a retry — one request per connection, so a
        // retry is a fresh connection.
        let _ = libakuma::set_nonblocking(stream.as_raw_fd(), true);
        // Read first, apply second: the read can wait, `handle` cannot.
        // Trivially safe with one owner, but kept explicit because it is
        // the shape that stays correct if the owner ever shares again.
        let response = match read_request(&mut stream) {
            Ok(req) => state.handle(req),
            Err(Some(err)) => err,
            Err(None) => continue,
        };
        write_frame(&mut stream, &encode_response(&response));
        served += 1;
    }
    served
}

fn write_frame(stream: &mut TcpStream, payload: &str) -> bool {
    deadline::write_all(stream, &deadline::frame(payload), deadline::IO_TIMEOUT_US)
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter serve tests ---\n");

    // join → peers (with term/leader), idempotent join, join/leave events
    total += 1;
    {
        let mut hub = HubState::new();
        hub.set_leader("sherlock", 1);
        let j = hub.handle(Request::Join { name: String::from("sherlock") });
        hub.handle(Request::Join { name: String::from("sherlock") });
        let peers = hub.handle(Request::Peers { since: 0 });
        let joined = matches!(j, Response::Joined);
        let ok = matches!(&peers, Response::Peers { names, term, leader, events, .. }
            if names.len() == 1 && *term == 1 && leader.as_deref() == Some("sherlock")
               && events.iter().any(|e| e.contains("sherlock is leader"))
               && events.iter().filter(|e| e.contains("joined")).count() == 1);
        if joined && ok { passed += 1; }
        else { libakuma::print(&format!("  [!] join/peers: joined={:?} peers={:?}\n", j, peers)); }
    }

    // event cursor: since=last epoch returns nothing new
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("a") });
        let after = hub.handle(Request::Peers { since: 0 });
        let epoch = match &after {
            Response::Peers { epoch, .. } => *epoch,
            _ => 0,
        };
        let again = hub.handle(Request::Peers { since: epoch });
        let fresh = matches!(&again, Response::Peers { events, .. } if events.is_empty());
        if epoch > 0 && fresh { passed += 1; }
        else { libakuma::print(&format!("  [!] cursor: epoch={} again={:?}\n", epoch, again)); }
    }

    // history paging: newest-first take, delivered ascending
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("hercules") });
        for i in 0..10u64 {
            let r = hub.handle(Request::send(String::from("hercules"), String::from("hercules"), format!("m{}", i), i as i64));
            let _ = r;
        }
        // Give the messages distinct ts values by construction: send twice
        // (now_us granularity may collide), so instead read all and page by
        // the second-newest ts.
        let all = hub.handle(Request::Inbox { name: String::from("hercules") });
        let msgs = match all { Response::Inbox { messages } => messages, _ => Vec::new() };
        let before = msgs[8].ts; // everything from m8 back
        let page = hub.handle(Request::History { name: String::from("hercules"), before, limit: 3 });
        let paged = match page { Response::Inbox { messages } => messages, _ => Vec::new() };
        let ok = paged.len() == 3 && paged.iter().enumerate().all(|(i, m)| m.body == format!("m{}", 5 + i as u64));
        if ok { passed += 1; }
        else { libakuma::print(&format!("  [!] history page: {:?}\n", paged.iter().map(|m| m.body.clone()).collect::<Vec<_>>())); }
    }

    // group fan-out reaches every member
    total += 1;
    {
        let mut hub = HubState::new();
        for n in ["sherlock", "hercules", "zenigata"] {
            hub.handle(Request::Join { name: String::from(n) });
        }
        hub.handle(Request::send(String::from("sherlock"), String::from(GROUP_NAME), String::from("attention all"), 2));
        let every_member_has_it = ["sherlock", "hercules", "zenigata"].iter().all(|n| {
            matches!(hub.handle(Request::Inbox { name: String::from(*n) }),
                Response::Inbox { messages } if messages.iter().any(|m| m.body == "attention all"))
        });
        if every_member_has_it { passed += 1; }
        else { libakuma::print("  [!] group broadcast: some member did not receive the fan-out\n"); }
    }

    // compaction: fold past the tail into a marker; marker sits at inbox
    // head and carries the summary; History walks stop at it naturally
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("sherlock") });
        for i in 0..KEEP_RECENT + 5 {
            hub.handle(Request::send(String::from("sherlock"), String::from("sherlock"), format!("msg {}", i), i as i64));
        }
        let folded = hub.compact();
        let inbox = hub.handle(Request::Inbox { name: String::from("sherlock") });
        let msgs = match inbox { Response::Inbox { messages } => messages, _ => Vec::new() };
        let marker_first = msgs[0].body.starts_with("[compacted:") && msgs[0].body.contains("msg 0");
        let kept = msgs.len() == KEEP_RECENT + 1; // tail + marker
        let idempotent = hub.compact() == 0;
        if folded == 5 && marker_first && kept && idempotent {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] compact: folded={} first={:?} kept={} idem={}\n", folded, msgs.first().map(|m| m.body.clone()), kept, idempotent));
        }
    }

    // relay log: outbound chat is captured with its original `to` and the
    // SENDER's envelope, carried verbatim. An unsigned send is never
    // captured — the hub will not put words on the wire under a name it
    // cannot prove, so no identity/key means an empty log.
    total += 1;
    {
        crate::tools::litter::set_litter_name(Some(String::from("yard")));
        // A real 32-byte Ed25519 seed: `SigningKey::from_bytes` takes
        // exactly 32, and a short hex string silently leaves the key unset
        // (relay off) rather than failing loudly — which is what made this
        // test pass vacuously before.
        super::sig::set_our_key(Some(&"ab".repeat(32)));
        let mut hub = HubState::new();
        hub.handle(super::sig::signed_send("sherlock", GROUP_NAME, "broadcast", 1));
        hub.handle(super::sig::signed_send("sherlock", "tiger", "direct", 1));
        // ...and one unsigned send, which must not be relayable.
        hub.handle(Request::send(String::from("sherlock"), String::from(GROUP_NAME), String::from("unsigned"), 1));
        let ok = hub.relay.log.len() == 2
            && hub.relay.log[0].to == GROUP_NAME
            && hub.relay.log[1].to == "tiger"
            && hub.relay.log[0].msg.ts < hub.relay.log[1].msg.ts
            && hub.relay.log.iter().all(|e| {
                e.msg.kind == litter_wire::MessageKind::Chat
                    && e.origin_litter == "yard"
                    && e.origin_ts > 0
                    && !e.origin_sig.is_empty()
            });
        if ok { passed += 1; }
        else { libakuma::print(&format!("  [!] relay log: {:?}\n", hub.relay.log.iter().map(|e| (e.to.clone(), e.msg.ts)).collect::<Vec<_>>())); }
        crate::tools::litter::set_litter_name(None);
        super::sig::set_our_key(None);
    }

    // task hooks: [task] seeds the table, coordinator tick assigns and
    // delivers, [done: tN] closes and logs an event
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("sherlock") });
        hub.handle(Request::send(String::from("root"), String::from(GROUP_NAME), String::from("[task] audit main.rs"), 0));
        hub.task_tick();
        let inbox = hub.handle(Request::Inbox { name: String::from("sherlock") });
        let msgs = match inbox { Response::Inbox { messages } => messages, _ => Vec::new() };
        let got_assignment = msgs.iter().any(|m| m.body.starts_with("[assigned: t1]") && m.body.contains("audit main.rs"));
        let done = hub.handle(Request::send(String::from("sherlock"), String::from(GROUP_NAME), String::from("[done: t1] all clear"), 0));
        let peers = hub.handle(Request::Peers { since: 0 });
        let done_ok = matches!(done, Response::Sent { .. })
            && matches!(&peers, Response::Peers { events, .. } if events.iter().any(|e| e.contains("t1 done by sherlock")))
            && hub.record.tasks.is_empty();
        if got_assignment && done_ok { passed += 1; }
        else { libakuma::print(&format!("  [!] task hooks: got_assignment={} done_ok={}\n", got_assignment, done_ok)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

/// Deadline-bounded I/O over a NONBLOCKING stream. The whole point: a
/// hub client that goes silent (hung leader, half-open connection) must
/// cost the reader a bounded wait, never an unbounded one — otherwise a
/// dead hub freezes the very loop that is supposed to detect it and
/// re-elect (see docs/LITTER_STATE_MACHINE.md, WAYWARD).
pub mod deadline {
    use alloc::vec::Vec;
    use libakuma::net::TcpStream;

    /// One bounded I/O wait in µs — comfortably above a healthy hub's
    /// serve latency, far below WAYWARD_TIMEOUT.
    pub const IO_TIMEOUT_US: u64 = 5 * 1_000_000;
    const POLL_SLEEP_MS: u64 = 2;

    /// Invoked from inside the polling waits below, every iteration. The live
    /// agent registers a drain of its own listener here: when the ONLY thread
    /// that can serve the hub is the same thread making a hub call (a tool
    /// call inside an LLM turn, raft thread not running), the wait must serve
    /// as it waits or it deadlocks until the deadline. See
    /// docs/archive/AMD64_SPAWNED_THREAD_NEVER_RUNS.md.
    static IO_POLL_HOOK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

    pub fn set_io_poll_hook(f: fn()) {
        IO_POLL_HOOK.store(f as *const () as u64, core::sync::atomic::Ordering::Release);
    }

    fn poll_hook() {
        let f = IO_POLL_HOOK.load(core::sync::atomic::Ordering::Acquire);
        if f != 0 {
            // SAFETY: the hook is registered once at startup as a plain fn();
            // see live.rs `local_drain` for its contract.
            let f = unsafe { core::mem::transmute::<u64, fn()>(f) };
            f();
        }
    }

    fn now_us() -> u64 {
        crate::util::now_us()
    }

    /// Read exactly `buf.len()` bytes or give up at the deadline.
    pub fn read_exact(stream: &TcpStream, buf: &mut [u8], timeout_us: u64) -> bool {
        let deadline = now_us().saturating_add(timeout_us);
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = unsafe {
                libakuma::read_fd(stream.as_raw_fd(), &mut buf[filled..])
            };
            if n > 0 {
                filled += n as usize;
            } else if n == 0 {
                return false; // EOF
            } else if now_us() >= deadline {
                return false; // timeout (or hard error — same treatment)
            } else {
                poll_hook();
                libakuma::sleep_ms(POLL_SLEEP_MS);
            }
        }
        true
    }

    /// Write the whole framed payload or give up at the deadline.
    pub fn write_all(stream: &TcpStream, payload: &[u8], timeout_us: u64) -> bool {
        let deadline = now_us().saturating_add(timeout_us);
        let mut sent = 0usize;
        while sent < payload.len() {
            let n = unsafe {
                libakuma::write_fd(stream.as_raw_fd(), &payload[sent..])
            };
            if n > 0 {
                sent += n as usize;
            } else if now_us() >= deadline {
                return false;
            } else {
                poll_hook();
                libakuma::sleep_ms(POLL_SLEEP_MS);
            }
        }
        true
    }

    /// Frame a payload (4-byte big-endian length + bytes).
    pub fn frame(payload: &str) -> Vec<u8> {
        use litter_wire::{encode_len_header, MAX_FRAME_LEN};
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&encode_len_header(payload.len() as u32));
        framed.extend_from_slice(payload.as_bytes());
        framed
    }
}
