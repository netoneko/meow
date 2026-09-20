//! The hub, as a state machine any meow can carry — the no_std twin of
//! `crates/litter-hub`'s `HubState`, extended to the full coordinator role
//! (see `docs/LITTER_STATE_MACHINE.md`, `docs/LITTER_RAFT_LOOP.md`):
//! roster + inboxes, the cluster event log, term/leader status, the task
//! table (`tasks`), and marker-based history compaction.
//!
//! Threading: when this process wins the bind race, one `PMutex<HubState>`
//! is shared between the raft/serve thread (drain + ticks) and the agent
//! loop's direct reads. Every method takes `&mut self`, so the mutex is
//! the only synchronization needed; critical sections are a handful of Vec
//! operations.
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

use super::tasks::{TableEvent, TaskTable};

/// Sanity bound on roster growth, same as `litter-hub`'s.
const MAX_ROSTER_SIZE: usize = 256;

/// The reserved group name: `Send { to: "litter" }` fans out to every
/// roster member instead of creating an inbox for a phantom agent.
pub const GROUP_NAME: &str = "litter";

/// Newest messages per inbox that compaction always preserves.
pub const KEEP_RECENT: usize = 16;

/// Event-log cap. The epoch keeps rising past drops, so a stale cursor
/// simply gets "everything we still have" — never a silent gap presented
/// as fresh news.
const EVENT_LOG_CAP: usize = 512;

/// Cap on the relay log: oldest entries drop when a peer stays unreachable
/// past this many outbound messages (relay is best-effort, never unbounded).
const RELAY_LOG_CAP: usize = 256;

/// One outbound message recorded for the relay plane: `to` as the sender
/// addressed it (GROUP_NAME for broadcasts), the stamped message, and the
/// sender's own envelope — origin litter, origin ts and origin signature,
/// exactly as they arrived. The hub is a courier here, not a signer: it
/// never mints a signature over words it did not say.
/// The raft thread drains this log to peer litters (see `live::relay_tick`).
pub struct RelayEntry {
    pub to: String,
    pub msg: Message,
    /// The litter the sender belongs to (envelope `ol`) — ours, since only
    /// locally-originated traffic is ever captured here.
    pub origin_litter: String,
    /// The SENDER's timestamp, which is what its signature commits to.
    /// Distinct from `msg.ts`, the hub's own monotonic stamp: that one
    /// orders our history and drives the relay cursor, this one goes on
    /// the wire and must be reproduced byte-for-byte to verify `sig`.
    pub origin_ts: u64,
    /// The sender's signature, made in the sender's process with the
    /// sender's key. Relayed verbatim; never re-signed.
    pub origin_sig: String,
}

/// Cap on how many folded-message summaries one compaction marker
/// carries. The marker is a summary, not an archive.
const MARKER_SUMMARY_LINES: usize = 20;

/// Seen-set entry: dedup metadata only — origin litter, origin **agent**
/// and origin ts, plus the agent that relayed it here.
///
/// The identity is the (litter, agent, ts) triple rather than (litter, ts):
/// now that the sender stamps `ot` from its own clock, two agents in one
/// litter can hand out the same microsecond, and keying on the pair alone
/// would silently drop the second one as a duplicate. `rl` is kept for
/// provenance ("who carried it"), not for dedup — an echo of our own
/// words is caught earlier, by `ol` matching our litter.
///
/// No bodies, no signatures — the history already has the transcript.
pub struct Seen {
    pub ol: String,
    pub from: String,
    pub ot: u64,
    pub rl: String,
}

/// Seen-set cap backstop: entries are age-pruned every relayed-in accept
/// (past the cutoff "seen" is forgotten on purpose — the age guard then
/// discards the replayed message anyway), but a hostile flood must not
/// grow the table unbounded between prunes either.
const SEEN_CAP: usize = 512;

pub struct HubState {
    roster: Vec<String>,
    inboxes: Vec<(String, Vec<Message>)>,
    /// Raft term. Bumped every time a new binder takes over — monotonic
    /// across leadership changes so agents can order leaderships they see.
    pub term: u64,
    /// Hub-local monotonic ts floor (see stamp()).
    last_ts: u64,
    /// Whoever currently owns the hub socket (set by the winner of the
    /// bind race at startup; it can only change by that process dying).
    pub leader: Option<String>,
    /// Cluster event log: (epoch, text), epoch strictly increasing.
    events: Vec<(u64, String)>,
    pub tasks: TaskTable,
    /// Outbound chat traffic, oldest first, for the cross-litter relay.
    /// Captured in `handle` (pure state), drained by the raft thread's
    /// relay tick — never by `handle` itself, which does no I/O.
    pub relay_log: Vec<RelayEntry>,
    /// Relay metadata for traffic already delivered from peer litters —
    /// the dedup table ("delivered once is the contract"), pruned by age.
    pub seen: Vec<Seen>,
}

