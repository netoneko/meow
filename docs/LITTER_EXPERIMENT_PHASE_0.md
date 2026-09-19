# Litter experiment — Phase 0 handoff (2026-09-19)

Status: **snapshot for resuming in a fresh session**, not a stability-graded
reference doc. Read `docs/LITTER_EXPERIMENT.md` and `docs/LINUX_NET_BUGS.md`
first — they're up to date and cover the design decisions and real bugs found
so far in full detail; this doc is the short "where things stand and what's
next" version.

## Context

Working in `userspace/meow` (a submodule of the Akuma bare-metal OS repo).
Building a multi-agent "litter" of `meow` instances (Akuma's LLM chat client)
that discover each other and message over a shared filesystem mailbox, debate
Akuma's own source, and elect a leader via a hand-rolled Raft subset.

## What's built and verified

- `src/tools/litter/mod.rs`: `SendMessage`/`ReadInbox`/`ListPeers` tools
  (renamed from `Litter*` — plain verb+noun names, since a tiny local model
  failed to reliably call `LitterSend`). Gated behind Cargo feature `litter`
  (on by default). Mailbox lives at `/litter/{roster.json,inbox/<name>/}`,
  config key `litter_agent_name` in `/etc/meow/config`.
- `userspace/litter-raft/`: sibling crate, the Raft election subset
  (`ElectionState`, term/vote/heartbeat, root-override `is_authorized`). Zero
  deps, genuinely `cargo test`-able on macOS
  (`cd userspace/litter-raft && cargo test --target $(rustc -vV | grep '^host:' | cut -d' ' -f2)`
  — 12/12 passing, including a mock-transport multi-node election
  simulation). `src/tools/litter/raft.rs` in meow just re-exports it.
- `meow litter {peers,inbox,send,chase}` CLI subcommand in `src/main.rs`.
  `send --to <name> --body "<text>" [--round N]` currently hardcodes sender
  identity to `"root"` — **was mid-edit adding a `--from <name>` flag when
  this session ended**, not yet applied.
- Four personas in `swarm/personas/{sherlock,hercules,zenigata,ressler}.md`
  (Sherlock Holmes / Hercules / Inspector Zenigata / Agent Ressler), dropped
  as `MEOW.md` into each agent's working directory.
- Root ECDSA P-256 keypair at
  `userspace/meow/target/litter-keys/{root.pem,root.pub.pem}` (generated via
  `openssl ecparam`).
- Real bugs found and fixed this session (see `docs/LINUX_NET_BUGS.md` for
  full detail): `libakuma::uptime()`, `FileDelete`/`unlink`, and — the big
  one — `spawn()`/`waitpid()` were completely broken under the `linux-net`
  build (Akuma-private syscall numbers with no Linux handler); fixed in
  `userspace/libakuma/src/lib.rs` under the existing `linux-abi` feature
  using real `fork`+`pipe2`+`dup3`+`execve`+`wait4`. The `Shell` tool now
  actually works under `linux-net` for the first time.
- A parallel tool-call dispatch (fork per `Shell`/`HttpFetch` call) was
  prototyped and **reverted** — hit an unresolved nested-fork bug (forking
  from inside a child that itself forks via the fixed `spawn()`). Not
  shipped; see the comment left in `app/chat.rs`.

## Build/test recipe

Needed every time — the target is cross-compiled, not native:

```bash
cd userspace/meow
MUSL_LIBC=/opt/homebrew/Cellar/musl-cross/0.9.11/libexec/aarch64-linux-musl/lib/libc.a
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc \
RUSTFLAGS="-C link-self-contained=no -C link-arg=-nostartfiles -C link-arg=$MUSL_LIBC" \
cargo +nightly build --release -Zbuild-std=core,alloc --target aarch64-unknown-linux-musl --features linux-net,tests
docker run --platform linux/arm64 --rm -v "$(pwd)/target/aarch64-unknown-linux-musl/release/meow:/meow:ro" alpine:3.20 /meow test
```

## Environment as of this snapshot

`ollama serve` is running on `127.0.0.1:11434` (started via
`/Users/netoneko/github.com/netoneko/yolo/run-ollama.sh`, which sets
`OLLAMA_MAX_LOADED_MODELS=2` — running more than two distinct models
concurrently causes swap thrashing, so run agents sequentially, not in
parallel, when using it). Models available: `qwen3.5:0.8b`, `qwen3:4b`,
`gemma4-yolo-4b:latest` (8B), `gemma4:e4b` (8B).

Four standalone `llama-server` instances from an earlier round may still be
running on ports 8081–8084 (check `ps aux | grep llama-server`) — probably
fine to kill now that Ollama is the plan.

From inside a Docker container, reach the Mac host via IP literal
`192.168.65.254` (**not** `host.docker.internal` — that hostname triggers a
still-unresolved DNS retry-storm bug, documented in `docs/LINUX_NET_BUGS.md`
§5 but not root-caused).

## Immediate next task

1. Finish adding `--from <name>` to `run_litter_send` in `src/main.rs`
   (defaulting to `"root"` when omitted, to keep the existing root-override
   semantics).
2. Rebuild.
3. Wire up 4 litter agents (reuse the Sherlock/Hercules/Zenigata/Ressler
   personas) each pointed at a different Ollama model via
   `base_url=http://192.168.65.254:11434` + a distinct `current_model`.
4. Send each an initial task message using `meow litter send --from meow-chan`.

## Also still owed (asked for earlier, not yet written)

Dedicated `SWARM_AUTH.md` and `RAFT.md` docs were requested at one point;
their content ended up folded into `docs/LITTER_EXPERIMENT.md`'s "Where this
is headed" section instead. Worth splitting out properly once the
identity/signing layer is actually built, rather than before.
