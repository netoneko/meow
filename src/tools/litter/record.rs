//! The litter's **record**: the ordered event log every agent reads, the
//! term/leader bookkeeping that orders leaderships, and the application
//! state machine that the log drives (`tasks::TaskTable`).
//!
//! In the Tendermint analogy (`LITTER_WORKFLOW.md`) this is the consensus
//! log plus the ABCI application, with `membership.rs` as the peer layer.
//! The division of labour is the point: membership moves bytes to
//! mailboxes and has no opinion about them; the record decides what a
//! message *means*, who was allowed to say it, and what state changes
//! follow. Nothing here does I/O, so "apply" is deterministic — the same
//! record applied to the same state yields the same result on any agent,
//! which is what makes the log worth replicating at all.
//!
//! `now_us` is a parameter everywhere, for the same reason it is in
//! `tasks.rs`: a state machine with a clock in it cannot be tested.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::tasks::TaskTable;

/// Event-log cap. The epoch keeps rising past drops, so a stale cursor
/// simply gets "everything we still have" — never a silent gap presented
/// as fresh news.
const EVENT_LOG_CAP: usize = 512;

pub struct Record {
    /// Raft term. Bumped every time a new binder takes over — monotonic
    /// across leadership changes so agents can order leaderships they see.
    pub term: u64,
    /// Whoever currently owns the hub socket (set by the winner of the
    /// bind race at startup; it can only change by that process dying).
    pub leader: Option<String>,
    /// Cluster event log: (epoch, text), epoch strictly increasing.
    events: Vec<(u64, String)>,
    /// The application state machine the log drives.
    pub tasks: TaskTable,
}

impl Record {
    pub fn new() -> Self {
        Self { term: 1, leader: None, events: Vec::new(), tasks: TaskTable::new() }
    }

    /// The bind-race winner announces itself. `new_term` is `previous + 1`
    /// on a takeover, `1` for a fresh litter; either way the announcement
    /// is the first event every pulse will carry.
    pub fn set_leader(&mut self, name: &str, new_term: u64) {
        self.term = new_term;
        self.leader = Some(String::from(name));
        self.event(format!("[event] {} is leader (term {})", name, new_term));
    }

    pub fn is_leader(&self, name: &str) -> bool {
        self.leader.as_deref() == Some(name)
    }

    /// Append one entry and bump the epoch. Every status change
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

    /// The current height, in the log's own terms.
    pub fn epoch(&self) -> u64 {
        self.events.last().map(|(e, _)| *e).unwrap_or(0)
    }

    /// Everything newer than the caller's cursor — the change feed every
    /// `Peers` pulse carries.
    pub fn events_since(&self, since: u64) -> Vec<String> {
        self.events
            .iter()
            .filter(|(e, _)| *e > since)
            .map(|(_, text)| text.clone())
            .collect()
    }
}