impl HubState {
    /// A hub that knows nobody yet — the "empty seed roster" shape; the
    /// litter bootstraps itself.
    pub fn new() -> Self {
        Self {
            roster: Vec::new(),
            inboxes: Vec::new(),
            term: 1,
            last_ts: 0,
            leader: None,
            events: Vec::new(),
            tasks: TaskTable::new(),
            relay_log: Vec::new(),
            seen: Vec::new(),
        }
    }

    /// The bind-race winner announces itself. `new_term` is `previous + 1`
    /// on a takeover, `1` for a fresh litter; either way the announcement
    /// is the first event every pulse will carry.
    pub fn set_leader(&mut self, name: &str, new_term: u64) {
        self.term = new_term;
        self.leader = Some(String::from(name));
        self.event(format!("[event] {} is leader (term {})", name, new_term));
    }

    fn now_us() -> u64 {
        crate::util::now_us()
    }

    /// Append one cluster event and bump the epoch. Every status change
    /// (elections, joins, leaves, task churn) flows through here and from
    /// here to every agent's next pulse.
    pub fn event(&mut self, text: String) {
        let epoch = self.next_epoch();
        self.events.push((epoch, text));
        if self.events.len() > EVENT_LOG_CAP {
            self.events.remove(0);
        }
    }

    fn next_epoch(&self) -> u64 {
        self.events.last().map(|(e, _)| *e + 1).unwrap_or(1)
    }

    fn inbox_mut(&mut self, name: &str) -> &mut Vec<Message> {
        for (n, msgs) in self.inboxes.iter_mut() {
            if n == name {
                return msgs;
            }
        }
        self.inboxes.push((String::from(name), Vec::new()));
        match self.inboxes.last_mut() {
            Some((_, msgs)) => msgs,
            // Unreachable: we just pushed an entry.
            None => unreachable!(),
        }
    }

    /// Deliver one message into one inbox (no validation — callers did it).
    fn deliver(&mut self, to: &str, msg: Message) {
        self.inbox_mut(to).push(msg);
    }

    /// Hub-local monotonic timestamp. `util::now_us` alone can collide when
    /// several messages arrive within one microsecond (a whole round-trip
    /// batch can), and `History`'s `ts < before` paging is lossy under
    /// collisions — so the hub guarantees every delivered message carries a
    /// strictly greater ts than its predecessor.
    fn stamp(&mut self) -> u64 {
        let now = crate::util::now_us();
        self.last_ts = if now <= self.last_ts { self.last_ts + 1 } else { now };
        self.last_ts
    }

    /// Drop seen-table entries past the relay age cutoff: past it the age
    /// guard discards any replay anyway, so remembering the (ol, ot) pair
    /// buys nothing. Called on every relayed-in accept, so the table is
    /// steady-state small.
    fn prune_seen(&mut self) {
        let cutoff = crate::util::now_us().saturating_sub(super::live::RELAY_MAX_AGE_US);
        self.seen.retain(|s| s.ot >= cutoff);
        while self.seen.len() > SEEN_CAP {
            self.seen.remove(0);
        }
    }

    /// Handle one already-decoded request and produce the response. Pure
    /// state transition, no I/O — the half the test suite drives directly.
    pub fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::Join { name } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                if !self.roster.iter().any(|n| n == &name) {
                    if self.roster.len() >= MAX_ROSTER_SIZE {
                        return Response::Error { message: String::from("roster is full") };
                    }
                    self.roster.push(name.clone());
                    self.event(format!("[event] {} joined the litter", name));
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
                // agent brought it. Dedup is metadata-only — (origin
                // litter, origin agent, origin ts) identifies the message,
                // so a repeat is dropped without touching any inbox, and
                // `ol == our litter` means our own words bounced back. The
                // table is pruned by the relay age cutoff, so "already
                // seen" only ever means "recently seen".
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
                    self.prune_seen();
                    let duplicate = self.seen.iter().any(|s| s.ol == *ol && s.ot == *ot && s.from == from);
                    if !duplicate {
                        self.seen.push(Seen { ol: ol.clone(), from: from.clone(), ot: *ot, rl: rl.clone() });
                        // The sender is the sender: `from` stays the bare
                        // origin agent name — provenance rides the envelope
                        // (ol/rl), it never rewrites who said it.
                        let ts = self.stamp();
                        if to == GROUP_NAME {
                            let members = self.roster.clone();
                            for member in members {
                                self.deliver(&member, Message::chat(from.clone(), round, body.clone(), ts));
                            }
                        } else {
                            self.deliver(&to, Message::chat(from.clone(), round, body.clone(), ts));
                        }
                    }
                    return Response::Sent { bytes };
                }

