# Litter raft loop: messages and tool calls that drive the swarm

Status: built and live-verified (2026-09-20); companion to
`docs/LITTER_STATE_MACHINE.md` (agent-side states). This doc is the
*message-level* view: which wire messages and LLM tool calls flow, in what
order, and which process drives each. Read that one first for the states;
this one for the traffic.

## Process view: two threads, one process

The agent is a **single process** (`meow litter live`) with two threads.
Built and live-verified (2026-09-20) — and the threads are NOT musl
pthreads: meow's raw `_start` never initializes musl's thread runtime, so
`pthread_create` fails. `src/rt.rs` does what
`userspace/amd64/threadprobe` does: raw `clone(CLONE_VM|…|CLONE_THREAD)`
with a per-arch assembly trampoline (aarch64 + x86_64, cfg-gated; child
never returns into Rust; `exit`-not-`exit_group`), and the shared state
sits behind one futex-word mutex (`rt::PMutex`, Drop-guard unlock).
Live-found bugs this encoding absorbed: loopback self-deadlock, blocking
`try_accept`, the self-silencing staleness gate, CLONE_SETTLS EINVAL,
unconditional aarch64 asm — see `docs/LITTER_EXPERIMENT_PHASE_2.md`.

**`CLONE_SETTLS` is per-target, because the two targets refuse the
opposite thing** (2026-09-20). Linux refuses the flag with a NULL tls
(`EINVAL`) — that is the "CLONE_SETTLS EINVAL" above, and why the flag was
dropped. Akuma/amd64 refuses its *absence* (`amd64/src/thread.rs`), so the
same flag word that works on Linux cannot create a thread there at all:
`spawn_detached` returned false on the Firecracker guest and every leader
ran with **no raft thread**, which is not a degraded mode but a broken one
(see "When thread 1 does not exist" below). So x86_64 now passes
`CLONE_SETTLS` plus a real TCB — the shape musl's `pthread_create` already
uses — while aarch64 keeps the flag off. The child is still TLS-free by
construction either way: nothing in raft-thread code reads the thread
pointer, the block merely has to exist.

```
┌─────────────────────────── meow litter live (one process) ─────────────────────────────┐
│                                                                                       │
│  THREAD 1: RAFT/SERVE  (spawned at startup — only if this process won the bind race)  │
│  ┌─────────────────────────────────────────┐                                          │
│  │ never blocks on inference, 1s tick:     │      lock      ┌───────────────────────┐ │
│  │  · drain(): accept backlog frames,      │───────────────►│  SHARED STATE         │ │
│  │    serve Join/Inbox/Send/History/       │                │  Mutex<HubState>      │ │
│  │    Peers/Vote one-shot connections      │───────────────►│  · roster             │ │
│  │  · raft tick: bump term, broadcast      │───────────────►│  · inboxes            │ │
│  │    Heartbeat{term} to peers' control    │                │  · snapshot +         │ │
│  │    inboxes (tick-level, never LLM)      │                │    compaction marker  │ │
│  │  · task tick: assign pending tasks      │───────────────►│  · task table         │ │
│  │    (lease), requeue expired, fold       │                │    (holder/until)     │ │
│  │    [done] replies, prune → marker       │                │  · term + vote state  │ │
│  │  · answer RequestVote frames in-place   │                │    (litter-raft)      │ │
│  └─────────────────────────────────────────┘                └───────────▲───────────┘
│                                                                         │ lock        │
│  THREAD 2: AGENT LOOP  (main thread — may stall minutes in a turn)      │             │
│  ┌─────────────────────────────────────────────────────────┐            │             │
│  │ 1s tick:                                                │            │             │
│  │   · pulse: Peers probe → round-trip = heartbeat observed│            │             │
│  │   · inbox count (direct lock if leader,                 │────────────┘             │
│  │     TCP client if another process holds the socket)     │                          │
│  │   · growth → wake → chat_once TURN (minutes; thread 1                   │
│  │     keeps serving heartbeats/votes untouched — this is the              │
│  │     "consensus decoupled from model workers" guarantee)                 │
│  │                                                                         │
│  │ inside the turn, the LLM's tool calls:                                  │
│  │   ListPeers  ──► lock state (leader) / client Peers (follower)          │
│  │   ReadInbox  ──► lock state / client Inbox  (paged batches until        │
│  │                  the "[compacted: …]" marker on cold start)             │
│  │   SendMessage► lock state / client Send    (to:"litter" = fan-out)      │
│  │   FileRead / FileWrite ──► local filesystem (scratch, reports,          │
│  │                  /akuma-src) — never a transport                        │
│  └─────────────────────────────────────────────────────────────────────────┘
└─────────────────────────────────────────────────────────────────────────────────────────┘
        │                                                    ▲
        │ if bind race LOST, thread 1 never starts:          │ every follower can
        │ this process is a pure client                      │ become the binder: if
        ▼                                                    │ heartbeats go stale past
   TCP to whoever owns the socket ◄──────────────────────────┘ HEARTBEAT_TIMEOUT, its
   (socket owner = the election; one socket, one hub, no split)  tick re-races the bind
```

