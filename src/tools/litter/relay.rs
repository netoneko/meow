//! The **cross-litter relay plane**: what this hub owes other litters, and
//! what it has already accepted from them.
//!
//! Split out of `HubState` because it is a different concern from both the
//! peer layer (`membership.rs`) and the record (`record.rs`): it is neither
//! local delivery nor local consensus, but a best-effort courier between
//! two litters that each have their own. The hub is a courier here and not
//! a signer — it relays the sender's own signature verbatim and never mints
//! one over words it did not say.
//!
//! See `docs/LITTER_RELAY_TOPOLOGY.md`. Relay is exactly one hop by
//! construction: envelope-carrying traffic never reaches capture, so no hub
//! is ever a transit node.

use alloc::string::String;
use alloc::vec::Vec;

use litter_wire::Message;

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

/// Everything this hub owes other litters, and everything it has already
/// taken from them.
pub struct RelayPlane {
    /// Outbound chat traffic, oldest first. Captured while handling a
    /// request (pure state), drained by the raft thread's relay tick —
    /// never by the handler itself, which does no I/O.
    pub log: Vec<RelayEntry>,
    /// Relay metadata for traffic already delivered from peer litters —
    /// the dedup table ("delivered once is the contract"), pruned by age.
    pub seen: Vec<Seen>,
}

impl RelayPlane {
    pub fn new() -> Self {
        Self { log: Vec::new(), seen: Vec::new() }
    }

    /// Record one locally-originated message for forwarding, oldest first,
    /// dropping the oldest past the cap: relay is best-effort, never
    /// unbounded.
    pub fn capture(&mut self, entry: RelayEntry) {
        self.log.push(entry);
        if self.log.len() > RELAY_LOG_CAP {
            self.log.remove(0);
        }
    }

    /// Has this exact origin message already been delivered here?
    ///
    /// The identity is the (litter, agent, ts) triple rather than
    /// (litter, ts): the sender stamps `ot` from its own clock, so two
    /// agents in one litter can hand out the same microsecond, and keying
    /// on the pair alone would silently drop the second as a duplicate.
    pub fn is_duplicate(&self, ol: &str, from: &str, ot: u64) -> bool {
        self.seen.iter().any(|s| s.ol == ol && s.ot == ot && s.from == from)
    }

    pub fn remember(&mut self, seen: Seen) {
        self.seen.push(seen);
    }

    /// Drop seen-table entries past the relay age cutoff: past it the age
    /// guard discards any replay anyway, so remembering the triple buys
    /// nothing. Called on every relayed-in accept, so the table is
    /// steady-state small. The cap is a backstop against a hostile flood
    /// between prunes.
    pub fn prune_seen(&mut self, now_us: u64, max_age_us: u64) {
        let cutoff = now_us.saturating_sub(max_age_us);
        self.seen.retain(|s| s.ot >= cutoff);
        while self.seen.len() > SEEN_CAP {
            self.seen.remove(0);
        }
    }
}