                // Sender role is hub-authoritative, stamped at delivery:
                // the leader's own messages are leader-traffic, the fixed
                // `root` identity is the operator, everyone else is a peer.
                let role = if self.leader.as_deref() == Some(from.as_str()) {
                    litter_wire::SenderRole::Leader
                } else if from == "root" {
                    litter_wire::SenderRole::Root
                } else {
                    litter_wire::SenderRole::Peer
                };
                let msg = Message { role, ..Message::chat(from, round, body, self.stamp()) };

                // Relay-plane capture: every locally-originated outbound
                // chat is recorded (with its original `to` and its origin
                // signature, signed here exactly once) for the raft thread
                // to forward. Envelope-carrying traffic never reaches this
                // branch (returned above), so no hub is ever a transit
                // node: relay is exactly one hop, by construction. Task/
                // system traffic (assignments, markers, done lines) stays
                // litter-local.
                if msg.kind == litter_wire::MessageKind::Chat {
                    // The sender's own signature is what gets relayed — the
                    // hub carries it, it does not mint one. An unsigned
                    // message (no litter identity configured at the sender,
                    // or a client from before the envelope) is simply not
                    // relayable: it stays litter-local rather than going out
                    // over someone else's name.
                    if let (Some(ol), Some(ot), Some(sig)) = (&ol, &ot, &sig) {
                        self.relay_log.push(RelayEntry {
                            to: to.clone(),
                            msg: msg.clone(),
                            origin_litter: ol.clone(),
                            origin_ts: *ot,
                            origin_sig: sig.clone(),
                        });
                        if self.relay_log.len() > RELAY_LOG_CAP {
                            self.relay_log.remove(0);
                        }
                    }
                }

                // Task-table hooks: [task] opens, [done: tN] closes. The
                // table's outputs are events (and, at tick time,
                // assignment messages) — the chat copy still lands where it
                // was addressed, so the debate keeps its transcript.
                // Only the operator (root) or the leader may OPEN tracked
                // tasks — otherwise every peer chat that starts with
                // "[task]" mints work for the whole litter (observed live:
                // an agent tasking the litter to police the kernel). Any
                // holder may still CLOSE their task with [done: tN].
                let can_open = matches!(msg.role, litter_wire::SenderRole::Root | litter_wire::SenderRole::Leader);
                match self.tasks.note_message(&msg.from, &msg.body, msg.ts, can_open) {
                    TableEvent::Done(event_text, summary) => {
                        self.event(event_text);
                        // Completion knowledge belongs in history: a short
                        // system line under the group name.
                        let done_line = Message {
                            kind: litter_wire::MessageKind::Done,
                            role: litter_wire::SenderRole::Leader,
                            ..Message::chat(String::from(GROUP_NAME), msg.round, format!("[done] {}", summary), self.stamp())
                        };
                        let members = self.roster.clone();
                        for member in members {
                            self.deliver(&member, Message { ..done_line.clone() });
                        }
                    }
                    TableEvent::Noted => {}
                    TableEvent::Requeued => {}
                }