Key consequences of the split:

- **Heartbeats survive turns.** Thread 1 answers `Peers` probes and vote
  frames while thread 2 is minutes deep in an LLM call. `HEARTBEAT_TIMEOUT`
  can be short (~15s) because leader silence now means *dead*, not
  *thinking* — which is the concrete version of "consensus networking
  runs strictly decoupled from model worker tasks".
- **No hub process, no hub thread-of-its-own.** The coordinator role is a
  thread of whichever agent won the bind race. If that whole *process*
  dies, its socket dies with it, and every other agent's next re-race
  spawns its own thread-1. Herd / the micro-VM supervisor just restarts
  the one process; the role comes back with it.
- **One lock, coarse on purpose.** `Mutex<HubState>` is held for
  microseconds per frame; a litter is a handful of agents, contention is
  nil. No channels, no lock-free anything.
- **Nothing does network I/O while holding that lock.** "Microseconds per
  frame" is the whole basis of the coarse lock, and a single remote round
  trip inside the critical section invalidates it. `relay_send` was
  written this way from the start; `probe_static_peers` was not, and did a
  cross-host TCP round trip **per peer** under the lock. Two litters
  pointed at each other then deadlock in slow motion — our probe waits on
  their hub, which is locked waiting on its probe of ours — and the
  symptom looks exactly like a leader gone deaf in an LLM turn while
  having nothing to do with the LLM. It only appears once a static peer is
  configured. Split into `probe_peers_io` (no locks) and
  `apply_probe_results` (locks, no I/O), 2026-09-20. **Before adding
  anything to the raft tick, check which side of that line it falls on.**
- **The leader reads its own inbox through the lock, never through a
  socket.** The diagram has always said "direct lock if leader"; the code
  did not, and always went through `hub::inbox_messages`. With thread 1
  alive nobody notices, because thread 1 answers. Without it the leader
  blocks waiting for a reply only it could have sent, times out every
  tick, and reads its own inbox as permanently empty — so it never wakes,
  never runs a turn, and never serves anyone. Now `local_inbox_count`
  (2026-09-20), which is what the diagram always described.

### When thread 1 does not exist

If `spawn_detached` fails, this design does not degrade — it stops. The
hub is served *only* from thread 1, so a leader without it accepts
nothing: the listen backlog fills and the hub goes from slow to actively
refusing connections, while the leader itself sits in a turn. Followers
then go WAYWARD, re-race the bind, and fail because the leader still holds
the socket.

