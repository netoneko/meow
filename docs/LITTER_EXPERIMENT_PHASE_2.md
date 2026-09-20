# Litter experiment — Phase 2 handoff (2026-09-20)

Status: snapshot for resuming in a fresh session, same genre as Phase 0/1.
Read `docs/LITTER_EXPERIMENT.md` for the experiment's full history and
`docs/LITTER_STATE_MACHINE.md` + `docs/LITTER_RAFT_LOOP.md` for the design
the code now implements. This is the short "what changed and what's
running" version.

## What changed since Phase 1

1. **`swarm/` is `litter/`** (we aren't savages). Personas stayed put,
   `run_litter.sh` untouched.
2. **The hub stopped being a process.** `crates/litter-hub` remains only
   as a debugging tool. The hub is now an in-meow state machine
   (`src/tools/litter/serve.rs`, the no_std twin of litter-hub's
   `HubState`) owned by whichever `meow litter live` process wins the bind
   race on the hub socket. The OS deciding who owns the socket IS the
   election; one socket, so the swarm can't split.
3. **`meow litter live`: a resident agent.** One process, two threads
   (state machine + wiring: the two LITTER design docs). The agent loop
   polls its inbox and wakes into a normal `chat_once` turn on new
   messages; the raft/serve thread drains the hub backlog, runs the task
   table, probes static peers, and compacts history — and never blocks on
   inference.
4. **Threads are raw `clone(2)`**, not musl pthreads (`src/rt.rs`,
   modeled on `userspace/amd64/threadprobe`): meow's raw `_start` means
   musl's thread runtime never initializes, so `pthread_create` fails.
   The trampoline is per-arch assembly (aarch64 + x86_64, `cfg`-gated,
   `compile_error!` fallback — an unconditional-aarch64 version broke
   `x86_64-unknown-none` builds for the trashcan within hours of landing).
   Threads are TLS-free by construction: no `CLONE_SETTLS`, because the
   parent's thread pointer is never initialized either. No libc calls from
   litter thread code. The one mutex (`PMutex`) is a futex word with a
   Drop guard — no libc pthread mutex either.
5. **Wire protocol v2** (`crates/litter-wire`):
   - `Peers { since }` is now the pulse: one request is heartbeat
     observation (any answer = leader alive), roster fetch, and change
     feed (`term`/`leader`/`epoch`/`events[]`).
   - `History { name, before, limit }`: paged backward inbox reads so a
     joining agent sources history from the protocol, in batches, up to
     the last compaction marker.
   - `Message` gained `kind` (`chat`/`marker`/`assignment`/`done`) and
     `role` (`peer`/`leader`/`root`) — hub-stamped at delivery, so
     protocol traffic and sender authority are fields, not body-string
     conventions.
6. **Compaction is a marker, not a snapshot**: each inbox keeps its newest
   `KEEP_RECENT` messages plus ONE marker message summarizing what was
   folded; `History` walks stop at the marker. Nothing older is ever
   pulled by a joining agent.
7. **Task table** (in-memory, coordinator-owned, `tasks.rs`): `[task]`
   bodies open tasks — but only from `root` or the leader (the live run
   where agents task-spammed each other is why). The leader leases them to
   the least-loaded agent, requeues expired leases, and folds
   `[done: tN]` closes into events. Task state is leader memory by
   design; if the leader dies, unfinished work is re-posted by whoever
   remembers it.
8. **`MEOW_HOME` scoping** (`config::scope`): config, `MEOW.md` persona,
   and the (now legacy) fs mailbox all prefix with `$MEOW_HOME`, so N
   resident agents share one filesystem without one global config.
9. **`litter_static_peers=name@host:port,…`** config key: peers the raft
   thread probes on its tick, for the trashcan/laptop split — the other
   host's agents show up as discovered/lost events instead of not
   existing.
10. **The filesystem mailbox is retired** for hub-mode agents; the litter
    is network-bound. The `swarm`-era scripts became `litter/yard.sh` +
    `litter/yard_init.sh` (persistent container, one resident agent per
    persona; `talk`/`task`/`respawn`/`watch`/`logs` subcommands).

## Bugs found live (each one is a design lesson now encoded)

1. **Leader self-deadlock**: the leader polling its own inbox over
   loopback when the same loop was the only thing that could serve it.
   Fixed by having BOTH threads drain the shared listener — the agent
   loop is not just a client.
2. **`try_accept` blocks** unless the listener is set nonblocking: the
   first drain parked in `accept()` holding the state lock — a hub frozen
   while still answering. Confusion followed ("why no re-election?"):
   the hub never went silent, so there was nothing to re-elect *from*.
3. **The staleness gate self-silenced**: refusing tool calls because
   "time since last hub call > 15s" reads a model thinking for 15s as
   hub death, and latches the whole turn shut. Removed: every client
   call is deadline-bounded (5s) instead, and the tick loop's pulse —
   not the tools — drives the WAYWARD transition.
4. **`CLONE_SETTLS` with a never-initialized thread pointer → EINVAL**:
   bisected live; led to the documented TLS-free constraint above.
5. **Unconditional aarch64 asm** broke amd64 builds (see 4 above).

## Tests and builds, as of this snapshot

- `cargo test -p litter-wire` 19/19, `-p litter-raft` 12/12 (host, use
  `--target $(rustc -vV | grep host | cut -d' ' -f2)` from
  `userspace/meow`).
- `meow test` in the Alpine/arm64 container (linux-net + tests features):
  all suites green — litter 3/3, hub client 2/2, observe 4/4, serve 6/6,
  tasks 4/4, election 3/3, rt threads 2/2 — except the pre-existing,
  documented `ui::tui::stream` 2/4.
- `x86_64-unknown-none --features litter` checks clean again.

## Live run status (yard container, 4 personas / 4 Ollama models)

Verified working end to end: bootstrap (bind race → leader term 1), event
feed to every follower, group broadcast waking all four agents, kill an
agent → respawn → rejoin with full event-log replay.

Not yet verified: a COMPLETE four-model debate transcript. The first two
attempts were killed by bug (3) and by the frozen-hub bug (2) before any
agent finished a turn; the fixes are in, the rerun hasn't happened yet.
That rerun is the first thing a fresh session should do:

```bash
litter/yard.sh stop && litter/yard.sh start
litter/yard.sh talk litter "Debate the merits of this kernel: read one \
  file under /akuma-src/src, post your verdict to the litter."
sleep 300 && litter/yard.sh watch
```

(Ollama must be reachable at `192.168.65.254:11434` — it currently runs
with `OLLAMA_HOST=0.0.0.0` after a bounce on 2026-09-20; the default
`run-ollama.sh` binds 127.0.0.1, which containers cannot reach.)

## Not done (next, roughly in order)

1. The full debate rerun above; then the report-diff summarizer (old
   Next steps item 4).
2. `election.rs` vote exchange over the relay (terms/votes are stubbed;
   leader change is still bind-race only).
3. Signed envelopes: root identity = the operator's sshd
   `authorized_keys` key (see `LITTER_RAFT_LOOP.md` future work).
4. Trashcan/Ryzen split: static peers + herd service units per agent.
5. `meow litter` as an MCP server (old item 10).
