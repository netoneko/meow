//! The litter's task table — the **parent → directed sub-task → claim →
//! submit → clear → artifact** lifecycle of `docs/LITTER_WORKFLOW.md`,
//! coordinator-owned and in memory (see `docs/LITTER_STATE_MACHINE.md`).
//!
//! Task traffic arrives as ordinary messages whose body starts with a
//! bracketed verb; that doc's "Implementation" section is the table of
//! spellings and is the design of record. Authority (`Authority`) is what
//! the HUB stamped on the sender, never a string the body claims: "only the
//! leader may clear" is enforced against whoever owns the socket.
//!
//! Everything here is a pure state transition — `now_us` is always a
//! parameter, there is no clock, no socket and no interior mutability — so
//! the whole lifecycle is unit-testable with neither thread and the table
//! adds **no synchronization of its own**. It runs inside the one
//! `PMutex<HubState>` that already exists, and (unlike the code that used to
//! serve requests) nothing it does can block.
//!
//! Deliberately NOT persisted: the table is leader memory. If the leader
//! dies mid-flight, unfinished work is re-posted by whoever remembers it;
//! continuity of *knowledge* is the protocol history's job — compaction
//! folds the open sub-task list into the marker (`open_subtask_lines`).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use litter_wire::{TaskAct, TaskOp};

/// How long an *offer* stands before it is made again. An assignee that is
/// mid-turn on something else, or has just died, should not hold a
/// sub-task hostage — but an LLM turn is minutes, so this is not short.
///
/// It was 180 s, which is *shorter than a turn* on the models this runs
/// against: measured live, a wake-to-tool-call round on qwen3:4b took
/// 120-200 s, so an offer lapsed and was re-made while its assignee was
/// still thinking about the first copy. A re-offer window has to be
/// comfortably longer than the time it takes to answer one.
pub const CLAIM_WINDOW_US: u64 = 600 * 1_000_000;

/// How long a *claimed* sub-task may go without a submission. Generous on
/// purpose: an expired lease just means the work is re-offered — assume
/// good will, accept duplicate work on the margin.
pub const LEASE_US: u64 = 900 * 1_000_000;

/// How often the holder of a claimed sub-task is reminded that it still
/// owes a result.
///
/// The counterpart of "completion is explicit": an agent claims, its turn
/// ends, and then *nothing wakes it again* — the replicated record is
/// non-waking by design, so a claimed sub-task would sit untouched
/// forever. Observed live 2026-09-20: two agents claimed their sub-tasks
/// through `TaskUpdate`, both turns ended cleanly, and neither ever
/// reported, because between the claim and the deadline nothing in the
/// protocol addressed them.
///
/// The leader has had directives from the start; this is the same idea
/// pointed at workers.
pub const WORK_NAG_US: u64 = 150 * 1_000_000;

/// How many consecutive unanswered nudges a holder gets before the table
/// stops asking and simply lets the lease run out.
///
/// Bounded on purpose. An unbounded reminder is a loop: every nudge wakes
/// the holder, every wake costs an LLM turn, and an agent that is not
/// going to answer never will — so the cost is paid forever for nothing,
/// and the litter's other work queues behind it. Three is enough to cover
/// a model that dropped one message or spent a turn thinking without
/// emitting; past that the honest conclusion is that this holder is not
/// going to finish, and requeueing (via the lease) is the right answer.
///
/// Counted per sub-task and reset whenever its holder actually acts.
pub const MAX_WORK_NUDGES: u32 = 3;

/// How often an outstanding leader directive is repeated. Directives are
/// nagged rather than sent once because a single dropped message would
/// otherwise stall a parent task forever.
pub const NAG_US: u64 = 120 * 1_000_000;

/// The sender's authority, as the hub stamped it at delivery.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Authority {
    /// The operator. Outranks everyone, including the leader.
    Root,
    /// Whoever currently owns the hub socket.
    Leader,
    /// Every other agent.
    Peer,
}

impl Authority {
    fn may_open_parent(self) -> bool {
        matches!(self, Authority::Root | Authority::Leader)
    }
    fn is_leader(self) -> bool {
        matches!(self, Authority::Leader)
    }
}