There is no fallback, deliberately. A "pump" that drains the listener from
inside the model's streaming loop was tried on 2026-09-20 and removed the
same day: it works (the hub went from refusing connections to 44/45
reachable), but it puts hub-serving inside the model call, which is
precisely the coupling this whole split exists to prevent. The supported
answer is that the thread must spawn. `live::start_raft_thread` therefore
logs a loud warning and a `raft thread spawn FAILED` line to the agent's
`raft.log` rather than quietly carrying on.

The old standalone `litter-hub` binary stays only as a debugging tool;
the swarm no longer needs any separately-launched process.

## Bootstrap (agent joins, self-serve)

```
 agent                                   coordinator                    wire messages
   │  connect                                   │
   │─────────Join{name:"sherlock"}─────────────►│  Join (idempotent; adds to roster)
   │◄────────Joined─────────────────────────────┤
   │  page history back in batches until the    │
   │  last compaction marker:                   │
   │─────────History{name, before:now, limit:32}►│
   │◄────────Inbox{batch}───────────────────────┤   newest → oldest
   │─────────History{name, before:oldest_ts}────►│
   │◄────────Inbox{batch}───────────────────────┤
   │     ... until batch contains the marker    │
   │     "[compacted: <summary>]" or history    │
   │     is exhausted — never reads past it     │
   │                                            │
   │  (no Snapshot blob, no pushed welcome:     │
   │   history is sourced from the protocol     │
   │   itself, in batches, up to the marker)    │
```

Nothing activates a joining agent from the outside: Join + paged History
are ordinary requests it makes on its own. The compaction marker is an
ordinary message sitting at the top of the pruned history — `History`
reads stop there, so a fresh agent's context gets exactly the relevant
tail and one summary line, nothing older.

## The agent tick (worker loop)

```
 every 1s tick (agent, ATTACHED):
   ┌─ inbox poll ────► Inbox{name:"me"}          (cheap count; wakes a turn on growth)
   └─ every PULSE_TICKS ─► Peers                 (round-trip = heartbeat observation;
                                                  response refreshes roster cache)

 every 1s tick (coordinator):
   ┌─ drain() backlog: serve Join/Inbox/Send/History/Peers/Vote frames
   ├─ every TASK_TICKS: assign pending tasks round-robin (lease stamp),
   │                    requeue expired leases, fold [done] replies
   ├─ every PULSE_TICKS: broadcast Heartbeat{term} to roster   (raft tick)
   └─ every COMPACT_TICKS: prune history past the recent tail,
                          write "[compacted: ...]" marker message

 WAYWARD (agent saw no answer for HEARTBEAT_TIMEOUT):
   └─ every tick: try connect; if dead → spawn serve child; if stale-but-bound
      (hung coordinator) → keep failing tools fast, keep probing
```

## Status changes & election notification

Agents are *always* told when the cluster shape or leadership changes —
through the same probe they already make, never by a separate push
connection:

```
 coordinator                                          agent (tick loop)
      │                                                     │
      │  election happened / peer joined / peer left        │
      │  → append to in-memory event log, bump epoch        │
      │                                                     │
      │◄────────────Peers{since: <my epoch>}────────────────│  (the regular pulse)
      │─────────────Peers{names, term, leader,              │
      │                   epoch, events: [ … new since … ]}─►│
      │                                                     │ diff vs cache:
      │                                                     │  · refresh roster cache
      │                                                     │  · record term/leader
      │                                                     │  · queue new events for
      │                                                     │    the AGENT thread
      │                                                     │
      │                                          agent thread, next wake (or a
      │                                          standing preamble even with no
      │                                          inbox growth — events DO reach
      │                                          the model, but they don't wake
      │                                          a turn by themselves):
      │                                            "cluster events since your last
      │                                             turn: [event] hercules elected
      │                                             leader (term 4); ressler left"
```

- Events are protocol state (an in-memory log with a monotonically
  increasing epoch), not chat messages: they are consumed at tick level and
  surfaced to the model as context. They never wake a turn on their own —
  an election is worth *knowing*, not worth *talking about*.
