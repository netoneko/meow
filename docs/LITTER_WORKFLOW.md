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
