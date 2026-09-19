# Litter experiment — Phase 1 handoff (2026-09-19, later same day as Phase 0)

Status: **snapshot for resuming in a fresh session**, not a stability-graded
reference doc, same as `docs/LITTER_EXPERIMENT_PHASE_0.md`. Read
`docs/LITTER_EXPERIMENT.md` first — it's up to date and has full detail on
everything below; this is the short "where things stand and what's running
right now" version.

## What changed since Phase 0

1. **`--from` flag** finished on `meow litter send` (was mid-edit at the end
   of Phase 0). Works on both transports.
2. **TCP hub built**: two new crates, `litter-wire` (wire format/framing) and
   `litter-hub` (the relay binary), now living at
   `userspace/meow/crates/{litter-wire,litter-hub}` as real members of
   meow's own Cargo workspace (moved there from sibling-of-meow
   `userspace/litter-*` mid-session — a nested path dependency can't also be
   its own separate `[workspace]` root, so they had to become workspace
   members with `litter-hub` excluded from `default-members` since it's a
   `std` binary and meow's own build is `-Zbuild-std=core,alloc` against a
   `no_std` target). `litter-raft` moved the same way. Opt-in via
   `litter_hub_addr` in `/etc/meow/config`.
3. **Bootstrap-join**: `Request::Join`/`Response::Joined` added to the wire
   protocol. `litter-hub --roster` is now just an optional seed; `meow`'s own
   startup sends `Join` automatically whenever both `litter_hub_addr` and
   `litter_agent_name` are configured, so a hub can start with an **empty**
   roster and learn its whole membership from agents bootstrapping.
4. **`meow litter observe`**: read-only CLI subcommand, merges every
   participant's messages (hub: `Peers`+`Inbox`; filesystem: scans
   `/litter/inbox/*`) sorted by round, prints each with a small colored ASCII
   avatar (`src/akuma_20.txt`, one of 8 cycling ANSI colors per sender).
5. **`litter-hub` cross-compiles as an ordinary `std` binary** for
   `aarch64-unknown-linux-musl` with the **stable** toolchain (no
   `-Zbuild-std`, unlike `meow` itself) — confirmed working. This means hub +
   all four agents can run **inside one container**, talking over
   `127.0.0.1`, sidestepping the still-unconfirmed `host.docker.internal`
   retry bug entirely for this use case (Ollama on the host is still reached
   via the `192.168.65.254` IP literal, since that's the one thing outside
   the container).
6. **`swarm/run_litter.sh`** (new, checked in): the one-liner entrypoint —
   starts `litter-hub` in the background, runs one `meow litter chase` round
   per `swarm/personas/*.md` sequentially against four different Ollama
   models, ends with `meow litter observe`. The one-liner itself is in
   `docs/LITTER_EXPERIMENT.md`'s "Next steps" §2.

All of this is documented in full in `docs/LITTER_EXPERIMENT.md` (search for
"2026-09-19" — three sections were added/updated: "Transport", "Bootstrap",
"`meow litter observe`", plus the "Next steps" list was struck through and
updated item by item).

## Tests, all passing as of this snapshot

`cargo test -p litter-wire` 17/17, `cargo test -p litter-hub` 13/13,
`cargo test -p litter-raft` 12/12 (all from `userspace/meow`, any subdir —
they're workspace members now, not separate workspaces — use
`--target $(rustc -vV | grep '^host:' | cut -d' ' -f2)`). `meow test` inside
the Alpine/arm64 container: everything green except the pre-existing,
documented, unrelated `ui::tui::stream::run_tests` (2/4).

## In progress when this session ended

A real end-to-end run of `swarm/run_litter.sh` was **started as a background
process and was NOT finished or verified when this session ended** — the
container was still on the *first* persona (sherlock, `qwen3:4b`) several
minutes in, reading kernel source files via `FileRead`. Check whether it's
still running / what it produced:

```bash
docker ps --filter name=sweet_johnson   # or whatever the container is now named
docker logs sweet_johnson 2>&1 | tail -100
```

If it finished cleanly, the tail of that log is the transcript
(`meow litter observe`'s output — colored ASCII avatars + `[round N]`
messages from sherlock/hercules/zenigata/ressler). If it's still running,
either wait for it or `docker stop` it — it's an ordinary `--rm` container,
safe to kill (the hub and its state die with it; nothing persists outside
the container by design, see next section).

If it errored, likely causes to check first: Ollama unreachable at
`http://192.168.65.254:11434` from inside the container, a model name typo
in `swarm/run_litter.sh`'s `AGENTS` list vs. what `ollama list` actually has,
or `/akuma-src` not mounted (the task prompt references it).

## Known limitation, called out explicitly in the doc

`run_litter.sh` is a **one-shot batch**, not a persistent setup: the
container is `--rm`, so the hub and everything it learned dies when the
script finishes. There is currently no way to "keep a litter running and
drop in new tasks whenever" — `docs/LITTER_EXPERIMENT.md`'s "Next steps" §7
names this as the natural next step (a persistent hub container + a separate
one-liner that just sends a task, rather than re-running the whole batch).
The user asked about this specifically ("when can i give them tasks") right
as this session ended — that's almost certainly where a fresh session should
pick up, once the in-progress run above is checked on.

## Immediate next task

1. Check on / clean up the in-progress `run_litter.sh` container (above).
2. If the transcript looks sane, build the "persistent hub" half of Next
   Steps §7: a long-lived (not `--rm`) container running just `litter-hub`,
   and a small `meow litter send`/`chase` wrapper the user can invoke against
   it on demand, rather than re-running the fixed 4-persona batch from
   scratch every time.
3. Everything else in `docs/LITTER_EXPERIMENT.md`'s "Next steps" list is
   still open, roughly in the order listed there (report-diff script,
   per-agent keypairs + wiring `election.rs` to the hub, a live dashboard).
