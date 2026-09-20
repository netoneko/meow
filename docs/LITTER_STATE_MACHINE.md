# Litter agent state machine

Status: implementation-accurate as of 2026-09-20 (live-found bugs absorbed;
see `docs/LITTER_EXPERIMENT_PHASE_2.md`). Companion docs:
`LITTER_RAFT_LOOP.md` (message-level view: wire frames + tool calls, thread
wiring). This doc is the agent's control flow; the code, tests and scripts
all use these words.

## Framing: what this is NOT

This is **not a general-purpose coordination service** and we are not
reinventing etcd. It is a very contained protocol for a handful of
cooperative agents that already share one trust domain (one network, one
operator). No quorum of arbitrary nodes, no log replication. It is also
**purely network-bound**: the filesystem is not a transport, not a fallback
channel, and not a state store. The hub socket is the only channel; state
lives in process memory; the knowledge that should survive a leader dying
lives distributed in the agents' own contexts and in protocol history up to
the last compaction marker.

## The states

One process (`meow litter live`), **two threads** (raw-clone trampoline,
NOT pthreads — meow's raw `_start` never initializes musl's thread
runtime, so `pthread_create` fails; see `src/rt.rs` and the wiring notes
in `LITTER_RAFT_LOOP.md`): the main thread is the agent loop (poll → wake
→ LLM turn, may stall minutes); a second thread runs the raft/serve duties
(drain, heartbeats, task leases, compaction, votes) and is never blocked by
inference. Agent-loop decisions happen between ticks; the raft thread ticks
straight through any turn.

```
                             STARTUP
            (config, Join, paged History pull, bind race)
                 │ wins bind             │ bind fails (hub is up)
                 ▼                       ▼
      ┌────────────────┐  Peers answered ┌────────────────┐
 ────►│     LEADER     │◄────────────────│    FOLLOWER    │◄─┐
      │ raft thread on │────────────────►│                │  │
      │  own socket    └────────────────┘  probe = ping   └──┤
      └────────────────┘                     │             │
        │                                      │             │
        │ dies                                 │ no answer   │
        │ (socket frees)                       │ for HEARTBEAT_TIMEOUT (~15s)
        │                                      ▼             │
        │                        ┌─────────────────────────┐  │
        │  re-race bind          │        WAYWARD          │  │
        │  succeeds → LEADER     │ hub calls stopped,      │  │
        └─────────────────────────│ tools fail fast,        │  │
                   ▲              │ cached roster only      │  │
                   │              └─────────────────────────┘  │
                   │ hub answers again: re-Join, resume hub    │
                   └───────────────────────────────────────────┘

      Leadership only ever changes through the bind race: the OS
      deciding who owns the socket IS the election. One socket, so
      the swarm can never split into two hubs.
```

Because the raft thread never stalls, `HEARTBEAT_TIMEOUT` is short (~15s):
leader silence means dead or partitioned, never "thinking". WAYWARD is
therefore rare and brief.

### LEADER

The process that won the bind race runs the raft thread: roster, inboxes,
compaction marker and the task table live in its `Mutex<HubState>`
(`serve.rs`), purely in memory.

Raft-thread tick (~1s):
- `drain()`: serve every framed request waiting in the listen backlog
  (Join / Inbox / Send / History / Peers / Vote) — agents mid-turn never
  delay the hub.
- heartbeat tick: term bookkeeping, heartbeat observation for peers,
  `RequestVote` answered in-place by the state machine (`litter-raft`) —
  never by an LLM.
- task tick (~5s): task-table duties — assign pending tasks to roster
  members round-robin (lease stamped `holder`/`until`), requeue leases
  past expiry (crashed worker), fold `[done]` replies into the compaction
  marker.
- compact tick (~30s): prune each inbox past `KEEP_RECENT` recent messages
  and keep exactly one `"[compacted: …]"` marker message summarizing what
  was folded.

Death is the only exit. State dies with the process — by design. What
survives: protocol history up to the marker (each agent re-pulls it), the
roster (everyone re-`Join`s, idempotently), and each agent's own context.

### FOLLOWER

Ordinary hub client, exactly like a one-shot `meow` invocation, plus a
pulse:

- every `PULSE_TICKS` (~5s): send `Peers`. **The round-trip IS the
  heartbeat observation** — no lease, no expiry bookkeeping; we only want
  to know the leader answered *something* in the last X time. A successful
  probe refreshes the in-memory roster cache.
- every tick: count own inbox; growth → run a turn (persona + tools, same
  as `litter chase`, woken by inbox activity instead of a CLI).

Bootstrap is self-serve and needs no outside activation: at startup the
agent `Join`s and pages its own `History` back in batches until it hits
the `"[compacted: …]"` marker (or history ends). Nobody pushes anything at
a joining member — it discovers compaction and relevant history through
the protocol itself. The same two calls re-register and re-source every
survivor automatically after a leadership change (Join is idempotent).

### WAYWARD

Entered when no `Peers` answer has arrived for `HEARTBEAT_TIMEOUT` (~15s —
short now, because the raft thread cannot stall on inference; silence
means the leader process is dead or partitioned).

WAYWARD agents:
- stop issuing hub calls; their Litter* tools fail fast with a clear
  "hub unresponsive (last heartbeat Ns ago)" instead of hanging;
- keep the **cached roster**, so they still know their peers;
- keep probing and **re-race the bind** every tick. Two exits:
  - **hub answers again** (old leader still alive, e.g. network blip):
    re-`Join`, re-pull history since the marker, resume FOLLOWER;
  - **bind succeeds** (leader died, socket freed): this process becomes
    LEADER; every other agent's next probe is answered and re-registers it.

One socket means the swarm cannot split into two hubs, and the bind race
cannot deadlock — kernel arbitration.

## Heartbeats, Raft, and what's deliberately missing

- Liveness = "hub answered within X": Raft's heartbeat *observation*, none
  of its ceremony; the leader doesn't even know it's being pinged.
- Leadership = socket ownership; the bind race is the election.
- `litter-raft` (terms, one-vote-per-term, step-down on higher term,
  majority threshold, `is_authorized(sender, root)`) is built and tested;
  the raft thread consumes it for term/vote bookkeeping. The root
  identity (operator messages) always outranks whoever is leader.
- Task leases (`holder`/`until`, in-memory) are the only timers: worker
  liveness, not leadership liveness. An expired lease requeues a task,
  nothing more. The task table is leader memory; if the leader dies
  mid-queue, unfinished tasks are re-posted by whoever remembers them.

## The states as a table

| State    | Hub socket     | Messaging              | Peer knowledge   | Exits on |
|----------|----------------|------------------------|------------------|----------|
| LEADER   | bound          | serves in-process (raft thread) | authoritative | process death (only) |
| FOLLOWER | absent         | hub client calls       | `Peers` + cache  | probe stale → WAYWARD |
| WAYWARD  | held by dead/hung leader | none — fail fast | cached roster | hub answers → FOLLOWER; bind win → LEADER |

Orthogonal "busy" bit: the agent thread can be mid-turn in any state;
turns delay polls, never the raft thread.
