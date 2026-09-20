//! The litter's task table — in-memory, coordinator-owned (see
//! `docs/LITTER_STATE_MACHINE.md`). Tasks enter as ordinary messages whose
//! body starts with `[task]` (operator or peer — the table doesn't care);
//! completion is a `[done: <id>]` reply from the assigned holder. The
//! coordinator's tick assigns pending tasks round-robin across the roster
//! with a lease (`holder`/`until`), requeues expired leases, and turns
//! completions into cluster events. Everything here is a pure state
//! transition so the whole table is unit-testable with no socket and no
//! clock — `now` is always a parameter.
//!
//! Deliberately NOT persisted: the table is leader memory. If the leader
//! dies mid-queue, unfinished tasks are re-posted by whoever remembers
//! them; continuity of *knowledge* is the protocol history's job (the
//! compaction marker carries the folded `[done]` summaries).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// How long an assignment may go without a `[done: <id>]` reply. Generous
/// on purpose (an LLM turn can take minutes): an expired lease just means
/// the task is re-offered — assume good will, accept duplicate work on the
/// margin.
pub const LEASE_US: u64 = 900 * 1_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: u64,
    pub holder: Option<String>,
    /// Lease expiry (µs since the epoch); 0 = unassigned.
    pub until: u64,
    pub body: String,
}

impl Task {
    pub fn is_leased(&self, now_us: u64) -> bool {
        self.holder.is_some() && self.until > now_us
    }

    /// The message an agent receives when this task is assigned to it.
    pub fn assignment_message(&self) -> String {
        format!(
            "[assigned: t{}] {}\
             \nReply with a message starting `[done: t{}]` when finished.",
            self.id, self.body, self.id
        )
    }
}

/// One cluster event worth telling every agent about, plus the table's
/// own bookkeeping. `TaskEvent`s are appended to the hub's event log by
/// the caller (they belong to the shared feed, not to this table).
#[derive(Debug, Clone, PartialEq)]
pub enum TableEvent {
    /// "[event] task t3 assigned to sherlock"
    Noted,
    /// "[event] task t3 requeued (lease expired)"
    Requeued,
    /// ("[event] task t3 done by hercules: <summary>", summary)
    Done(String, String),
}

pub struct TaskTable {
    next_id: u64,
    tasks: Vec<Task>,
}