                if to == GROUP_NAME {
                    // Group broadcast: every roster member gets a copy (the
                    // sender included — their own words in their inbox are
                    // the right transcript; `observe` dedups anyway).
                    let members = self.roster.clone();
                    for member in members {
                        self.deliver(&member, Message::chat(msg.from.clone(), msg.round, msg.body.clone(), msg.ts));
                    }
                } else {
                    let to = to;
                    self.deliver(&to, msg);
                }
                Response::Sent { bytes }
            }
            Request::Inbox { name } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                for (n, msgs) in self.inboxes.iter() {
                    if n == &name {
                        return Response::Inbox { messages: msgs.clone() };
                    }
                }
                Response::Inbox { messages: Vec::new() }
            }
            Request::History { name, before, limit } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                let limit = (limit as usize).max(1).min(128);
                let mut batch: Vec<Message> = Vec::new();
                for (n, msgs) in self.inboxes.iter() {
                    if n == &name {
                        batch = msgs
                            .iter()
                            .filter(|m| before == 0 || m.ts < before)
                            .rev() // newest first
                            .take(limit)
                            .cloned()
                            .collect();
                        batch.reverse(); // deliver ascending: reads naturally
                        break;
                    }
                }
                Response::Inbox { messages: batch }
            }
            Request::Peers { since } => {
                let events = self
                    .events
                    .iter()
                    .filter(|(e, _)| *e > since)
                    .map(|(_, text)| text.clone())
                    .collect();
                Response::Peers {
                    names: self.roster.clone(),
                    term: self.term,
                    leader: self.leader.clone(),
                    epoch: self.events.last().map(|(e, _)| *e).unwrap_or(0),
                    events,
                }
            }
        }
    }

    /// One coordinator tick over the task table: requeue expired leases,
    /// assign unassigned tasks round-robin, deliver the resulting
    /// assignment messages, and log the events. Cheap; called every few
    /// ticks by the raft thread.
    pub fn task_tick(&mut self) {
        let roster = self.roster.clone();
        let now = Self::now_us();
        let (messages, events) = self.tasks.tick(&roster, now);
        for (to, body) in messages {
            let members_ok = self.roster.iter().any(|n| n == &to);
            if members_ok {
                let ts = self.stamp();
self.deliver(&to, Message { kind: litter_wire::MessageKind::Assignment, role: litter_wire::SenderRole::Leader, ..Message::chat(String::from(GROUP_NAME), 0, body, ts) });
            }
        }
        for e in events {
            self.event(e);
        }
    }

    /// Fold inbox history into ONE marker message per inbox: everything
    /// past the newest `KEEP_RECENT` is summarized into a
    /// `"[compacted: …]"` marker placed at the head of the inbox, and the
    /// originals are dropped. Every `History` walk stops at the marker, so
    /// a fresh agent sources exactly the relevant tail from the protocol —
    /// never more than the marker plus the live tail. Returns how many
    /// messages were folded (0 = nothing to do; the call is idempotent).
    pub fn compact(&mut self) -> usize {
        let mut folded_total = 0usize;
        let mut lines: Vec<String> = Vec::new();
        let mut prior_marker_bodies: Vec<String> = Vec::new();
        for (_, msgs) in self.inboxes.iter_mut() {
            // Existing markers are bookkeeping, not history: strip them and
            // fold their text into the fresh marker, so repeated compaction
            // doesn't grow the inbox by one marker per pass.
            let mut idx = 0;
            while idx < msgs.len() && msgs[idx].kind == litter_wire::MessageKind::Marker {
                prior_marker_bodies.push(msgs[idx].body.clone());
                idx += 1;
            }
            let tail_len = msgs.len() - idx;
            if tail_len <= KEEP_RECENT {
                // Keep any markers seen so far even if nothing else folded.
                if idx > 0 {
                    msgs.drain(..idx);
                    for b in prior_marker_bodies.drain(..) {
                        msgs.insert(0, Message {
                            kind: litter_wire::MessageKind::Marker,
                            role: litter_wire::SenderRole::Leader,
                            ..Message::chat(String::from(GROUP_NAME), 0, b, Self::now_us())
                        });
                    }
                }
                continue;
            }
            let cut = tail_len - KEEP_RECENT;
            let folded: Vec<Message> = msgs.drain(idx..idx + cut).collect();
            folded_total += folded.len();
            for m in &folded {
                if lines.len() < MARKER_SUMMARY_LINES {
                    lines.push(format!("[r{}] {}: {}", m.round, m.from, summarize(&m.body)));
                }
            }
        }
        if folded_total == 0 && prior_marker_bodies.is_empty() {
            return 0;
        }

        let mut body = String::new();
        if folded_total > 0 {
            body.push_str(&format!("[compacted: {} earlier message(s) folded. What was said, in brief:]", folded_total));
            for line in lines {
                body.push('\n');
                body.push_str(&line);
            }
            if folded_total > MARKER_SUMMARY_LINES {
                body.push_str(&format!("\n[+ {} more, not summarized]", folded_total - MARKER_SUMMARY_LINES));
            }
        } else {
            // Nothing new folded — carry the most recent old marker forward.
            body = prior_marker_bodies.last().cloned().unwrap_or_else(|| String::from("[compacted]"));
        }

        let marker = Message {
            kind: litter_wire::MessageKind::Marker,
            role: litter_wire::SenderRole::Leader,
            ..Message::chat(String::from(GROUP_NAME), 0, body, Self::now_us())
        };
        for (_, msgs) in self.inboxes.iter_mut() {
            // replace any surviving old markers with the single fresh one
            let idx = {
                let mut i = 0;
                while i < msgs.len() && msgs[i].kind == litter_wire::MessageKind::Marker {
                    i += 1;
                }
                i
            };
            msgs.drain(..idx);
            msgs.insert(0, marker.clone());
        }
        if folded_total > 0 {
            self.event(format!("[event] history compacted ({} message(s) folded)", folded_total));
        }
        folded_total
    }

    /// Serve exactly one framed request/response pair over an already
    /// accepted connection. Returns `false` on a transport error; protocol
    /// problems still get a well-formed `Response::Error` back.
    pub fn serve_one(&mut self, stream: &mut TcpStream) -> bool {
        // The stream was set nonblocking by drain(); every read/write here
        // is deadline-bounded so a half-open client can't park the raft
        // thread (which would hold the state lock and freeze the hub).
        let mut header = [0u8; 4];
        if !deadline::read_exact(stream, &mut header, deadline::IO_TIMEOUT_US) {
            return false;
        }
        let len = decode_len_header(header);
        if len > MAX_FRAME_LEN {
            return write_frame(stream, &encode_response(&Response::Error {
                message: String::from("request frame too large"),
            }));
        }

        let mut body = alloc::vec![0u8; len as usize];
        if !deadline::read_exact(stream, &mut body, deadline::IO_TIMEOUT_US) {
            return false;
        }

        let response = match core::str::from_utf8(&body) {
            Ok(text) => match decode_request(text) {
                Ok(req) => self.handle(req),
                Err(litter_wire::WireError::UnsupportedVersion(v)) => Response::Error {
                    message: format!(
                        "unsupported protocol version {} (hub speaks {})",
                        v,
                        litter_wire::PROTOCOL_VERSION
                    ),
                },
                Err(litter_wire::WireError::Parse(reason)) => {
                    Response::Error { message: format!("malformed request: {}", reason) }
                }
            },
            Err(_) => Response::Error { message: String::from("request is not valid UTF-8") },
        };

        write_frame(stream, &encode_response(&response))
    }

}