- `Peers` doubles as the heartbeat AND the change feed: one request, three
  jobs (liveness, roster, events). On a successful probe after WAYWARD the
  since-cursor replay is exactly how a survivor learns "an election
  occurred while I was blind".
- The coordinator itself learns of elections the direct way: winning the
  bind race IS taking the term (it increments the persisted term and
  announces itself in the first heartbeat window). Vote exchange
  (`RequestVote`/`VoteResponse` through the relay) is the term-safety
  layer on top, driven entirely by raft threads.

## Future work (noted, not built)

- **Task tracking as a Raft log.** The in-memory task table maps naturally
  onto the raft protocol: tasks as appended log entries, assignment/lease
  renewal/requeue/completion as entries too — then the table replicates to
  followers for free and survives leadership changes the way raft state
  does, instead of dying with the leader's process. Same shape for the
  cluster event log.
- **Self-issued tasks.** An agent should be able to assign a task to
  itself — write it into its own table (or a local equivalent) with the
  same lease/`[done]` bookkeeping — without messaging anyone. Self-directed
  work with the same accountability as swarm-assigned work; nothing in the
  table's design assumes the issuer is another agent.
- **Signed envelopes + the embedded root key.** Half built (2026-09-20).
  **Built:** ed25519 signatures on the *cross-litter relay* envelope —
  each agent has its own key (`litter_key`), signs the messages it says
  (`sig`) and the hops its hub carries (`rs`), and the receiving hub
  verifies both. See `docs/LITTER_RELAY_TOPOLOGY.md`.
  **Not built:** litter-*local* `Send`/`Join` are still unsigned, so
  `litter-raft`'s `is_authorized(sender, root)` still takes `sender` as an
  unauthenticated string — the root role remains goodwill plus the hub
  stamping it at delivery. The operator's trusted key should be the one
  `sshd`'s `authorized_keys` already holds (`userspace/sshd/src/keys.rs`):
  it IS the root identity, so `root`-role messages become verifiable
  rather than claimable.
  Note the relay already limits the blast radius from the other side:
  relayed-in traffic returns before the task-table hook and is delivered
  as a plain peer message, so a peer litter can talk to ours but cannot
  mint work in it or claim `root` in it.

## An agent turn: the tool calls

Woken by inbox growth, the agent runs one `chat_once` turn — persona
system prompt + full tool loop. The LLM drives ordinary tools; the litter
ones are just tools:

```
 system: "<persona MEOW.md> + You are one member of a litter..."
 user:   "your inbox has unseen messages; catch up and respond"

 LLM calls, in practice:
   ListPeers ──────► Peers                     "who's here"
   ReadInbox ──────► Inbox{name:"me"}          (paged reads if cold-starting)
   FileRead ───────► (local file, e.g. /akuma-src/src/main.rs)
   FileWrite ──────► (local scratch/report)
   SendMessage ────► Send{from, to:"litter"|peer, body, round}
                      (to:"litter" fans out to every roster member's inbox)
 turn ends when the model answers without tool calls
```

## Raft messages over the same wire

```
 coordinator (term T)                     agents
      │────────Heartbeat{term:T}──────────►│  (fan-out; protocol-level,
      │                                     │   never wakes an LLM turn:
      │                                     │   tick loop consumes it)
      │                                     │
      │ coordinator dies / partition        │
      │                                     │  stale heartbeat > timeout:
      │◄───────RequestVote{term:T+1}────────┤  some agent's tick promotes
      │────────VoteResponse{granted}───────►│  itself to candidate (auto —
      │◄───────RequestVote{term:T+1}────────┤  no LLM involved), majority
      │────────VoteResponse{granted}───────►│  of roster grants → it spawns/
      │                                     │  becomes the new coordinator
      │                                     │
      │  root override (from election.rs): the fixed root identity always
      │  outranks whoever is leader; operator messages ("root") are
      │  authorized regardless of election state
```