impl TaskTable {
    pub fn new() -> Self {
        Self { next_id: 1, tasks: Vec::new() }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// One line per still-open task, for the compaction marker to carry
    /// forward. The table is leader memory and dies with the leader, so
    /// what has to survive a leadership change has to be in history.
    pub fn open_work_lines(&self) -> Vec<String> {
        self.tasks
            .iter()
            .map(|t| {
                format!(
                    "[open] t{} -> {}: {}",
                    t.id,
                    t.holder.as_deref().unwrap_or("unassigned"),
                    t.body
                )
            })
            .collect()
    }

    /// Feed one inbound message through the table. `[task] …` opens a task;
    /// `[done: tN] …` from the task's holder closes it. Everything else is
    /// ignored. `now_us` stamps new tasks' unassigned state.
    /// `can_open=false` demotes `[task]` bodies to ordinary chat (peer
    /// role); `[done: tN]` closes are accepted from the holder regardless.
    pub fn note_message(&mut self, from: &str, body: &str, now_us: u64, can_open: bool) -> TableEvent {
        if let Some(rest) = body.trim().strip_prefix("[task]") {
            if !can_open {
                return TableEvent::Noted;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.tasks.push(Task {
                id,
                holder: None,
                until: 0,
                body: String::from(rest.trim()),
            });
            let _ = (from, now_us);
            return TableEvent::Noted;
        }
        if let Some(rest) = body.trim().strip_prefix("[done:") {
            let mut parts = rest.splitn(2, ']');
            let id_part = parts.next().unwrap_or("").trim();
            let summary = parts.next().unwrap_or("").trim();
            let id: u64 = match id_part.trim_start_matches('t').parse() {
                Ok(v) => v,
                Err(_) => return TableEvent::Noted, // not a well-formed done — ignore
            };
            if let Some(pos) = self.tasks.iter().position(|t| t.id == id) {
                let task = &self.tasks[pos];
                if task.holder.as_deref() != Some(from) {
                    // Only the holder can close a task — goodwill plus a
                    // little bookkeeping discipline.
                    return TableEvent::Noted;
                }
                self.tasks.remove(pos);
                return TableEvent::Done(
                    format!("[event] task t{} done by {}: {}", id, from, summary),
                    String::from(summary),
                );
            }
        }
        TableEvent::Noted
    }

    /// One coordinator tick: assign unassigned tasks round-robin over
    /// `roster`, requeue expired leases, and return (recipient, body)
    /// messages to deliver plus event-log lines. Idempotent when nothing
    /// needs doing.
    pub fn tick(&mut self, roster: &[String], now_us: u64) -> (Vec<(String, String)>, Vec<String>) {
        let mut messages = Vec::new();
        let mut events = Vec::new();

        // Requeue expired leases first so they compete for assignment in
        // the same pass.
        for t in self.tasks.iter_mut() {
            if t.holder.is_some() && !t.is_leased(now_us) {
                events.push(format!("[event] task t{} requeued (lease expired)", t.id));
                t.holder = None;
                t.until = 0;
            }
        }

        if roster.is_empty() {
            return (messages, events);
        }

        // Live load per roster member, computed once: assignment picks the
        // least-loaded agent, ties broken round-robin, so one busy agent
        // never accumulates while an idle one starves.
        let loads: Vec<usize> = roster
            .iter()
            .map(|name| {
                self.tasks
                    .iter()
                    .filter(|t| t.holder.as_deref() == Some(name.as_str()) && t.is_leased(now_us))
                    .count()
            })
            .collect();
        let mut next = 0usize;
        for t in self.tasks.iter_mut() {
            if t.holder.is_some() {
                continue;
            }
            let mut best = 0usize;
            for i in 1..roster.len() {
                if loads[i] < loads[best] || (loads[i] == loads[best] && i == next) {
                    best = i;
                }
            }
            let holder = roster[best].clone();
            t.holder = Some(holder.clone());
            t.until = now_us + LEASE_US;
            events.push(format!("[event] task t{} assigned to {}", t.id, holder));
            messages.push((holder, t.assignment_message()));
            next = (next + 1) % roster.len();
        }

        (messages, events)
    }
}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- litter tasks tests ---\n");

    // [task] opens, tick assigns, [done: tN] closes — full life cycle
    total += 1;
    {
        let mut table = TaskTable::new();
        let roster = [String::from("sherlock"), String::from("hercules")];
        match table.note_message("root", "[task] audit main.rs", 100, true) {
            TableEvent::Noted => {}
            _ => libakuma::print("  [!] [task] should only note\n"),
        }
        let (msgs, events) = table.tick(&roster, 100);
        let assigned_ok = msgs.len() == 1 && msgs[0].0 == "sherlock" && msgs[0].1.contains("[assigned: t1]") && msgs[0].1.contains("audit main.rs");
        let event_ok = events.iter().any(|e| e.contains("t1 assigned to sherlock"));
        let done = table.note_message("sherlock", "[done: t1] found 3 issues", 200, true);
        let done_ok = matches!(done, TableEvent::Done(ref e, ref s) if e.contains("t1 done by sherlock") && s == "found 3 issues");
        if assigned_ok && event_ok && done_ok && table.is_empty() {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] lifecycle: assigned_ok={} event_ok={} done_ok={}\n", assigned_ok, event_ok, done_ok));
        }
    }

    // expired lease requeues and reassigns
    total += 1;
    {
        let mut table = TaskTable::new();
        let roster = [String::from("sherlock")];
        table.note_message("root", "[task] slow job", 100, true);
        table.tick(&roster, 100);
        let (msgs, events) = table.tick(&roster, 100 + LEASE_US + 1);
        let requeued = events.iter().any(|e| e.contains("requeued"));
        let reassigned = msgs.len() == 1 && msgs[0].0 == "sherlock" && msgs[0].1.contains("[assigned: t1]");
        if requeued && reassigned { passed += 1; }
        else { libakuma::print(&format!("  [!] requeue: requeued={} reassigned={}\n", requeued, reassigned)); }
    }

    // only the holder can close a task
    total += 1;
    {
        let mut table = TaskTable::new();
        let roster = [String::from("sherlock")];
        table.note_message("root", "[task] secret", 100, true);
        table.tick(&roster, 100);
        match table.note_message("hercules", "[done: t1] i did nothing", 150, true) {
            TableEvent::Noted | TableEvent::Requeued => {}
            TableEvent::Done(..) => libakuma::print("  [!] non-holder closed a task\n"),
        }
        match table.note_message("sherlock", "[done: t1] all clear", 160, true) {
            TableEvent::Done(..) => passed += 1,
            other => libakuma::print(&format!("  [!] holder's done was not applied: {:?}\n", other)),
        }
    }

    // load balancing: with two agents and two tasks, both get one
    total += 1;
    {
        let mut table = TaskTable::new();
        let roster = [String::from("a"), String::from("b")];
        table.note_message("root", "[task] one", 10, true);
        table.note_message("root", "[task] two", 10, true);
        let (msgs, _) = table.tick(&roster, 10);
        let holders: Vec<&str> = msgs.iter().map(|(to, _)| to.as_str()).collect();
        if holders.contains(&"a") && holders.contains(&"b") { passed += 1; }
        else { libakuma::print(&format!("  [!] balance: {:?}\n", holders)); }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