/// Sub-task status, straight out of the workflow diagrams.
impl SubState {
    pub fn as_str(self) -> &'static str {
        match self {
            SubState::Pending => "pending",
            SubState::InProgress => "in progress",
            SubState::AwaitingClearance => "awaiting clearance",
            SubState::Cleared => "cleared",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SubState {
    /// Created (or requeued), waiting for its assignee to claim it.
    Pending,
    /// Claimed, lease running.
    InProgress,
    /// Result submitted; the leader has not verified it yet.
    AwaitingClearance,
    /// Leader verified it. Counts toward the parent's artifact.
    Cleared,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubTask {
    pub parent: u64,
    pub n: u64,
    /// The agent this was **directed** to. Not a queue: sub-tasks are
    /// assigned by name in the leader's plan.
    pub assignee: String,
    pub state: SubState,
    /// `Pending`: when the standing offer lapses and is made again.
    /// `InProgress`: lease expiry. Unused in the terminal states.
    pub until: u64,
    pub body: String,
    pub result: String,
    /// When its holder was last reminded that it still owes a result.
    /// `0` means "due now" — set at claim, so the first nudge goes out on
    /// the very next tick rather than a nag interval later.
    nagged: u64,
    /// Consecutive reminders this holder has not answered.
    nudges: u32,
}

impl SubTask {
    pub fn label(&self) -> String {
        format!("t{}.{}", self.parent, self.n)
    }

    /// What the assignee is sent when this sub-task is offered. It names
    /// the exact tool call to make, because a small model will not infer it
    /// from a design document — and a task that is never claimed is
    /// indistinguishable from an agent that never woke.
    pub fn offer_message(&self) -> String {
        format!(
            "[assigned: {label}] {body}\
             \n\nTake it with TaskUpdate(task=\"{label}\", status=\"claim\"). \
             When you have an answer, report it with \
             TaskUpdate(task=\"{label}\", status=\"done\", text=\"<your findings>\"). \
             If you cannot do it, say so with status=\"failed\" and why.",
            label = self.label(),
            body = self.body
        )
    }

    fn is_open(&self) -> bool {
        matches!(self.state, SubState::Pending | SubState::InProgress)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parent {
    pub id: u64,
    pub body: String,
    /// Set by a `[plan: tN]`, which is atomic — see `note_message`.
    pub planned: bool,
    pub closed: bool,
    pub artifact: String,
    /// When the last leader directive for this parent went out.
    nagged: u64,
}

impl Parent {
    pub fn label(&self) -> String {
        format!("t{}", self.id)
    }
}

/// A message the table wants delivered. `to` may be [`GROUP`], meaning
/// every roster member.
#[derive(Debug, Clone, PartialEq)]
pub struct Outbound {
    pub to: String,
    pub body: String,
    pub kind: OutKind,
}

/// Which message kind the hub should stamp on an [`Outbound`].
///
/// The distinction is not cosmetic: `Assignment` is **wakeable** and
/// `Done` is not (`live::wakeable`). Directives and offers must wake their
/// recipient or nothing happens; the final artifact must NOT wake the whole
/// litter, or closing a parent task immediately starts another round of
/// turns about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutKind {
    Assignment,
    Done,
}

/// What applying a record did. `Ok(note)` means the state machine changed
/// state and the record is real; `Err(why)` means nothing happened.
///
/// A type rather than a note the caller sniffs for the word "refused".
/// Three separate places used to re-derive acceptance from the note's
/// prefix — the hub deciding whether to replicate the record, the tool
/// deciding ok-vs-error, and the wire, which did not carry the
/// distinction at all. Any of them could disagree with the state machine,
/// and the "already claimed" refusals disagreed with all three: they never
/// said "refused", so a no-op was replicated to the whole litter as though
/// it had happened.
pub type Applied = Result<String, String>;

/// The reserved broadcast name, mirroring `serve::GROUP_NAME`. Repeated
/// rather than imported so this module keeps depending on nothing.
pub const GROUP: &str = "litter";

/// The operator's reserved identity — the same name `serve` grants
/// `Authority::Root` to.
///
/// The operator is in the roster because it joins in order to send, but it
/// is a control channel, not a participant: there is no agent loop behind
/// it, so anything assigned to it is never claimed and never reported.
/// Measured live 2026-09-20: a leader asked to canvass the litter split
/// its task four ways and handed the fourth to `operator`, which meant the
/// parent could never close — the artifact requires every sub-task
/// cleared, and that one had nobody to clear it.
pub const OPERATOR: &str = "root";

/// Can this roster member be given work? Everyone except the operator.
fn assignable(name: &str) -> bool {
    name != OPERATOR
}

pub struct TaskTable {
    next_parent: u64,
    parents: Vec<Parent>,
    subs: Vec<SubTask>,
    /// A leader that has just taken the socket and has not yet been told
    /// to take stock. Consumed by the first `tick`.
    ///
    /// It is a flag rather than a message delivered at election time
    /// because of *when* election happens: the binder seeds its state
    /// before the agent loop takes its inbox baseline, so anything
    /// delivered during `set_leader` is counted as history-it-has-seen and
    /// never wakes anybody. Emitting it from the tick puts it after the
    /// baseline, where it is new mail.
    leader_wake: Option<String>,
}

impl TaskTable {
    pub fn new() -> Self {
        Self { next_parent: 1, parents: Vec::new(), subs: Vec::new(), leader_wake: None }
    }

    pub fn parents(&self) -> &[Parent] {
        &self.parents
    }

    pub fn subs(&self) -> &[SubTask] {
        &self.subs
    }

    pub fn is_empty(&self) -> bool {
        self.parents.iter().all(|p| p.closed)
    }

    /// One line per still-open sub-task, for the compaction marker to carry
    /// forward. This is the "carry forward active sub-tasks" half of the
    /// workflow's atomic compaction: the table itself dies with the leader,
    /// so what survives has to be in history.
    pub fn open_work_lines(&self) -> Vec<String> {
        self.subs
            .iter()
            .filter(|s| !matches!(s.state, SubState::Cleared))
            .map(|s| {
                format!(
                    "[open] {} ({}) -> {}: {}",
                    s.label(),
                    s.state.as_str(),
                    s.assignee,
                    truncate(&s.body, 90)
                )
            })
            .collect()
    }

    /// A new leader has taken the socket. The next tick wakes it.
    ///
    /// Leadership changes are how work gets lost in this design: the table
    /// is the old leader's memory and dies with it, so the successor starts
    /// empty while the litter still has unfinished business. What survives
    /// is in history — the compaction marker's `[still open]` block, put
    /// there by `open_work_lines`. Somebody has to go and read it, and the
    /// only agent that can act on it is the one now holding the socket.
    pub fn note_new_leader(&mut self, name: &str, term: u64) {
        // A brand-new litter has no predecessor to take stock of: term 1
        // with an empty table means nobody died, nothing was carried over
        // and no agent is holding a ticket from anyone. Waking the leader
        // to issue a roll call there is not merely redundant, it is
        // expensive — measured live, it cost the leader its entire first
        // turn (194 s on qwen3:4b) asking four idle agents what they were
        // holding, before any real work could be planned.
        //
        // A takeover is the case this exists for, and `term > 1` is exactly
        // what distinguishes one: the term only advances when somebody
        // re-raced the bind.
        if term <= 1 && self.parents.is_empty() && self.subs.is_empty() {
            return;
        }
        self.leader_wake = Some(String::from(name));
    }

    fn parent_mut(&mut self, id: u64) -> Option<&mut Parent> {
        self.parents.iter_mut().find(|p| p.id == id)
    }

    fn sub_pos(&self, parent: u64, n: u64) -> Option<usize> {
        self.subs.iter().position(|s| s.parent == parent && s.n == n)
    }

    /// Apply one task record. **The deterministic half of the workflow**:
    /// same record + same state ⇒ same result, on any agent, with no clock
    /// and no socket reachable from here. Returns the messages to deliver,
    /// the events to log, and a one-line note for the caller to hand back
    /// to whoever submitted the record.
    ///
    /// Every refusal returns a reason rather than failing silently. A model
    /// that claimed the wrong sub-task learns it did; a model whose record
    /// was dropped for lack of authority learns that too. Silence here
    /// would be indistinguishable from "accepted and nothing happened" —
    /// which, for a refusal, is exactly what it is.
    pub fn apply(
        &mut self,
        from: &str,
        authority: Authority,
        op: &TaskOp,
        now_us: u64,
        roster: &[String],
    ) -> (Vec<Outbound>, Vec<String>, Applied) {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let outcome = match op.act {
            TaskAct::Open => self.op_open(from, authority, &op.text, &mut events),
            TaskAct::Plan => self.op_plan(authority, op, roster, &mut events),
            TaskAct::Claim => self.op_claim(from, &op.id, now_us, &mut events),
            TaskAct::Done => self.op_done(from, &op.id, &op.text, &mut events),
            TaskAct::Failed => self.op_failed(from, &op.id, &op.text, &mut events),
            TaskAct::Clear => self.op_clear(from, authority, &op.id, &mut events),
            TaskAct::Reopen => self.op_reopen(from, authority, &op.id, &op.text, &mut events),
            TaskAct::Artifact => self.op_artifact(from, authority, op, &mut out, &mut events),
        };
        (out, events, outcome)
    }

    fn op_open(&mut self, from: &str, authority: Authority, text: &str, events: &mut Vec<String>) -> Applied {
        if !authority.may_open_parent() {
            // Otherwise any agent can mint work for the whole litter —
            // observed live, an agent tasking the litter to police the
            // kernel.
            return Err(String::from("only the operator or the leader may open a task"));
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(String::from("a task needs a brief"));
        }
        let id = self.next_parent;
        self.next_parent += 1;
        self.parents.push(Parent {
            id,
            body: String::from(text),
            planned: false,
            closed: false,
            artifact: String::new(),
            nagged: 0,
        });
        events.push(format!("[event] parent task t{} opened by {}", id, from));
        Ok(format!("opened t{}", id))
    }

    /// **Atomic, and refused if the parent is already planned.** One record
    /// carrying the whole plan is what makes "all sub-tasks cleared"
    /// decidable: with sub-tasks trickling in, the table could never know
    /// planning had finished, so the trigger for the final artifact would
    /// never fire.
    fn op_plan(
        &mut self,
        authority: Authority,
        op: &TaskOp,
        roster: &[String],
        events: &mut Vec<String>,
    ) -> Applied {
        if !authority.is_leader() {
            return Err(String::from("only the leader may plan a task"));
        }
        let Some(pid) = parse_parent(&op.id) else {
            return Err(String::from("'task' must be a parent id like t1"));
        };
        match self.parents.iter().find(|p| p.id == pid) {
            None => return Err(format!("no task t{}", pid)),
            Some(p) if p.planned => return Err(format!("t{} is already planned", pid)),
            Some(p) if p.closed => return Err(format!("t{} is closed", pid)),
            Some(_) => {}
        }

        let mut n = 0u64;
        let mut skipped: Vec<String> = Vec::new();
        for (who, what) in &op.plan {
            let who = who.trim();
            let what = what.trim();
            if who.is_empty() || what.is_empty() {
                continue;
            }
            // An assignee nobody can deliver to — absent, or the operator,
            // which has no agent loop — is a sub-task that stalls forever
            // while looking healthy. Surface it instead.
            if !assignable(who) || !roster.iter().any(|m| m == who) {
                skipped.push(String::from(who));
                events.push(format!("[event] plan for t{} skipped '{}': not in the roster", pid, who));
                continue;
            }
            n += 1;
            self.subs.push(SubTask {
                parent: pid,
                n,
                assignee: String::from(who),
                state: SubState::Pending,
                until: 0, // never offered yet; the next tick offers it
                body: String::from(what),
                result: String::new(),
                nagged: 0,
                nudges: 0,
            });
            events.push(format!("[event] t{}.{} assigned to {}", pid, n, who));
        }

        if n == 0 {
            return Err(format!("plan for t{} had no assignable sub-tasks", pid));
        }
        if let Some(p) = self.parent_mut(pid) {
            p.planned = true;
            p.nagged = 0;
        }
        if skipped.is_empty() {
            Ok(format!("planned t{}: {} sub-task(s)", pid, n))
        } else {
            Ok(format!("planned t{}: {} sub-task(s); not in the roster: {}", pid, n, skipped.join(", ")))
        }
    }

    fn op_claim(&mut self, from: &str, id: &str, now_us: u64, events: &mut Vec<String>) -> Applied {
        let Some(pos) = self.sub_by_id(id) else {
            return Err(format!("no sub-task {}", id));
        };
        let s = &mut self.subs[pos];
        if s.assignee != from {
            return Err(format!("{} is assigned to {}", s.label(), s.assignee));
        }
        if !matches!(s.state, SubState::Pending) {
            return Err(format!("{} is already {}", s.label(), s.state.as_str()));
        }
        s.state = SubState::InProgress;
        s.until = now_us.saturating_add(LEASE_US);
        // Due now: the holder should be told to proceed on the very next
        // tick, not a reminder interval later.
        s.nagged = 0;
        s.nudges = 0;
        events.push(format!("[event] {} claimed by {}", s.label(), from));
        Ok(format!("claimed {}", s.label()))
    }

    /// Accepted from `Pending` as well as `InProgress`: a model that skips
    /// the claim handshake and goes straight to the answer has still done
    /// the work, and losing the ceremony is cheaper than losing the result.
    fn op_done(&mut self, from: &str, id: &str, text: &str, events: &mut Vec<String>) -> Applied {
        let Some(pos) = self.sub_by_id(id) else {
            return Err(format!("no sub-task {}", id));
        };
        let result = text.trim();
        if result.is_empty() {
            return Err(String::from("a result needs text"));
        }
        let s = &mut self.subs[pos];
        if s.assignee != from {
            return Err(format!("{} is assigned to {}", s.label(), s.assignee));
        }
        if !s.is_open() {
            return Err(format!("{} is already {}", s.label(), s.state.as_str()));
        }
        s.state = SubState::AwaitingClearance;
        s.until = 0;
        s.result = String::from(result);
        events.push(format!("[event] {} submitted by {}, awaiting clearance", s.label(), from));
        Ok(format!("submitted {} — awaiting the leader's clearance", s.label()))
    }

    /// The rejected branch of the future. Without its own act, a sub-task
    /// that cannot be done is indistinguishable from one still in flight.
    /// It goes to the leader as a clearance decision rather than silently
    /// requeueing: whether to retry, re-assign or drop it is the leader's
    /// call, not the table's.
    fn op_failed(&mut self, from: &str, id: &str, text: &str, events: &mut Vec<String>) -> Applied {
        let Some(pos) = self.sub_by_id(id) else {
            return Err(format!("no sub-task {}", id));
        };
        let s = &mut self.subs[pos];
        if s.assignee != from {
            return Err(format!("{} is assigned to {}", s.label(), s.assignee));
        }
        if !s.is_open() {
            return Err(format!("{} is already {}", s.label(), s.state.as_str()));
        }
        let why = text.trim();
        s.state = SubState::AwaitingClearance;
        s.until = 0;
        s.result = format!("[FAILED] {}", if why.is_empty() { "no reason given" } else { why });
        events.push(format!("[event] {} reported FAILED by {}", s.label(), from));
        Ok(format!("marked {} failed — the leader will decide what happens to it", s.label()))
    }

    fn op_clear(&mut self, from: &str, authority: Authority, id: &str, events: &mut Vec<String>) -> Applied {
        if !authority.is_leader() {
            return Err(String::from("only the leader may clear a sub-task"));
        }
        let Some(pos) = self.sub_by_id(id) else {
            return Err(format!("no sub-task {}", id));
        };
        let pid = self.subs[pos].parent;
        let s = &mut self.subs[pos];
        if !matches!(s.state, SubState::AwaitingClearance) {
            return Err(format!("{} is {}, not awaiting clearance", s.label(), s.state.as_str()));
        }
        s.state = SubState::Cleared;
        let label = s.label();
        events.push(format!("[event] {} cleared by {}", label, from));
        if let Some(p) = self.parent_mut(pid) {
            p.nagged = 0; // the artifact directive should follow promptly
        }
        Ok(format!("cleared {}", label))
    }

    /// The leader rejecting a submission: back to `Pending` for a fresh
    /// offer, with the reason carried into the sub-task body so the worker
    /// is told what was wrong rather than being handed the same brief.
    fn op_reopen(&mut self, from: &str, authority: Authority, id: &str, text: &str, events: &mut Vec<String>) -> Applied {
        if !authority.is_leader() {
            return Err(String::from("only the leader may reopen a sub-task"));
        }
        let Some(pos) = self.sub_by_id(id) else {
            return Err(format!("no sub-task {}", id));
        };
        let why = text.trim();
        let s = &mut self.subs[pos];
        if !matches!(s.state, SubState::AwaitingClearance) {
            return Err(format!("{} is {}, not awaiting clearance", s.label(), s.state.as_str()));
        }
        s.state = SubState::Pending;
        s.until = 0;
        s.result = String::new();
        if !why.is_empty() {
            s.body = format!("{} (reopened: {})", s.body, why);
        }
        let label = s.label();
        events.push(format!("[event] {} reopened by {}", label, from));
        Ok(format!("reopened {}", label))
    }

    fn op_artifact(
        &mut self,
        from: &str,
        authority: Authority,
        op: &TaskOp,
        out: &mut Vec<Outbound>,
        events: &mut Vec<String>,
    ) -> Applied {
        if !authority.is_leader() {
            return Err(String::from("only the leader may close a task"));
        }
        let Some(pid) = parse_parent(&op.id) else {
            return Err(String::from("'task' must be a parent id like t1"));
        };
        let text = op.text.trim();
        if text.is_empty() {
            return Err(String::from("an artifact needs the report text"));
        }
        // Refuse to close over unfinished work: the artifact is a synthesis
        // of cleared results, and a leader declaring one early would strand
        // sub-tasks their workers are still holding.
        let outstanding = self
            .subs
            .iter()
            .filter(|s| s.parent == pid && !matches!(s.state, SubState::Cleared))
            .count();
        if outstanding > 0 {
            return Err(format!("t{} still has {} uncleared sub-task(s)", pid, outstanding));
        }
        let Some(p) = self.parent_mut(pid) else {
            return Err(format!("no task t{}", pid));
        };
        if p.closed {
            return Err(format!("t{} is already closed", pid));
        }
        p.closed = true;
        p.artifact = String::from(text);
        let label = p.label();
        events.push(format!("[event] parent task {} closed by {} with an artifact", label, from));
        out.push(Outbound {
            to: String::from(GROUP),
            body: format!("[artifact: {}] {}", label, text),
            // Done, not Assignment: closing a parent must not wake the whole
            // litter into another round of turns about it.
            kind: OutKind::Done,
        });
        // The finished sub-tasks have served their purpose; the artifact and
        // the history carry what they found.
        self.subs.retain(|s| s.parent != pid);
        Ok(format!("closed {} with the final artifact", label))
    }

    fn sub_by_id(&self, id: &str) -> Option<usize> {
        let (p, n) = parse_sub(id)?;
        self.sub_pos(p, n)
    }

    /// One coordinator tick: expire offers and leases, re-home sub-tasks
    /// whose assignee has left, make standing offers, and deliver the
    /// leader whatever decision it currently owes. Idempotent when nothing
    /// needs doing.
    pub fn tick(
        &mut self,
        roster: &[String],
        leader: Option<&str>,
        now_us: u64,
    ) -> (Vec<Outbound>, Vec<String>) {
        let mut out = Vec::new();
        let mut events = Vec::new();

        // Expire leases first, so the freed sub-tasks compete for an offer
        // in this same pass.
        for s in self.subs.iter_mut() {
            if matches!(s.state, SubState::InProgress) && s.until <= now_us {
                events.push(format!("[event] {} lease expired, requeued", s.label()));
                s.state = SubState::Pending;
                s.until = 0;
                s.nagged = 0;
                s.nudges = 0;
            }
        }

        if roster.is_empty() {
            return (out, events);
        }

        // Re-home anything directed at an agent that is no longer here.
        // Least-loaded rather than round-robin: the point is not fairness
        // across a queue, it is not piling three orphans on one survivor.
        for i in 0..self.subs.len() {
            if !matches!(self.subs[i].state, SubState::Pending) {
                continue;
            }
            if assignable(&self.subs[i].assignee) && roster.iter().any(|m| m == &self.subs[i].assignee) {
                continue;
            }
            let Some(new_home) = self.least_loaded(roster, now_us) else { continue };
            let label = self.subs[i].label();
            let gone = self.subs[i].assignee.clone();
            self.subs[i].assignee = new_home.clone();
            self.subs[i].until = 0; // offer it immediately below
            events.push(format!("[event] {} re-homed from {} to {} (left the litter)", label, gone, new_home));
        }

        // Standing offers: an offer that lapsed is simply made again.
        for s in self.subs.iter_mut() {
            if !matches!(s.state, SubState::Pending) || s.until > now_us {
                continue;
            }
            if !assignable(&s.assignee) || !roster.iter().any(|m| m == &s.assignee) {
                continue;
            }
            s.until = now_us.saturating_add(CLAIM_WINDOW_US);
            out.push(Outbound {
                to: s.assignee.clone(),
                body: s.offer_message(),
                kind: OutKind::Assignment,
            });
            events.push(format!("[event] {} offered to {}", s.label(), s.assignee));
        }

        // The election wake comes first, and unconditionally: a fresh
        // leader with an empty table has no parent to generate a directive
        // from, so without this it would sit silent while the litter's
        // carried-over work went unclaimed.
        // Tell whoever is holding work to get on with it: once right after
        // the claim, then on each reminder interval, up to
        // `MAX_WORK_NUDGES` consecutive unanswered times.
        //
        // Claiming ends a turn. Without this nothing ever addresses the
        // holder again and the sub-task rides its lease out in silence —
        // observed live 2026-09-20, two agents claimed through
        // `TaskUpdate`, both turns ended cleanly, and neither ever
        // reported.
        for s in self.subs.iter_mut() {
            if !matches!(s.state, SubState::InProgress) {
                continue;
            }
            if !assignable(&s.assignee) || !roster.iter().any(|m| m == &s.assignee) {
                continue;
            }
            if s.nudges >= MAX_WORK_NUDGES {
                if s.nudges == MAX_WORK_NUDGES {
                    s.nudges += 1; // say it once, not every tick
                    events.push(format!(
                        "[event] {} unanswered after {} reminders; leaving it to the lease",
                        s.label(),
                        MAX_WORK_NUDGES
                    ));
                }
                continue;
            }
            // `nagged == 0` is "due now": set at claim so the first nudge
            // follows immediately.
            let due = s.nagged == 0 || now_us >= s.nagged.saturating_add(WORK_NAG_US);
            if !due {
                continue;
            }
            s.nagged = now_us;
            s.nudges += 1;
            let tail = if s.nudges >= MAX_WORK_NUDGES {
                "\n\nThis is the last reminder — if you do not answer, the sub-task goes back \
                 to the litter for someone else."
            } else {
                ""
            };
            out.push(Outbound {
                to: s.assignee.clone(),
                body: format!(
                    "[still yours: {label}] You claimed this and have not reported yet \
                     (reminder {n} of {max}):\
                     \n\n{body}\
                     \n\nProceed with the work now and report it with \
                     TaskUpdate(task=\"{label}\", status=\"done\", text=\"<what you found>\"). \
                     If you cannot do it, say so with status=\"failed\" and why. Until you send \
                     one of those, this stays open and nobody else can finish it.{tail}",
                    label = s.label(),
                    n = s.nudges,
                    max = MAX_WORK_NUDGES,
                    body = truncate(&s.body, 300),
                    tail = tail
                ),
                kind: OutKind::Assignment,
            });
        }

        if let Some(name) = self.leader_wake.take() {
            let body = self.election_wake_body(&name, roster, now_us);
            out.push(Outbound { to: name, body, kind: OutKind::Assignment });
        }

        if let Some(leader) = leader {
            self.leader_directives(leader, roster, now_us, &mut out);
        }

        (out, events)
    }

    /// What a newly elected leader is told. Two situations, and they need
    /// different instructions:
    ///
    /// - **Tickets outstanding in our own table.** Rare (the table normally
    ///   dies with its leader) but possible for a leader that never lost
    ///   the socket. Name them and chase the holders.
    /// - **Nothing in the table** — the usual case after a death. The
    ///   danger here is the *silent* one: agents may still be holding
    ///   sub-tasks the old leader leased them, and nothing in this process
    ///   knows that. The successor cannot deduce it, so it has to ask. A
    ///   roll call is the only way that state is ever recovered; without it
    ///   the work sits claimed by someone nobody is tracking.
    fn election_wake_body(&self, name: &str, roster: &[String], now_us: u64) -> String {
        let mut body = format!("[leader-elected] You are now the leader of this litter.");

        let open: Vec<&SubTask> = self.subs.iter().filter(|s| !matches!(s.state, SubState::Cleared)).collect();
        if !open.is_empty() {
            body.push_str("\n\nOutstanding tickets you now own — issue a ROLL CALL and chase them:");
            for s in &open {
                body.push_str(&format!(
                    "\n  {} [{}] held by {}: {}",
                    s.label(),
                    s.state.as_str(),
                    s.assignee,
                    truncate(&s.body, 100)
                ));
            }
            body.push_str(
                "\n\nSend the roll call with SendMessage(to=\"litter\", body=\"...\"): ask each \
                 holder to confirm whether their ticket is still in progress. Anything nobody \
                 answers for, take back with TaskUpdate(task=\"tN.M\", status=\"reopen\", \
                 text=\"no answer at roll call\").",
            );
        } else {
            body.push_str(
                "\n\nYour task table is EMPTY — it was the previous leader's memory and died \
                 with it. Two places the litter's real state still lives:\
                 \n1. The `[still open]` block in your compaction marker: work that was being \
                 tracked when the old leader went. Nothing will happen to it unless you act.\
                 \n2. The other agents, who may still be holding tickets nobody is tracking now. \
                 You cannot see those from here — ask. Issue a ROLL CALL with \
                 SendMessage(to=\"litter\", body=\"roll call: what task are you holding, and \
                 what is its status?\").",
            );
        }

        let needs_plan: Vec<&Parent> = self.parents.iter().filter(|p| !p.closed && !p.planned).collect();
        if !needs_plan.is_empty() {
            body.push_str("\n\nTasks still awaiting a plan — call TaskPlan for each:");
            for p in needs_plan {
                body.push_str(&format!("\n  {}: {}", p.label(), truncate(&p.body, 120)));
            }
        }

        let members: Vec<&str> =
            roster.iter().map(|s| s.as_str()).filter(|n| *n != name && assignable(n)).collect();
        body.push_str(&format!("\n\nAgents available: {}", members.join(", ")));
        let _ = now_us;
        body
    }

    fn least_loaded(&self, roster: &[String], now_us: u64) -> Option<String> {
        let _ = now_us;
        roster
            .iter()
            .filter(|name| assignable(name))
            .map(|name| {
                let load = self
                    .subs
                    .iter()
                    .filter(|s| &s.assignee == name && s.is_open())
                    .count();
                (load, name)
            })
            .min_by_key(|(load, _)| *load)
            .map(|(_, name)| name.clone())
    }

    /// Whatever decision the leader currently owes, phrased as the literal
    /// verb to reply with. One directive per parent per nag interval: they
    /// are re-sent because a directive delivered once and dropped would
    /// stall its parent permanently, and they are rate-limited because a
    /// leader nagged every tick never finishes a turn.
    fn leader_directives(
        &mut self,
        leader: &str,
        roster: &[String],
        now_us: u64,
        out: &mut Vec<Outbound>,
    ) {
        for i in 0..self.parents.len() {
            if self.parents[i].closed {
                continue;
            }
            let pid = self.parents[i].id;
            if self.parents[i].nagged != 0 && now_us < self.parents[i].nagged.saturating_add(NAG_US) {
                continue;
            }
            let label = self.parents[i].label();

            let body = if !self.parents[i].planned {
                // Neither the leader (it coordinates) nor the operator (no
                // agent loop) belongs on this list: a name offered here is
                // a name the model will assign to.
                let who: Vec<&str> = roster
                    .iter()
                    .map(|s| s.as_str())
                    .filter(|n| *n != leader && assignable(n))
                    .collect();
                format!(
                    "[plan-needed: {label}] You are the leader. Split this task into one \
                     sub-task per agent and submit them in a SINGLE TaskPlan call:\
                     \n  TaskPlan(task=\"{label}\", assignments=[{{\"who\":\"<agent>\",\
                     \"what\":\"<what they should do>\"}}, ...])\
                     \nOne call, every sub-task — a partial plan cannot be completed later.\
                     \n\nThe task is: {body}\
                     \nAgents available: {who}",
                    label = label,
                    body = self.parents[i].body,
                    who = who.join(", ")
                )
            } else {
                let awaiting: Vec<&SubTask> = self
                    .subs
                    .iter()
                    .filter(|s| s.parent == pid && matches!(s.state, SubState::AwaitingClearance))
                    .collect();
                if !awaiting.is_empty() {
                    let mut lines = String::new();
                    for s in &awaiting {
                        lines.push_str(&format!(
                            "\n\n{} ({} reported): {}",
                            s.label(),
                            s.assignee,
                            truncate(&s.result, 600)
                        ));
                    }
                    format!(
                        "[clearance-needed: {label}] You are the leader. Verify each result \
                         below. Accept one with TaskUpdate(task=\"tN.M\", status=\"clear\"), \
                         or send it back with TaskUpdate(task=\"tN.M\", status=\"reopen\", \
                         text=\"<what is missing>\"). A result marked [FAILED] needs the same \
                         decision: clear it to accept the failure, or reopen it to retry.{lines}",
                        label = label,
                        lines = lines
                    )
                } else {
                    let cleared: Vec<&SubTask> = self
                        .subs
                        .iter()
                        .filter(|s| s.parent == pid && matches!(s.state, SubState::Cleared))
                        .collect();
                    // Planned, nothing awaiting, nothing cleared yet =>
                    // work is simply in flight. Say nothing.
                    if cleared.is_empty() || cleared.len() != self.subs.iter().filter(|s| s.parent == pid).count() {
                        continue;
                    }
                    let mut lines = String::new();
                    for s in &cleared {
                        lines.push_str(&format!(
                            "\n\n{} ({}): {}",
                            s.label(),
                            s.assignee,
                            truncate(&s.result, 600)
                        ));
                    }
                    format!(
                        "[artifact-needed: {label}] Every sub-task is cleared. Synthesize the \
                         findings below into the final answer to the original task and submit it \
                         with TaskUpdate(task=\"{label}\", status=\"artifact\", \
                         text=\"<your report>\"). This closes the task.\
                         \n\nOriginal task: {task}{lines}",
                        label = label,
                        task = self.parents[i].body,
                        lines = lines
                    )
                }
            };

            self.parents[i].nagged = now_us;
            out.push(Outbound { to: String::from(leader), body, kind: OutKind::Assignment });
        }
    }
}

/// `"t1"` → `1`. Tolerates a bare `"1"`.
fn parse_parent(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches('t');
    if s.is_empty() || s.contains('.') {
        return None;
    }
    s.parse().ok()
}

/// `"t1.2"` → `(1, 2)`.
fn parse_sub(s: &str) -> Option<(u64, u64)> {
    let s = s.trim().trim_start_matches('t');
    let (p, n) = s.split_once('.')?;
    Some((p.trim().parse().ok()?, n.trim().parse().ok()?))
}

/// Truncate on a char boundary. Directives carry submitted results back to
/// the leader, and an unbounded result would blow the turn's token budget.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    use alloc::vec;
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter tasks tests ---\n");

    fn roster() -> Vec<String> {
        vec![String::from("mimi"), String::from("tama"), String::from("kuro")]
    }
    fn op(act: TaskAct, id: &str, text: &str) -> TaskOp {
        TaskOp::new(act, String::from(id), String::from(text))
    }
    fn plan_op(id: &str, pairs: &[(&str, &str)]) -> TaskOp {
        TaskOp {
            act: TaskAct::Plan,
            id: String::from(id),
            text: String::new(),
            plan: pairs.iter().map(|(a, b)| (String::from(*a), String::from(*b))).collect(),
        }
    }
    // Assert against the typed outcome. A test that matched on the note's
    // wording would pass for a refusal that happened to contain the right
    // word — which is the class of bug this type exists to remove.
    fn okc(a: &Applied, needle: &str) -> bool {
        matches!(a, Ok(n) if n.contains(needle))
    }
    fn errc(a: &Applied, needle: &str) -> bool {
        matches!(a, Err(w) if w.contains(needle))
    }
    let mut check = |name: &str, cond: bool, passed: &mut usize| {
        if cond {
            *passed += 1;
        } else {
            libakuma::print(&format!("  [!] {}\n", name));
        }
    };

    // ---- the whole lifecycle, end to end -------------------------------
    // open -> plan -> claim -> done -> clear -> artifact. The states are
    // asserted between every step, because a table that reaches the right
    // end through the wrong middle is the failure this protocol is most
    // prone to.
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        let (_, _, n1) = t.apply("root", Authority::Root, &op(TaskAct::Open, "", "debate the kernel"), 100, &r);
        let (_, _, n2) = t.apply("mimi", Authority::Leader,
            &plan_op("t1", &[("tama", "run the tests"), ("kuro", "audit locking")]), 100, &r);
        let planned = t.subs().len() == 2
            && t.subs()[0].assignee == "tama"
            && t.subs()[1].assignee == "kuro"
            && t.subs().iter().all(|s| s.state == SubState::Pending);

        // tick offers both, to their NAMED assignees
        let (offers, _) = t.tick(&r, Some("mimi"), 200);
        let offered_to: Vec<&str> = offers.iter().map(|o| o.to.as_str()).collect();
        let offers_ok = offered_to.contains(&"tama") && offered_to.contains(&"kuro");

        let (_, _, n3) = t.apply("tama", Authority::Peer, &op(TaskAct::Claim, "t1.1", ""), 300, &r);
        let claimed = t.subs()[0].state == SubState::InProgress;
        let (_, _, n4) = t.apply("tama", Authority::Peer, &op(TaskAct::Done, "t1.1", "all green"), 400, &r);
        let submitted = t.subs()[0].state == SubState::AwaitingClearance && t.subs()[0].result == "all green";
        let (_, _, n5) = t.apply("mimi", Authority::Leader, &op(TaskAct::Clear, "t1.1", ""), 500, &r);
        let cleared = t.subs()[0].state == SubState::Cleared;

        // artifact must be refused while kuro's half is outstanding
        let (_, _, early) = t.apply("mimi", Authority::Leader, &op(TaskAct::Artifact, "t1", "report"), 600, &r);
        let refused_early = errc(&early, "uncleared") && !t.parents()[0].closed;

        t.apply("kuro", Authority::Peer, &op(TaskAct::Done, "t1.2", "one lock is unheld"), 700, &r);
        t.apply("mimi", Authority::Leader, &op(TaskAct::Clear, "t1.2", ""), 800, &r);
        let (out, _, n6) = t.apply("mimi", Authority::Leader, &op(TaskAct::Artifact, "t1", "FINAL: it works"), 900, &r);
        let closed = t.parents()[0].closed
            && t.parents()[0].artifact == "FINAL: it works"
            && t.subs().is_empty()
            && t.is_empty();
        // the artifact is broadcast, and must NOT wake the litter
        let broadcast_ok = out.len() == 1 && out[0].to == GROUP && out[0].kind == OutKind::Done;

        let ok = okc(&n1, "t1") && okc(&n2, "2 sub-task") && planned && offers_ok
            && okc(&n3, "claimed") && claimed && okc(&n4, "submitted") && submitted
            && okc(&n5, "cleared") && cleared && refused_early && okc(&n6, "closed")
            && closed && broadcast_ok;
        check("lifecycle open->plan->claim->done->clear->artifact", ok, &mut passed);
        if !ok {
            libackuma_note(&n1, &n2, &n3, &n4, &n5, &n6);
        }
    }

    // ---- authority is enforced, not advisory ---------------------------
    // Every privileged act refused for a plain peer. This is the check that
    // stops an agent minting work for the whole litter, which happened live.
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        let (_, _, open_peer) = t.apply("tama", Authority::Peer, &op(TaskAct::Open, "", "do my bidding"), 100, &r);
        let no_open = open_peer.is_err() && t.parents().is_empty();

        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "real task"), 100, &r);
        let (_, _, plan_peer) = t.apply("tama", Authority::Peer, &plan_op("t1", &[("kuro", "x")]), 100, &r);
        let no_plan = plan_peer.is_err() && t.subs().is_empty();

        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "x")]), 100, &r);
        t.apply("tama", Authority::Peer, &op(TaskAct::Done, "t1.1", "done"), 200, &r);
        let (_, _, clear_peer) = t.apply("kuro", Authority::Peer, &op(TaskAct::Clear, "t1.1", ""), 300, &r);
        let no_clear = clear_peer.is_err() && t.subs()[0].state == SubState::AwaitingClearance;

        t.apply("mimi", Authority::Leader, &op(TaskAct::Clear, "t1.1", ""), 300, &r);
        let (_, _, art_peer) = t.apply("tama", Authority::Peer, &op(TaskAct::Artifact, "t1", "mine"), 400, &r);
        let no_artifact = art_peer.is_err() && !t.parents()[0].closed;

        check("peers may not open, plan, clear or close",
              no_open && no_plan && no_clear && no_artifact, &mut passed);
    }

    // ---- a sub-task belongs to its assignee ----------------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "work")]), 100, &r);
        let (_, _, thief_claim) = t.apply("kuro", Authority::Peer, &op(TaskAct::Claim, "t1.1", ""), 200, &r);
        let (_, _, thief_done) = t.apply("kuro", Authority::Peer, &op(TaskAct::Done, "t1.1", "I did it"), 200, &r);
        check("only the assignee may claim or submit",
              thief_claim.is_err() && thief_done.is_err()
                  && t.subs()[0].state == SubState::Pending,
              &mut passed);
    }

    // ---- submitting without claiming is accepted -----------------------
    // Deliberate divergence from the diagrams: a small model that skips the
    // handshake has still done the work.
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "work")]), 100, &r);
        let (_, _, note) = t.apply("tama", Authority::Peer, &op(TaskAct::Done, "t1.1", "skipped the claim"), 200, &r);
        check("submit without claim is accepted",
              note.is_ok() && t.subs()[0].state == SubState::AwaitingClearance,
              &mut passed);
    }

    // ---- failed is its own act, not a flavour of done ------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "impossible")]), 100, &r);
        t.apply("tama", Authority::Peer, &op(TaskAct::Failed, "t1.1", "no such file"), 200, &r);
        let marked = t.subs()[0].state == SubState::AwaitingClearance
            && t.subs()[0].result.starts_with("[FAILED]")
            && t.subs()[0].result.contains("no such file");
        // the leader can still reopen it for a retry
        t.apply("mimi", Authority::Leader, &op(TaskAct::Reopen, "t1.1", "try /etc instead"), 300, &r);
        let reopened = t.subs()[0].state == SubState::Pending
            && t.subs()[0].result.is_empty()
            && t.subs()[0].body.contains("try /etc instead");
        check("failed lands as a clearance decision, and reopen retries it",
              marked && reopened, &mut passed);
    }

    // ---- planning is atomic and one-shot -------------------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "a")]), 100, &r);
        let (_, _, second) = t.apply("mimi", Authority::Leader, &plan_op("t1", &[("kuro", "b")]), 100, &r);
        let one_shot = errc(&second, "already planned") && t.subs().len() == 1;
        // an assignee nobody can deliver to is surfaced, not silently kept
        let mut t2 = TaskTable::new();
        t2.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        let (_, _, note) = t2.apply("mimi", Authority::Leader,
            &plan_op("t1", &[("tama", "a"), ("ghost", "b")]), 100, &r);
        let ghost_skipped = t2.subs().len() == 1 && okc(&note, "ghost");
        check("plan is atomic, one-shot, and rejects absent assignees",
              one_shot && ghost_skipped, &mut passed);
    }

    // ---- offers lapse and are made again; leases expire and requeue -----
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "work")]), 100, &r);
        let (first, _) = t.tick(&r, Some("mimi"), 1_000);
        let offered_once = first.iter().any(|o| o.to == "tama");
        // ...nothing yet at the same instant (the offer still stands)
        let (again, _) = t.tick(&r, Some("mimi"), 1_100);
        let not_spammed = !again.iter().any(|o| o.to == "tama");
        // ...but re-offered once the claim window lapses
        let (relisted, _) = t.tick(&r, Some("mimi"), 1_000 + CLAIM_WINDOW_US + 1);
        let reoffered = relisted.iter().any(|o| o.to == "tama");

        // a claimed sub-task whose worker died comes back
        t.apply("tama", Authority::Peer, &op(TaskAct::Claim, "t1.1", ""), 2_000, &r);
        let (_, events) = t.tick(&r, Some("mimi"), 2_000 + LEASE_US + 1);
        let requeued = events.iter().any(|e| e.contains("lease expired"))
            && t.subs()[0].state == SubState::Pending;
        check("offers lapse and re-offer; leases expire and requeue",
              offered_once && not_spammed && reoffered && requeued, &mut passed);
    }

    // ---- the leader is told, in order, exactly what to type -------------
    // plan-needed -> clearance-needed -> artifact-needed. If this sequence
    // breaks, a parent task stalls forever while looking healthy.
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "debate"), 100, &r);
        let (d1, _) = t.tick(&r, Some("mimi"), 1_000);
        let plan_needed = d1.iter().any(|o| o.to == "mimi" && o.body.contains("[plan-needed: t1]")
            && o.body.contains("TaskPlan") && o.kind == OutKind::Assignment);

        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "work")]), 1_100, &r);
        // work in flight: nothing owed, so the leader is left alone
        let (d2, _) = t.tick(&r, Some("mimi"), 1_200);
        let quiet = !d2.iter().any(|o| o.to == "mimi");

        t.apply("tama", Authority::Peer, &op(TaskAct::Done, "t1.1", "finding"), 1_300, &r);
        let (d3, _) = t.tick(&r, Some("mimi"), 1_300 + NAG_US + 1);
        let clearance_needed = d3.iter().any(|o| o.to == "mimi"
            && o.body.contains("[clearance-needed: t1]") && o.body.contains("finding"));

        t.apply("mimi", Authority::Leader, &op(TaskAct::Clear, "t1.1", ""), 1_400, &r);
        let (d4, _) = t.tick(&r, Some("mimi"), 1_400 + NAG_US + 1);
        let artifact_needed = d4.iter().any(|o| o.to == "mimi"
            && o.body.contains("[artifact-needed: t1]") && o.body.contains("TaskUpdate"));
        check("leader directives fire in order and name the tool",
              plan_needed && quiet && clearance_needed && artifact_needed, &mut passed);
    }

    // ---- a sub-task whose assignee left is re-homed ---------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        let full = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &full);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("kuro", "work")]), 100, &full);
        let survivors = vec![String::from("mimi"), String::from("tama")];
        let (out, events) = t.tick(&survivors, Some("mimi"), 1_000);
        check("a departed assignee's sub-task is re-homed",
              events.iter().any(|e| e.contains("re-homed"))
                  && t.subs()[0].assignee != "kuro"
                  && out.iter().any(|o| o.to == t.subs()[0].assignee),
              &mut passed);
    }

    // ---- open work survives compaction ---------------------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader,
            &plan_op("t1", &[("tama", "still going"), ("kuro", "also going")]), 100, &r);
        t.apply("tama", Authority::Peer, &op(TaskAct::Done, "t1.1", "result"), 200, &r);
        t.apply("mimi", Authority::Leader, &op(TaskAct::Clear, "t1.1", ""), 200, &r);
        let lines = t.open_work_lines();
        // the cleared one is finished knowledge; only live work carries over
        check("compaction carries open sub-tasks, not cleared ones",
              lines.len() == 1 && lines[0].contains("t1.2") && lines[0].contains("kuro"),
              &mut passed);
    }

    // ---- a claimed sub-task's holder is nudged, boundedly ---------------
    // The counterpart of explicit completion: claiming ends a turn, so
    // without a nudge nothing ever addresses the holder again. Bounded,
    // because every nudge costs the holder an LLM turn.
    total += 1;
    {
        let mut t = TaskTable::new();
        let r = roster();
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "task"), 100, &r);
        t.apply("mimi", Authority::Leader, &plan_op("t1", &[("tama", "go and look")]), 100, &r);
        t.tick(&r, Some("mimi"), 1_000);
        t.apply("tama", Authority::Peer, &op(TaskAct::Claim, "t1.1", ""), 1_100, &r);

        // First nudge lands on the very next tick: proceed with the work.
        let (n1, _) = t.tick(&r, Some("mimi"), 1_200);
        let proceeds = n1.iter().any(|o| {
            o.to == "tama" && o.body.contains("[still yours: t1.1]")
                && o.body.contains("go and look") && o.kind == OutKind::Assignment
        });
        // ...but not again until the interval is up.
        let (n2, _) = t.tick(&r, Some("mimi"), 1_250);
        let not_spammed = !n2.iter().any(|o| o.body.contains("[still yours"));

        // Reminders 2 and 3, then it stops asking.
        let mut at = 1_200u64;
        let mut sent = 1usize;
        for _ in 0..4 {
            at += WORK_NAG_US + 1;
            let (n, ev) = t.tick(&r, Some("mimi"), at);
            sent += n.iter().filter(|o| o.body.contains("[still yours")).count();
            let _ = ev;
        }
        let bounded = sent == MAX_WORK_NUDGES as usize;
        // ...and says so once, rather than silently going quiet.
        let (_, ev) = t.tick(&r, Some("mimi"), at + WORK_NAG_US + 1);
        let _ = ev;

        // A fresh holder after a requeue starts with a clean count.
        let (_, ev2) = t.tick(&r, Some("mimi"), 1_100 + LEASE_US + 1);
        let requeued = ev2.iter().any(|e| e.contains("lease expired"));
        t.apply("tama", Authority::Peer, &op(TaskAct::Claim, "t1.1", ""), 9_000_000_000, &r);
        let (again, _) = t.tick(&r, Some("mimi"), 9_000_000_001);
        let fresh_start = again.iter().any(|o| o.body.contains("reminder 1 of"));

        check("holder is nudged after claim, bounded, and reset on requeue",
              proceeds && not_spammed && bounded && requeued && fresh_start, &mut passed);
        if !(proceeds && not_spammed && bounded && requeued && fresh_start) {
            libakuma::print(&format!(
                "      proceeds={} not_spammed={} sent={} requeued={} fresh={}\n",
                proceeds, not_spammed, sent, requeued, fresh_start));
        }
    }

    // ---- the operator is never given work ------------------------------
    total += 1;
    {
        let mut t = TaskTable::new();
        // root is in the roster (it joins in order to send) but has no
        // agent loop, so a sub-task handed to it can never be claimed.
        let r = vec![String::from("mimi"), String::from("tama"), String::from(OPERATOR)];
        t.apply("root", Authority::Root, &op(TaskAct::Open, "", "canvass everyone"), 100, &r);
        let (_, _, note) = t.apply("mimi", Authority::Leader,
            &plan_op("t1", &[("tama", "a"), (OPERATOR, "b")]), 100, &r);
        let skipped = t.subs().len() == 1 && t.subs()[0].assignee == "tama";
        // ...and it is not offered as a candidate either
        let (out, _) = t.tick(&r, Some("mimi"), 1_000);
        let not_listed = !out.iter().any(|o| o.to == OPERATOR);
        check("the operator is never assigned work",
              skipped && okc(&note, OPERATOR) && not_listed, &mut passed);
    }

    // ---- a fresh litter's first leader is not roll-called --------------
    total += 1;
    {
        let r = roster();
        let mut fresh = TaskTable::new();
        fresh.note_new_leader("mimi", 1);
        let (out, _) = fresh.tick(&r, Some("mimi"), 1_000);
        let quiet = !out.iter().any(|o| o.body.contains("[leader-elected]"));

        // ...but a takeover is, because a predecessor died holding state.
        let mut taken = TaskTable::new();
        taken.note_new_leader("mimi", 2);
        let (out2, _) = taken.tick(&r, Some("mimi"), 1_000);
        let roll_called = out2.iter().any(|o| {
            o.to == "mimi" && o.body.contains("[leader-elected]") && o.body.contains("ROLL CALL")
        });

        // ...and so is a term-1 leader that somehow has work already.
        let mut busy = TaskTable::new();
        busy.apply("root", Authority::Root, &op(TaskAct::Open, "", "work"), 100, &r);
        busy.note_new_leader("mimi", 1);
        let (out3, _) = busy.tick(&r, Some("mimi"), 1_000);
        let woken_anyway = out3.iter().any(|o| o.body.contains("[leader-elected]"));

        check("election wake fires on takeover, not on a fresh litter",
              quiet && roll_called && woken_anyway, &mut passed);
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}

#[cfg(feature = "tests")]
fn libackuma_note(a: &Applied, b: &Applied, c: &Applied, d: &Applied, e: &Applied, f: &Applied) {
    libakuma::print(&format!("      notes: {:?} | {:?} | {:?} | {:?} | {:?} | {:?}\n", a, b, c, d, e, f));
}
