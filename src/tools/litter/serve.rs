//! The hub, as a state machine any meow can carry — the no_std twin of
//! `crates/litter-hub`'s `HubState`, minus `std`, `Mutex` and `HashMap`
//! (meow is single-threaded and `no_std`+`alloc`, so plain `Vec`s are the
//! whole story). This is what makes a dedicated hub *process* unnecessary:
//! the first `meow litter live` agent that can bind the hub socket becomes
//! the coordinator (see `live`), serves framed requests out of this state
//! between its own turns, and if it dies the next agent to start simply
//! wins the race instead — the roster re-seeds itself through the same
//! bootstrap `Join` every agent already sends at startup.
//!
//! Request handling is a deliberate port of `litter-hub`'s `HubState::handle`
//! (join/send/inbox/peers, name validation, roster cap) with one deliberate
//! extension: a `Send` addressed to the reserved name `litter` is a group
//! broadcast — fanned out into every roster member's inbox — so an agent (or
//! the operator via `meow litter send --to litter`) can address the whole
//! litter at once and every member's own-inbox polling wakes on it. That
//! means one message lives in several inboxes; `meow litter observe` dedups
//! on `(ts, from, round, body)` for exactly this reason.
//!
//! Like `litter-hub`, one connection carries exactly one framed
//! request/response pair (`litter-wire` framing), and inboxes are
//! non-destructive: `Inbox` peeks, nothing consumes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libakuma::net::{TcpListener, TcpStream};

use litter_wire::{
    decode_len_header, decode_request, encode_len_header, encode_response, is_valid_name,
    Message, Request, Response, MAX_FRAME_LEN,
};

/// Sanity bound on roster growth, same as `litter-hub`'s — a litter is a
/// handful of agents; this only exists so a misbehaving client can't grow
/// the roster unboundedly.
const MAX_ROSTER_SIZE: usize = 256;

/// The reserved group name: `Send { to: "litter" }` fans out to every
/// roster member instead of creating an inbox for a phantom agent.
pub const GROUP_NAME: &str = "litter";

pub struct HubState {
    roster: Vec<String>,
    inboxes: Vec<(String, Vec<Message>)>,
    /// Compacted shared knowledge (done tasks, decisions). A joining agent
    /// receives exactly this as its first inbox message and then sees only
    /// post-compaction traffic — history before the last compaction is
    /// deliberately not replayed, which keeps a fresh agent's context small
    /// instead of filling it with a stale transcript. The same text is
    /// persisted to `snapshot.log` (see `live`), which is what a leadership
    /// takeover loads back — the snapshot, not the inboxes, is the state
    /// that survives a leader dying.
    pub snapshot: String,
}

/// How many newest messages per inbox compaction always preserves — the
/// live tail of the debate. Anything older is folded into `snapshot` and
/// dropped from the inboxes.
const KEEP_RECENT: usize = 16;

impl HubState {
    /// A hub that knows nobody yet — exactly the "empty seed roster" shape
    /// `litter-hub --roster` made optional; the litter bootstraps itself.
    pub fn new() -> Self {
        Self { roster: Vec::new(), inboxes: Vec::new(), snapshot: String::new() }
    }

    /// Seed the snapshot at startup — a leadership takeover loads the
    /// previous leader's `snapshot.log` here so continuity survives the
    /// power cycle.
    pub fn set_snapshot(&mut self, text: String) {
        self.snapshot = text;
    }

    /// Current roster (the coordinator persists it so a takeover can
    /// restore membership — see `live`).
    pub fn roster(&self) -> Vec<String> {
        self.roster.clone()
    }

    /// Re-add a member WITHOUT the joining snapshot welcome — takeover-time
    /// roster restoration, for members that were already in the litter.
    pub fn restore_member(&mut self, name: &str) {
        if is_valid_name(name) && !self.roster.iter().any(|n| n == name) && self.roster.len() < MAX_ROSTER_SIZE {
            self.roster.push(String::from(name));
        }
    }