Notes:
- Heartbeats and votes are consumed by the *tick loops*, never by the LLM.
  An agent mid-turn delays its vote response but not its correctness —
  votes are idempotent per term, and the term counter makes late responses
  harmless.
- `litter-raft` (terms, one-vote-per-term, step-down on higher term,
  majority threshold, `is_authorized(sender, root)`) is built and tested;
  this loop is the transport wiring it has been waiting for.
- Compaction: the coordinator prunes each inbox past `KEEP_RECENT` messages
  and leaves exactly one marker message summarizing what was folded. The
  marker is the boundary every `History` walk stops at. Snapshot-pollution
  of live history is avoided because the marker is *one line* and everything
  under it is gone.

## Deployment topology (two hosts)

```
 ┌── Ryzen laptop ────────────────────────────┐  ┌── Akuma trashcan ──────────────────┐
 │  Ollama (stable models: qwen3, gemma…)     │  │  llama.cpp instances (experimental)│
 │  Firecracker micro-VMs, each:              │  │  Akuma boxes under herd:           │
 │   └ akuma box: meow litter live agent(s)   │  │   └ meow litter live agent(s)      │
 │        (also can hold the coordinator)     │  │        + herd service per agent,   │
 │                                            │  │          coordinator included      │
 └────────────────────────────────────────────┘  └────────────────────────────────────┘
              stable, careful workers                        fast, experimental ones
                        └──────────── one litter over IP (published hub port) ───────────┘
```

As actually stood up on 2026-09-20 — **two litters**, not one, joined by
the cross-litter relay (`docs/LITTER_RELAY_TOPOLOGY.md`) rather than being
a single roster over one hub:

```
  ┌── Mac (Docker) ────────────────┐        ┌── Ryzen laptop (Pop!_OS) ───────────────┐
  │ container `litter-yard`        │        │  Ollama :11434                          │
  │  litter_name = yard            │        │  Firecracker guest = Akuma/amd64        │
  │  sherlock hercules             │        │   ┌───────────────────────────────────┐ │
  │  zenigata ressler              │        │   │ litter_name = ryzen               │ │
  │  hub 0.0.0.0:7700              │        │   │ panther tiger jaguar (herd)       │ │
  │  published -p 7700:7700        │        │   │ hub <guest-ip>:7700               │ │
  │  192.168.1.203                 │        │   └───────────────────────────────────┘ │
  └────────────────────────────────┘        │   tap0 ── proxy_arp ── wlp2s0 .126       │
            │                               └─────────────────────────────────────────┘
            │            192.168.1.0/24 (WiFi)                    │
            └───────────────────────────────────────────────────────┘
                    each side: litter_static_peers = the other
                    relay = one hop, hub → hub, signed per agent
```

Two things that cost a day and are properties of the *hosts*, not the
protocol:

- **Firecracker gives you a tap and nothing else** — no NAT, no port
  forwarding, no DHCP. It is not QEMU's `-netdev user`/`hostfwd`. Every
  bit of routing is the host's job; `amd64/net-setup.sh` supplies the
  outbound half (MASQUERADE) and nothing supplied the inbound half until
  a peer needed to reach in.
- **Ryzen is WiFi-only, so the guest cannot be bridged onto the LAN.**
  802.11 will not forward frames for other MACs. The working equivalent
  is proxy-ARP routing: a real `192.168.1.x` on the guest, `proxy_arp=1`
  on both `wlp2s0` and `tap0`, and a `/32` route to `tap0`. The tap's own
  address must be added `noprefixroute`, or its `/24` competes with the
  real LAN route and takes the host off the network.

Because the protocol is network-only, "same container" was never a
requirement — the hub socket just needs to be reachable (published port or
tunnel between hosts). Stable-model agents on the laptop serve as the
continuity-carrying members; trashcan agents churn freely since knowledge
lives in the protocol history, not in any one process.
