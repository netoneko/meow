//! Who is in the litter, and the message queue each of them has.
//!
//! The **peer layer**, in the Tendermint analogy (`LITTER_WORKFLOW.md`):
//! roster and mailboxes, with no opinion about what any message means. It
//! never inspects a body, never decides authority and never touches the
//! task table — all of that is the record's job (`record.rs`). What it
//! owns is delivery and the monotonic stamp that orders it.
//!
//! Pure: `now_us` is always a parameter, there is no clock and no socket
//! here, so every path below is unit-testable without a thread.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use litter_wire::Message;

/// Sanity bound on roster growth, same as `litter-hub`'s.
pub const MAX_ROSTER_SIZE: usize = 256;

/// The reserved group name: `Send { to: "litter" }` fans out to every
/// roster member instead of creating an inbox for a phantom agent.
pub const GROUP_NAME: &str = "litter";

/// Newest messages per inbox that compaction always preserves.
pub const KEEP_RECENT: usize = 16;

/// Cap on how many folded-message summaries one compaction marker
/// carries. The marker is a summary, not an archive.
pub const MARKER_SUMMARY_LINES: usize = 20;

pub struct Membership {
    roster: Vec<String>,
    inboxes: Vec<(String, Vec<Message>)>,
    /// Hub-local monotonic ts floor (see `stamp`).
    last_ts: u64,
}

impl Membership {
    pub fn new() -> Self {
        Self { roster: Vec::new(), inboxes: Vec::new(), last_ts: 0 }
    }

    pub fn roster(&self) -> &[String] {
        &self.roster
    }

    pub fn contains(&self, name: &str) -> bool {
        self.roster.iter().any(|n| n == name)
    }

    pub fn is_full(&self) -> bool {
        self.roster.len() >= MAX_ROSTER_SIZE
    }

    /// Add a member. Returns `true` if this was a new arrival (the caller
    /// turns that into a cluster event); `Join` is idempotent, which is
    /// what lets every survivor re-register itself after a leader change
    /// without coordination.
    pub fn join(&mut self, name: &str) -> bool {
        if self.contains(name) {
            return false;
        }
        self.roster.push(String::from(name));
        true
    }

    /// Hub-local monotonic timestamp. Wall-clock microseconds alone can
    /// collide when several messages arrive within one microsecond (a whole
    /// round-trip batch can), and `history`'s `ts < before` paging is lossy
    /// under collisions — so every delivered message is guaranteed a
    /// strictly greater ts than its predecessor.
    pub fn stamp(&mut self, now_us: u64) -> u64 {
        self.last_ts = if now_us <= self.last_ts { self.last_ts + 1 } else { now_us };
        self.last_ts
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

    /// Deliver one message into one inbox. No validation — callers did it.
    pub fn deliver(&mut self, to: &str, msg: Message) {
        self.inbox_mut(to).push(msg);
    }

    /// Deliver a copy to every roster member. The sender included: their
    /// own words in their own inbox are the right transcript, and
    /// `observe` dedups anyway.
    pub fn broadcast(&mut self, msg: &Message) {
        let members = self.roster.clone();
        for member in members {
            self.deliver(&member, msg.clone());
        }
    }

    pub fn inbox(&self, name: &str) -> Vec<Message> {
        for (n, msgs) in self.inboxes.iter() {
            if n == name {
                return msgs.clone();
            }
        }
        Vec::new()
    }

    /// One page of an inbox, walking backwards from `before` (0 = newest).
    /// Delivered ascending, because that is how it reads.
    pub fn history(&self, name: &str, before: u64, limit: usize) -> Vec<Message> {
        let limit = limit.max(1).min(128);
        for (n, msgs) in self.inboxes.iter() {
            if n == name {
                let mut batch: Vec<Message> = msgs
                    .iter()
                    .filter(|m| before == 0 || m.ts < before)
                    .rev()
                    .take(limit)
                    .cloned()
                    .collect();
                batch.reverse();
                return batch;
            }
        }
        Vec::new()
    }

    /// Fold inbox history into ONE marker message per inbox: everything
    /// past the newest `KEEP_RECENT` is summarized into a `"[compacted: …]"`
    /// marker placed at the head of the inbox, and the originals are
    /// dropped. Every history walk stops at the marker, so a fresh agent
    /// sources exactly the relevant tail from the protocol.
    ///
    /// `carry` is the open-work list the record wants carried across the
    /// boundary — the "carry forward active sub-tasks" half of the
    /// workflow's atomic compaction. Membership does not know what those
    /// lines mean; it only guarantees they survive the prune, which is the
    /// whole reason the table's contents can afford to be leader memory.
    ///
    /// Returns how many messages were folded (0 = nothing to do; the call
    /// is idempotent).
    pub fn compact(&mut self, carry: &[String], now_us: u64) -> usize {
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
                if idx > 0 {
                    msgs.drain(..idx);
                    for b in prior_marker_bodies.drain(..) {
                        msgs.insert(0, Message {
                            kind: litter_wire::MessageKind::Marker,
                            role: litter_wire::SenderRole::Leader,
                            ..Message::chat(String::from(GROUP_NAME), 0, b, now_us)
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

        if folded_total == 0 && prior_marker_bodies.is_empty() && carry.is_empty() {
            return 0;
        }

        let mut body = String::new();
        if folded_total > 0 {
            body.push_str(&format!(
                "[compacted: {} earlier message(s) folded. What was said, in brief:]",
                folded_total
            ));
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

        // Open work rides the marker across the boundary. It goes AFTER the
        // transcript summary on purpose: it is the part that is still
        // actionable, so it should be the last thing an agent reads.
        if !carry.is_empty() {
            body.push_str("\n[still open]");
            for line in carry {
                body.push('\n');
                body.push_str(line);
            }
        }

        let marker = Message {
            kind: litter_wire::MessageKind::Marker,
            role: litter_wire::SenderRole::Leader,
            ..Message::chat(String::from(GROUP_NAME), 0, body, now_us)
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
        folded_total
    }
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