/// Accept-and-serve every connection already waiting in the backlog.
/// Non-blocking (`TcpListener::try_accept`): BOTH threads call this on
/// every tick — the raft thread and the agent loop — so requests are
/// served by whichever tick lands first. Returns how many frames were
/// served (0 = quiet litter, no cost).
pub fn drain(listener: &TcpListener, state: &mut HubState) -> usize {
    let mut served = 0usize;
    while let Ok((mut stream, _peer)) = listener.try_accept() {
        // Deadline-bounded serving: a client that never finishes its
        // request must not hold the raft thread (and the state lock)
        // forever. It costs the client a retry — one request per
        // connection, so a retry is a fresh connection.
        let _ = libakuma::set_nonblocking(stream.as_raw_fd(), true);
        state.serve_one(&mut stream);
        served += 1;
    }
    served
}

fn write_frame(stream: &mut TcpStream, payload: &str) -> bool {
    deadline::write_all(stream, &deadline::frame(payload), deadline::IO_TIMEOUT_US)
}

/// One-line stand-in for a folded message: truncate to ~120 chars on a
/// char boundary. A dumb summary, deliberately — the marker's job is
/// keeping payloads and context windows small, not editorializing.
fn summarize(body: &str) -> String {
    let single = body.replace('\n', " ");
    if single.len() <= 120 {
        return single;
    }
    let mut cut = 120;
    while !single.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut s = String::from(&single[..cut]);
    s.push('…');
    s
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
        let ok = hub.relay_log.len() == 2
            && hub.relay_log[0].to == GROUP_NAME
            && hub.relay_log[1].to == "tiger"
            && hub.relay_log[0].msg.ts < hub.relay_log[1].msg.ts
            && hub.relay_log.iter().all(|e| {
                e.msg.kind == litter_wire::MessageKind::Chat
                    && e.origin_litter == "yard"
                    && e.origin_ts > 0
                    && !e.origin_sig.is_empty()
            });
        if ok { passed += 1; }
        else { libakuma::print(&format!("  [!] relay log: {:?}\n", hub.relay_log.iter().map(|e| (e.to.clone(), e.msg.ts)).collect::<Vec<_>>())); }
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
            && hub.tasks.is_empty();
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
