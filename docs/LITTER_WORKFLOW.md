# Litter Workflow

Here is the updated flow for **v1**, framed around a custom message protocol running over **Raft** consensus (using Raft's log replication and index/block heights for state updates and compaction).

In this model:

1. **Everything is a Task:** When the Leader asks an agent for input, it creates an explicit sub-task assigned to (or claimable by) that specific agent.
2. **Leader Clears Tasks:** As sub-tasks finish, the Leader verifies them, marks them cleared, and folds the results into the ongoing state.
3. **Compaction & Final Artifact:** When context grows too large, the Leader issues an atomic compaction carrying forward open sub-tasks. Once all sub-tasks are resolved, the Leader produces the final artifact (the answer to the initial query) and clears the parent task.

---

### Diagram 1: Follower Agent Loop (Tama / Kuro)

This diagram shows how a Follower agent reacts strictly to state changes, claims assigned sub-tasks, executes local work, and returns responses.

```
       FOLLOWER AGENT (Tama / Kuro)                   RAFT LOG & SHARED STATE
──────────────────────────────────────────         ──────────────────────────────
                                                                  │
                                                                  │ 1. Leader posts Sub-Task
                                                                  │    Task #42.1 assigned to: Tama
                                                                  │    State: Pending
                                                                  │
┌────────────────────────────────────────┐                        │
│ Tama (Follower)                        │                        │
│ - Detects Sub-Task #42.1 for 'Tama'    │ 2. Msg: ClaimTask      │
│ - Sends claim to Raft Leader           ├───────────────────────►│
└───────────────────┬────────────────────┘                        │ 3. Raft Replicates Claim:
                    │                                             │    - Task #42.1 -> InProgress
                    │ 4. Ack: Claim Granted                       │    - Lease set to Raft Index + 50
                    │◄────────────────────────────────────────────┤
                    │                                             │
┌───────────────────┴────────────────────┐                        │
│ Tama (Follower)                        │                        │
│ - Executes local work (e.g. LLM / code)│                        │
│ - Produces sub-result                  │ 5. Msg: SubmitSubTask  │
│ - Sends result to Raft Leader          ├───────────────────────►│
└────────────────────────────────────────┘                        │ 6. Raft Replicates Result:
                                                                  │    - Task #42.1 -> AwaitingClearance

```

---

### Diagram 2: Leader Loop & Full Task Lifecycle (Mimi)

This diagram shows how the Leader (Mimi) breaks down queries into sub-tasks, monitors progress, compacts state across Raft log indices, clears completed sub-tasks, and delivers the final artifact.

```
         LEADER AGENT (Mimi)                          RAFT LOG & SHARED STATE
──────────────────────────────────────────         ──────────────────────────────
                                                                  │
                                                                  │ 1. Initial User Query Received
                                                                  │    Parent Task #42: "Debate & Report"
                                                                  │
┌────────────────────────────────────────┐                        │
│ Mimi (Leader)                          │                        │
│ - Breaks query into 2 directed tasks:  │ 2. Msg: CreateSubTasks │
│   Task #42.1 -> Assigned to Tama       ├───────────────────────►│
│   Task #42.2 -> Assigned to Kuro       │                        │ 3. Raft Index 100: Sub-tasks created
└───────────────────┬────────────────────┘                        │
                    │                                             │
                    │ [ Followers claim & work on Sub-Tasks ]     │
                    │ [ Shared state accumulates response logs ]  │
                    │                                             │
┌───────────────────┴────────────────────┐                        │
│ Mimi (Leader)                          │                        │
│ - Detects context growth threshold     │                        │
│ - Summarizes open debate off-chain     │                        │
│ - Bundles Summary + Active Sub-Tasks   │ 4. Msg: CompactState   │
└───────────────────┬────────────────────┘  (Index Cutoff: 150)   │
                    │────────────────────────────────────────────►│
                    │                                             │ 5. Raft Index 151 (Atomic Shift):
                    │                                             │    - Epoch 0 -> Epoch 1
                    │                                             │    - Discard raw logs < Index 150
                    │                                             │    - Store Base Summary
                    │ 6. State Event: EpochAdvanced               │    - Carry forward active sub-tasks
 All Agents Reset ◄─┤                                             │
 Local Context      │                                             │
                    │                                             │
┌───────────────────┴────────────────────┐                        │
│ Mimi (Leader)                          │                        │
│ - Inspects Sub-Task outputs            │ 7. Msg: ClearTask      │
│ - Verifies Kuro's finding & Tama's log ├───────────────────────►│
└───────────────────┬────────────────────┘                        │ 8. Raft Replicates Clearance:
                    │                                             │    - Task #42.1 & #42.2 -> Cleared
                    │                                             │
┌───────────────────┴────────────────────┐                        │
│ Mimi (Leader)                          │                        │
│ - Synthesizes cleared sub-task results │                        │
│ - Compiles final report artifact       │ 9. Msg: CompleteParent │
│ - Submits final answer                 ├───────────────────────►│
└────────────────────────────────────────┘                        │ 10. Raft Index 200:
                                                                  │     - Parent Task #42 -> Closed
                                                                  │     - Final Artifact Stored

```

---

### Protocol Execution Step-by-Step

#### 1. Task Delegation via Directed Sub-Tasks

* **Initial State:** User submits Parent Task `#42` (*"Debate if this codebase even works and produce a report"*).
* **Leader Action:** Mimi splits Parent Task `#42` into two explicit, assigned sub-tasks:
* `SubTask #42.1`: Assigned to **Tama** (*"Run build and tests, report stability"*).
* `SubTask #42.2`: Assigned to **Kuro** (*"Audit memory safety and concurrency, report bugs"*).


* Both sub-tasks are appended to the Raft log with `Status::Pending`.

#### 2. Follower Claim & Execution

* **Tama** and **Kuro** monitor the replicated Raft state. When they spot sub-tasks directed at their agent IDs, each sends a `ClaimTask` message to the Leader.
* Upon Raft consensus, the sub-tasks shift to `Status::InProgress` with an attached lease index.
* Each follower runs its local loop (LLM reasoning / tool calls) off-chain and posts a `SubmitSubTask` message containing its raw findings.

#### 3. State Compaction Across Raft Height

* As debate messages and intermediate tool logs accumulate, Mimi triggers an epoch boundary at Raft index `150`.
* Mimi submits a single atomic `CompactState` transaction containing:
* `cutoff_index: 150`
* `summary`: *"Tama confirmed build passes; Kuro found an unhandled lock in memory.rs."*
* `carried_over_tasks`: `[SubTask #42.1 (AwaitingClearance), SubTask #42.2 (AwaitingClearance)]`


* The Raft state machine updates atomically: logs prior to index `150` are dropped from active memory, and all agents reset their local prompt windows to the newly committed summary baseline.

#### 4. Leader Clearance & Artifact Generation

* **Clearance:** Mimi reviews the submitted findings from Tama and Kuro. Satisfied with the coverage, Mimi submits `ClearTask { task_id: 42.1 }` and `ClearTask { task_id: 42.2 }`. The state machine updates both sub-tasks to `Cleared`.
* **Final Synthesis:** With all sub-tasks cleared, Mimi synthesizes the final artifact off-chain and submits `CompleteParent`:
```rust
CompleteParent {
    parent_task_id: 42,
    artifact: "FINAL REPORT: Codebase compiles and passes unit tests, but fails under concurrent load due to a race condition in memory.rs."
}

```


* The Raft cluster commits the transaction, closing Parent Task `#42` and delivering the final artifact result.

---

## Implementation: how the protocol above is spelled on the wire

Status: design of record for the `tasks.rs` rewrite (2026-09-20). The
diagrams above are the contract; this section is how it is actually
encoded, and where we knowingly diverge.

### Why typed records and not body prefixes

Task traffic is its own request — `Request::Task { from, op }`, protocol
**v4** — not a bracket prefix on a chat body. An earlier draft of this
document specified `[submit: t1.2] …` prefixes; that was wrong, and the
reasons are worth keeping:

- A model has to *spell* a prefix correctly, every time. A tool call
  arrives already parsed, with its arguments in fields.
- Authority has to be checkable on the call. With prefixes, "only the
  leader may clear" is enforced against a string scraped out of prose an
  LLM wrote, and any agent can type the string.
- A record is applied and replicated. Chat is neither.

The cost is a protocol break: a v3 hub treats a v4 task record as an
unknown op. Everything in a yard rebuilds together, so this is cheap
today and deliberately paid now rather than later.

Authority is still stamped hub-side from `from` (`Root` for the operator,
`Leader` for whoever holds the socket, `Peer` otherwise). That remains a
name the sender chooses — see "Future work".

### The model-facing surface: two tools

| Tool | Class | What it is |
|------|-------|-----------|
| `SendMessage(to, body)` | public | Say something. `to` is an agent, or `litter` for everyone. |
| `TaskUpdate(task, status, text)` | public | Every per-sub-task act, as a `status`: `claim`, `done`, `failed`, `clear`, `reopen`, `artifact`. |
| `TaskPlan(task, assignments[])` | public | Leader only. Splits a parent into directed sub-tasks, atomically. |
| `ListPeers()` | local | A read. Leaves no record. |

`TaskUpdate` is one tool with a `status` enum rather than five
near-identical tools: the acts share their arguments, a small model picks
a *value* more reliably than it picks among similar tool names, and a new
act costs a value instead of new surface — `failed` was the first, and
proved it.

**`ReadInbox` is gone.** Delivery is automatic (see "Auto-feed" below), so
a tool for it would only let a model spend a call re-reading its own
prompt.

### The acts

| Workflow step      | Call                                                      | From          | Transition |
|--------------------|-----------------------------------------------------------|---------------|------------|
| Parent task        | `TaskUpdate(status="open", text=…)` (operator: `meow litter task`) | root, leader | new parent `tN` |
| `CreateSubTasks`   | `TaskPlan(task="tN", assignments=[{who,what},…])`          | leader        | parent → planned; subtasks `tN.1…` `Pending` |
| *(offer)*          | `[assigned: tN.M] …` delivered to the assignee            | the table     | claim window opens |
| `ClaimTask`        | `TaskUpdate(task="tN.M", status="claim")`                 | the assignee  | `Pending` → `InProgress`, lease set |
| `SubmitSubTask`    | `TaskUpdate(task="tN.M", status="done", text=…)`          | the assignee  | → `AwaitingClearance` |
| *(cannot do it)*   | `TaskUpdate(task="tN.M", status="failed", text=why)`      | the assignee  | → `AwaitingClearance`, result marked `[FAILED]` |
| `ClearTask`        | `TaskUpdate(task="tN.M", status="clear")`                 | leader        | → `Cleared` |
| *(reject)*         | `TaskUpdate(task="tN.M", status="reopen", text=why)`      | leader        | → `Pending` |
| `CompleteParent`   | `TaskUpdate(task="tN", status="artifact", text=report)`   | leader        | all `Cleared` → parent closed |

`TaskPlan` carries **every** sub-task in one call, matching the diagram's
single `CreateSubTasks` entry. Without that the table could never know
planning had finished, so "all sub-tasks cleared" — the trigger for the
final artifact — would never be decidable.

`failed` is a sibling of `done`, not a flavour of it: both land in
`AwaitingClearance`, differing in the stored result, because whether to
retry, reassign or accept a failure is the leader's decision and not the
table's.

### Applying a record is typed, not sniffed

`TaskTable::apply` returns `Applied = Result<String, String>` — `Ok(note)`
means state changed, `Err(why)` means nothing happened — and the wire
carries it as `applied: bool` beside the note.

Three separate places used to re-derive acceptance by testing whether the
note *began with the word "refused"*: the hub deciding whether to
replicate the record, the tool deciding ok-vs-error, and the wire, which
did not carry the distinction at all. Any of them could disagree with the
state machine, and the "already claimed" refusals disagreed with all
three — they never said "refused", so a no-op would have been replicated
to the whole litter as though it had happened.

### Records are replicated

An **accepted** record is broadcast to every roster member as a `[record]`
line — `[record] kuro done t1.2: one lock is unheld` — so the litter's
picture of who is doing what is maintained by the protocol rather than by
agents remembering to narrate themselves. Refused records are not
replicated: nothing happened, so there is nothing to replicate.

The broadcast copy is `Done`-kind (non-waking). Every agent *sees* every
record; a record is not an instruction to anyone, and waking four agents
per record turns one task into sixteen LLM turns. The *targeted* traffic —
offers and leader directives — is `Assignment`-kind and does wake.

### Directives: the table tells the leader what to type

A 0.8B model will not infer `[artifact: t1]` from a design document. So
every point where the workflow needs a leader decision, the table
*delivers the leader a message naming the exact verb*, the same way
assignments are already delivered to workers:

- `[plan-needed: tN]` — the parent's text plus the current roster.
- `[clearance-needed: tN]` — each submitted result awaiting verification.
- `[artifact-needed: tN]` — every cleared result, ready to synthesize.

These are re-sent on a nag interval rather than once, because a dropped
directive would otherwise stall a parent task permanently.

### Timers

| Name              | Value | What it protects |
|-------------------|-------|------------------|
| `CLAIM_WINDOW_US` | 180 s | An offer nobody claimed is re-offered (assignee busy or gone). |
| `LEASE_US`        | 900 s | A claimed sub-task whose worker died is requeued. Generous: an LLM turn is minutes. |
| `NAG_US`          | 120 s | How often an outstanding leader directive is repeated. |

### Divergences from the diagrams, deliberately

1. **No Raft log, no index cutoff.** Ordering is the hub's event log and
   its epoch; compaction is the marker described in
   `LITTER_STATE_MACHINE.md`, not a `cutoff_index`. Same guarantees for a
   single-socket litter, far less machinery.
2. **A submit without a claim is accepted** as claim-then-submit. The
   handshake is in the protocol and followers are told to use it, but a
   small model that skips straight to the result should not have that work
   thrown away — losing the ceremony is cheaper than losing the answer.
3. **`EpochAdvanced` does not reset local context.** There is nothing to
   reset: `run_turn` builds a fresh `Conversation` for every wake, so an
   agent's window is already per-turn. Carry-forward happens through the
   compaction marker the next turn reads.
4. **Task state remains leader memory**, as before. Compaction now folds
   the *open sub-task list* into the marker, so a leader's death loses the
   table but not the knowledge of what was outstanding.

### Lock discipline (why this layer adds none)

`TaskTable` is a pure state machine: `&mut self`, `now_us` always a
parameter, no clock, no socket, no interior mutability. It introduces **no
new synchronization** — it runs inside the one `PMutex<HubState>` that
already exists, and is fully unit-testable without either thread.

That lock has one rule, learned the hard way on 2026-09-20: **it must
never be held across I/O.** `serve::drain` used to hold it across
`try_accept` and across each connection's deadline-bounded read/write; the
deadline poll hook calls back into `local_drain`, which re-locked it, and
`PMutex` is not reentrant. Both leader threads parked in `FUTEX_WAIT` on
the same word at the same PC, with the lock left at 1 and every thread
that could clear it asleep — unrecoverable, and indistinguishable from
outside from three unrelated faults ("went silent before answering").

The rule is therefore structural, not a comment: read the request frame
with no lock, take the lock only for `handle()` (a pure transition,
microseconds), drop it, then write the response with no lock. Callbacks
that might run inside a critical section use `PMutex::try_lock` and skip —
a lock you cannot take means someone is already serving.

### Future work

**Signature verification and address recovery on every message.** Today
only *relayed* traffic is verified: `handle`'s relay branch checks the
origin signature and the relayer signature against a canonical payload
(`sig.rs`), but a locally-originated `Send` is trusted on its `from` field
alone — an agent can claim to be any name the roster knows, and the hub
stamps authority (`Root`/`Leader`/`Peer`) from that claimed name. With
public tool calls becoming raft records, that is the wrong place to stop:
a record is replicated and acted on, so "who signed this" has to be
decidable from the record itself.

What is wanted is **address recovery**, not a name lookup: derive the
sender's identity *from the signature over the payload*, so the identity
is a consequence of the signature rather than an assertion the signature
happens to sit beside. Same for the relayer on a forwarded record. Then:

- `from` stops being an input and becomes a recovered output;
- authority (may this sender open a parent task? clear a sub-task?) is
  checked against a recovered address, not a string;
- a forged `from` is not a policy failure, it is a signature that does not
  recover to anyone in the roster.

The pieces already exist — per-agent keys are generated at first run and
`sig::verify_payload` is in the tree — so this is wiring plus a canonical
payload for each public record type, not new cryptography.

And the reason this matters beyond tidiness: **root is the key to the cat
house.** Opening a parent task — giving the litter something to talk about
— is the one privileged act in the protocol, and today it is authorized by
a string: `yard.sh talk/task` runs `meow litter send --from root`, so
anyone who can reach the hub socket can claim to be the operator and set
the whole litter working. Once identity is recovered from a signature,
"root" stops being a name and becomes possession of the operator key, and
the authority check in `Authority::may_open_parent` is checking something
real. The intended key is the operator's existing sshd `authorized_keys`
identity (`LITTER_RAFT_LOOP.md` future work) — no new secret to manage.

---

## The end goal

Recorded 2026-09-20, from the design discussion that drove the `HubState`
split. This is the target the code is being moved toward, not a
description of what it does today; the "Where this stands" table at the
end says which parts have landed. The diagrams at the top of this file
still show the Raft-flavoured framing and need redrawing against this.

### The analogy

**A hand-rolled Tendermint with futures bolted onto it.** That is the
shortest accurate description, and it is worth keeping because it makes
the right predictions:

- There is an **ordered record** and a **deterministic application** that
  the record drives. `record.rs` is the log (term, leader, epoch);
  `tasks.rs` is the application. Application is pure — no I/O, `now_us`
  passed in — which is exactly why the same record applied to the same
  state gives the same answer anywhere. That property is the only reason
  a log is worth replicating.
- There is a **peer layer** that knows nothing about meaning:
  `membership.rs`, moving bytes to mailboxes.
- **Public tool calls are transactions**; local tool calls are queries.
  One goes into the record and fans out to everyone, the other is a read
  that leaves no trace.
- **Compaction is state sync**: prune below a height, keep a summary,
  carry the still-open work across.
- The **futures** are the tasks. Each is submitted asynchronously, runs
  while events stream into the agent holding it, and resolves only on an
  explicit completion.

### Ownership: single owner, no locks

Communication is async request/response. An inbound thread parses a frame
and pushes the result onto the target agent's queue; responses travel back
the same way. **Nothing is shared, so there is nothing to lock.**

```
   inbound ──parse──► [ queue: agent A ] ──► owner loop ──► [ queue: out ] ──► outbound
   inbound ──parse──► [ queue: agent B ] ──►    │
                                                └─ owns Membership + Record + RelayPlane
                                                   (by value; no Arc, no PMutex)
```

The mutex that exists today is an artifact, not a requirement. `handle()`
is already a pure `Request -> Response` with no I/O in it; what forced a
lock was that *two* threads drive it — the agent loop and the raft thread
both call `drain()`. That in turn came from Phase 2 bug 1, where the
leader polled its own inbox over loopback while being the only thread that
could answer, and self-deadlocked. Single ownership solves that bug
without a lock: the agent loop stops being a second driver and submits
through an in-process queue like anyone else, instead of talking to itself
over a socket.

The one real cost: on targets where the second thread cannot spawn
(amd64 — `live.rs` already handles `raft thread DOWN`), a single owner
means the hub is unserved while an LLM turn runs. Today's dual-drain plus
poll-hook exists to paper over exactly that, and it is what produced both
the permanent deadlock and the stack-recursion hazard it was replaced
with. Paying that cost honestly is better than paying it in wedges.

### The tool surface: public records vs local queries

What the model can do is a **tool surface**, and it splits in two:

| Class | Goes in the record? | Examples |
|-------|--------------------|----------|
| **Public** | yes — replicated, fanned out, acted on | send a message, mark a task done; later mark a task **failed** |
| **Local** | no — a read, no trace | list peers, inspect state |

This replaces the bracket-prefix protocol documented in "Implementation"
above (`[submit: t1.2] …`). Typed tool calls are strictly better here:
a 0.8B model does not have to spell a bracket syntax correctly, arguments
arrive already parsed, and authority is checked on the *call* rather than
on a string scraped out of a chat body. The wire records those calls
produce are what the diagrams above call messages.

`mark failed` is not decoration — it is the rejected branch of the future,
and without it a task that cannot be done is indistinguishable from one
still being worked on.

### Auto-feed, explicit completion — the throughput lever

Two halves, and the asymmetry between them is the whole point:

- **Inbound messages and events are fed into the agent and the LLM
  automatically.** There is no "read your inbox" call to make; delivery is
  not something the model has to remember to do.
- **Finishing a task requires a specific, deliberate mark.** A task stays
  open until its holder says otherwise.

Together these are what produce throughput. An agent absorbs many events
inside one working context and only stops when it decides it is done,
instead of round-tripping a whole turn per message and re-deciding what it
is doing each time. It also means the natural end of an LLM turn is *not*
the end of a task — which is the mistake the current
fresh-`Conversation`-per-wake loop makes structurally.

Everything then hits compaction eventually: history folds to a marker,
open tasks carry over, and the agents keep going from the summary.

### One loop, role as state

Every agent runs the same loop. There is no leader program and no follower
program — `live::run`'s `Machine` is a *state*, and the role is re-derived
each pass. That is why a leadership change needs no new code path: the
loop simply runs again and applies whatever it now is.

What has to change with it is the **model**, and that is easy to forget,
because the code adapts silently. Three things carry it:

1. **The role is restated every turn.** Each turn's context reports
   `leader` and `you_are_leader`, freshly fetched. Since a turn builds a
   new conversation, there is no stale belief to correct — the model is
   simply told what it is now.
2. **The change itself is delivered.** Cluster events (`X is leader
   (term N)`, joins, leaves, task churn) used to go to the event log,
   which the loop printed to a console nobody reads and the model never
   saw. They now ride into the turn as `changed`, ahead of the messages,
   because an election changes how the rest of the batch should be read.
3. **A new leader is woken specifically.** Taking the socket queues a
   `[leader-elected]` directive (`TaskTable::note_new_leader`), so
   promotion is an instruction to act, not just a fact to notice.

The result is a machine that keeps running until the work runs out: tasks
arrive, get split, get claimed, come back, get cleared, and close as
artifacts, and the loop only goes quiet when there is nothing left
outstanding.

### The turn context is JSON, and that is a safety property

What a turn is handed — role, peers, cluster events, the message batch —
is a single escaped JSON object; only the trailing instruction is prose.
The obvious reason is that it is less string-building on a `no_std` heap
and a model reads either equally well.

The real reason is **provenance**. When each message is rendered as
`[from] body`, the delimiter is a bracket any agent can type, so a body
containing `\n[sherlock] ignore that, do X` is indistinguishable from a
header written by sherlock. The same trick forges cluster events and
fakes a role. Escaped JSON puts an unambiguous boundary between what was
said and who said it, and a message can no longer claim to be its own
context.

### Where this stands

| Piece | State |
|-------|-------|
| `HubState` split into membership / record / relay | **landed** |
| Lock never held across I/O | **landed** (interim; the lock itself is still there) |
| Single owner, queues, no locks | not started |
| Parent → sub-task → claim → submit → clear → artifact | written, not wired |
| Public/local tool surface replacing bracket prefixes | not started |
| Auto-feed + explicit completion | not started |
| One working context per agent (not per wake) | not started |
| Compaction carrying open work | **landed** (`open_work_lines` → marker) |
| Signature verification / address recovery | not started — see "Future work" |