    fn now_us() -> u64 {
        crate::util::now_us()
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

    /// Handle one already-decoded request and produce the response. Pure
    /// state transition, no I/O — the half the test suite drives directly.
    pub fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::Join { name } => {
                if !is_valid_name(&name) {
                    return Response::Error { message: String::from("'name' must be a plain agent name") };
                }
                let is_new = !self.roster.iter().any(|n| n == &name);
                if is_new {
                    if self.roster.len() >= MAX_ROSTER_SIZE {
                        return Response::Error { message: String::from("roster is full") };
                    }
                    self.roster.push(name.clone());
                    // A new member skips all history before the last
                    // compaction: its first inbox message IS the snapshot.
                    let welcome = Message {
                        from: String::from(GROUP_NAME),
                        round: 0,
                        body: format!("[litter snapshot — history before this was compacted]\n{}[end snapshot]", self.snapshot),
                        ts: Self::now_us(),
                    };
                    self.inbox_mut(&name).push(welcome);
                }
                Response::Joined
            }
            Request::Send { from, to, body, round } => {
                if !is_valid_name(&from) {
                    return Response::Error { message: String::from("'from' must be a plain agent name") };
                }
                if !is_valid_name(&to) {
                    return Response::Error { message: String::from("'to' must be a plain agent name") };
                }
                if body.is_empty() {
                    return Response::Error { message: String::from("send requires a non-empty body") };
                }
                let msg = Message { from, round, body, ts: Self::now_us() };
                let bytes = msg.body.len();
                if to == GROUP_NAME {
                    // Group broadcast: every roster member gets a copy (the
                    // sender included — they'll see their own words in their
                    // inbox next turn, which is the right transcript for a
                    // debate, and `observe` dedups the copies anyway).
                    for member in self.roster.clone() {
                        self.inbox_mut(&member).push(Message {
                            from: msg.from.clone(),
                            round: msg.round,
                            body: msg.body.clone(),
                            ts: msg.ts,
                        });
                    }
                } else {
                    self.inbox_mut(&to).push(msg);
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
            Request::Peers => Response::Peers { names: self.roster.clone() },
        }
    }

    /// Fold inbox history into the snapshot and trim each inbox to its
    /// newest `KEEP_RECENT` messages. Called periodically by the
    /// coordinator's tick loop (`live`), never mid-turn, and returns how
    /// many messages were folded — 0 means "nothing to do". Pure state
    /// transition, fully testable: the chat-side effect is simply that
    /// every agent's next ReadInbox shows the snapshot-anchored tail
    /// instead of an ever-growing transcript.
    pub fn compact(&mut self) -> usize {
        let mut folded: Vec<String> = Vec::new();
        for (_, msgs) in self.inboxes.iter_mut() {
            if msgs.len() <= KEEP_RECENT {
                continue;
            }
            let cut = msgs.len() - KEEP_RECENT;
            for m in msgs.drain(..cut) {
                folded.push(format!("[r{}] {}: {}", m.round, m.from, summarize(&m.body)));
            }
        }
        if folded.is_empty() {
            return 0;
        }
        self.snapshot.push_str(&format!("[compacted {} message(s)]\n", folded.len()));
        for line in folded {
            self.snapshot.push_str(&line);
            self.snapshot.push('\n');
        }
        1
    }

    /// Serve exactly one framed request/response pair over an already
    /// accepted connection, then return — the same one-shot shape every
    /// meow-side client (`hub::call`) already speaks. Returns `false` on a
    /// transport error (peer hung up mid-frame); protocol-level problems
    /// still get a well-formed `Response::Error` back, so a misbehaving
    /// client can tell them apart from a network failure.
    pub fn serve_one(&mut self, stream: &mut TcpStream) -> bool {
        let mut header = [0u8; 4];
        if stream.read_exact(&mut header).is_err() {
            return false;
        }
        let len = decode_len_header(header);
        if len > MAX_FRAME_LEN {
            return write_frame(stream, &encode_response(&Response::Error {
                message: String::from("request frame too large"),
            }));
        }

        let mut body = alloc::vec![0u8; len as usize];
        if stream.read_exact(&mut body).is_err() {
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

fn write_frame(stream: &mut TcpStream, payload: &str) -> bool {
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&encode_len_header(payload.len() as u32));
    framed.extend_from_slice(payload.as_bytes());
    stream.write_all(&framed).is_ok()
}

/// One-line stand-in for a message in the snapshot: truncate to ~120 chars
/// on a char boundary. A dumb summary, deliberately — the snapshot's job is
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

/// Accept-and-serve every connection already waiting on `listener`'s backlog,
/// then return how many were served. Non-blocking by design
/// (`TcpListener::try_accept`): the leader calls this between inbox polls and
/// around turns, so hub duty costs nothing when the litter is quiet — and
/// while an agent is mid-turn (an LLM call can take minutes), clients simply
/// queue in the kernel's listen backlog and get served on the next drain.
pub fn drain(listener: &TcpListener, state: &mut HubState) -> usize {
    let mut served = 0usize;
    while let Ok((mut stream, _peer)) = listener.try_accept() {
        state.serve_one(&mut stream);
        served += 1;
    }
    served
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter serve tests ---\n");

    // join → peers round-trip, and join is idempotent
    total += 1;
    {
        let mut hub = HubState::new();
        let j1 = hub.handle(Request::Join { name: String::from("sherlock") });
        let j2 = hub.handle(Request::Join { name: String::from("sherlock") });
        let peers = hub.handle(Request::Peers);
        let joined = matches!(j1, Response::Joined) && matches!(j2, Response::Joined);
        let listed = matches!(&peers, Response::Peers { names } if names.len() == 1 && names[0] == "sherlock");
        if joined && listed { passed += 1; }
        else { libakuma::print(&format!("  [!] join/peers: joined={:?} listed={:?}\n", j1, peers)); }
    }

    // join rejects invalid names and rejects a roster overflowing the cap
    total += 1;
    {
        let mut hub = HubState::new();
        let bad = hub.handle(Request::Join { name: String::from("../etc") });
        let mut full = HubState::new();
        let mut overflow = false;
        for i in 0..MAX_ROSTER_SIZE + 1 {
            let r = full.handle(Request::Join { name: format!("agent-{}", i) });
            if let Response::Error { .. } = r {
                overflow = i == MAX_ROSTER_SIZE;
                break;
            }
        }
        let bad_rejected = matches!(bad, Response::Error { .. });
        if bad_rejected && overflow { passed += 1; }
        else { libakuma::print(&format!("  [!] cap/invalid: bad={:?} overflow={}\n", bad, overflow)); }
    }

    // send → inbox; empty inbox for an unknown name is not an error
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("hercules") });
        hub.handle(Request::Join { name: String::from("sherlock") });
        let sent = hub.handle(Request::Send {
            from: String::from("sherlock"),
            to: String::from("hercules"),
            body: String::from("the game is afoot"),
            round: 1,
        });
        let inbox = hub.handle(Request::Inbox { name: String::from("hercules") });
        let ghost = hub.handle(Request::Inbox { name: String::from("ressler") });
        let ok_sent = matches!(sent, Response::Sent { bytes } if bytes == 17);
        let ok_inbox = matches!(&inbox, Response::Inbox { messages }
            if messages.len() == 1 && messages[0].from == "sherlock" && messages[0].round == 1);
        let ok_ghost = matches!(&ghost, Response::Inbox { messages } if messages.is_empty());
        if ok_sent && ok_inbox && ok_ghost { passed += 1; }
        else { libakuma::print(&format!("  [!] send/inbox: sent={:?} inbox={:?} ghost={:?}\n", sent, inbox, ghost)); }
    }

    // send rejects invalid from/to and empty bodies
    total += 1;
    {
        let mut hub = HubState::new();
        let bad_from = hub.handle(Request::Send { from: String::from("a b"), to: String::from("x"), body: String::from("hi"), round: 0 });
        let bad_to = hub.handle(Request::Send { from: String::from("x"), to: String::new(), body: String::from("hi"), round: 0 });
        let empty = hub.handle(Request::Send { from: String::from("x"), to: String::from("y"), body: String::new(), round: 0 });
        let all_err = [bad_from, bad_to, empty].iter().all(|r| matches!(r, Response::Error { .. }));
        if all_err { passed += 1; }
        else { libakuma::print("  [!] send validation: some invalid send was accepted\n"); }
    }

    // group broadcast: Send to the reserved `litter` name fans out to every
    // roster member, and the sender gets their own copy back too
    total += 1;
    {
        let mut hub = HubState::new();
        for n in ["sherlock", "hercules", "zenigata"] {
            hub.handle(Request::Join { name: String::from(n) });
        }
        hub.handle(Request::Send { from: String::from("sherlock"), to: String::from(GROUP_NAME), body: String::from("attention all"), round: 2 });
        let every_member_has_it = ["sherlock", "hercules", "zenigata"].iter().all(|n| {
            matches!(hub.handle(Request::Inbox { name: String::from(*n) }),
                Response::Inbox { messages } if messages.len() == 1 && messages[0].body == "attention all")
        });
        if every_member_has_it { passed += 1; }
        else { libakuma::print("  [!] group broadcast: some member did not receive the fan-out\n"); }
    }

    // compaction: history beyond the tail folds into the snapshot and a
    // joining member receives the snapshot as its first inbox message
    total += 1;
    {
        let mut hub = HubState::new();
        hub.handle(Request::Join { name: String::from("sherlock") });
        for i in 0..KEEP_RECENT + 5 {
            hub.handle(Request::Send { from: String::from("sherlock"), to: String::from("sherlock"), body: format!("msg {}", i), round: i as i64 });
        }
        let folded = hub.compact();
        let kept = matches!(hub.handle(Request::Inbox { name: String::from("sherlock") }),
            Response::Inbox { messages } if messages.len() == KEEP_RECENT);
        let empty = hub.compact() == 0; // idempotent when nothing to fold
        if folded == 1 && kept && empty && hub.snapshot.contains("[r0] sherlock: msg 0") && !hub.snapshot.contains("msg 18") {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] compact: folded={} kept={} empty2={} snap={}\n", folded, kept, empty, hub.snapshot));
        }
    }

    // a joining agent's first inbox message is the snapshot, nothing older
    total += 1;
    {
        let mut hub = HubState::new();
        hub.set_snapshot(String::from("we decided: use dual-bank boot\n"));
        hub.handle(Request::Send { from: String::from("sherlock"), to: String::from(GROUP_NAME), body: String::from("old debate"), round: 1 });
        hub.handle(Request::Join { name: String::from("ressler") });
        let inbox = hub.handle(Request::Inbox { name: String::from("ressler") });
        let ok = matches!(&inbox, Response::Inbox { messages }
            if messages.len() == 1 && messages[0].body.contains("dual-bank boot") && !messages[0].body.contains("old debate"));
        if ok { passed += 1; }
        else { libakuma::print(&format!("  [!] join snapshot: {:?}\n", inbox)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
