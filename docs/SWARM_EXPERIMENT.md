# Swarm experiment: multiple `meow` agents debating Akuma

Status: **first working pass, 2026-09-19.** This is an experiment doc, not a
stability-graded reference doc — expect it to be rewritten as the design moves.

## Goal

Run several `meow` instances, each pointed at a different model, that can
discover each other and exchange messages, so they can be set loose on a
shared task (read Akuma's own source and debate what should change) and the
result can be read back afterward — including where they agreed and where
they didn't. Longer-term goal (not built yet): a control plane to watch this
live, and swapping the model backend for `llama.cpp` running several models
side by side.

## Code review, before adding anything

Read through `userspace/meow/src/` (`app/`, `api/`, `tools/`, `ui/`,
`config.rs`, `main.rs`) before changing it. Contrary to the "messy" impression
that prompted this — it isn't. It's already split into the modules you'd want
(`app` = conversation/turn loop, `api` = provider wire format, `tools` =
one file per tool family, `ui` = TUI rendering), tool dispatch is a flat
`match` in `tools/mod.rs` with no cleverness, and nearly every non-obvious
line carries a comment explaining *why* (e.g. `Conversation` in
`app/history.rs` is JSONL-backed specifically so a 6 MB box never holds the
whole chat in a `Vec`). Each module has an in-binary `#[cfg(feature =
"tests")] run_tests()`, run via `meow test`.

The one real gap found: `app/chat.rs` had a `warn_if_history_full` that only
*printed a warning* when history hit `MAX_HISTORY_SIZE` — the comment said
outright "Autocompact at this limit is not yet implemented." That is exactly
the failure mode an unattended swarm agent hits first (nobody is there to read
the warning or type `/compact`), so it's now fixed — see
[Recovery](#recovery-automatic-compaction) below.

Everything else in this doc is new, additive, and optional; nothing about
meow's non-swarm behavior changed.

## Communication: a shared mailbox, not a network protocol

The prompt for this was: Akuma boxes on the same host don't get network
isolation from each other, so simulate that on Linux. The simplest honest
simulation of "no isolation" isn't a broadcast/discovery protocol — it's a
filesystem all the agents can freely read and write, which is *more*
permissive than a real flat network (any agent can read any other agent's
mailbox, not just send it packets).

So the "network" is a directory, `/swarm`, mounted into every agent's
container:

```
/swarm/roster.json          # written by the launcher (docker-compose / a script), not by meow
/swarm/inbox/<agent-name>/  # one directory per agent; anyone can drop a file in here
```

Three new tools in `userspace/meow/src/tools/swarm.rs`, wired into
`tools::execute_tool_by_name` and both `OPENAI_TOOLS_JSON` schema variants in
`config.rs`:

| Tool | Effect |
|---|---|
| `SwarmPeers` | Read `/swarm/roster.json` (discovery: who else is out there) |
| `SwarmSend { to, body, round }` | Write a JSON envelope into `/swarm/inbox/<to>/` |
| `SwarmInbox` | Read every envelope currently in `/swarm/inbox/<my-name>/` |

Design choices worth stating:

- **meow never resolves a peer name to an address.** `SwarmPeers` is a
  passive read of a file some external launcher wrote. This keeps meow
  ignorant of Docker/DNS/network details entirely — it only knows how to talk
  to its own mailbox directory.
- **Nothing is deleted (for now), but not because it can't be.** `libakuma`
  *does* have `unlink` (`lib.rs:1512`) — `tools/fs.rs`'s `FileDelete` had
  simply never been wired up to it and returned a hardcoded "not yet
  implemented" instead; that's now fixed as a drive-by, since it's a real bug
  independent of the swarm work. `SwarmInbox` itself still doesn't consume
  messages: it re-reads every envelope in the directory each time and reports
  each one's `round` number so the reader (the model, or you) can tell what's
  new. That's deliberate for now (a short debate has few enough messages that
  re-reading is free), not a limitation of the substrate — worth revisiting if
  this moves off the filesystem (see [Where this is headed](#where-this-is-headed)).
- **`to` is sanitized.** It's LLM-supplied text landing directly in a file
  path (`/swarm/inbox/<to>/...`), and this module intentionally lives *outside*
  `tools::fs`'s sandbox (`/swarm` is not under the working-directory sandbox root
  the other file tools enforce). `swarm::is_valid_name` requires a bare
  `[A-Za-z0-9_-]+` token — `to = "../../etc"` is rejected, not sandboxed-and-resolved.
- **Opt-in via config, not a compile-time feature.** All three tools check
  `swarm_agent_name` in `/etc/meow/config` (new `Config` field) and fail with a
  clear error if it's unset. An agent nobody configured for the swarm can't
  accidentally read or write someone else's mailbox. The tools are always
  compiled in (declaring a `swarm` Cargo feature over an already-inert,
  runtime-gated set of tools would just be another axis of the build matrix
  for no real benefit).

Tests: `SwarmMessage::write_json`/`parse` round-trip (plain and with
quotes/newlines needing escaping), a malformed envelope is rejected, a missing
`round` defaults to `0` instead of failing, `is_valid_name` accepts plain
tokens and rejects path traversal / separators / spaces, and an unconfigured
agent's `SwarmInbox` fails with a message that says so. Run via `meow test`
inside the same Alpine/arm64 container the `linux-net` build already uses
(see `docker-linux-net-test.sh`): 6/6 passing as of this writing.

```
--- swarm tests ---
  result: 6/6
```

(Running the full suite also surfaced a pre-existing, unrelated failure in
`ui::tui::stream::run_tests` — 2/4 — from before this change; not touched
here, flagging it so it isn't mistaken for something this work broke.)

## Recovery: automatic compaction

`app/chat.rs::auto_compact_if_needed` (replacing the old warn-only
`warn_if_history_full`) now actually compacts: once `Conversation::len()` hits
`MAX_HISTORY_SIZE` (100 messages) or `Conversation::tokens()` hits
`TOKEN_LIMIT_FOR_COMPACTION` (32,000, `config.rs`), it reseeds the conversation
down to `[system prompt, a fixed "N messages/T tokens were dropped" notice,
one acknowledgement]` and keeps going — the same `Conversation::reseed` path
the LLM-invoked `CompactContext` tool already used, just triggered
mechanically instead of waiting for the model to notice and choose to call it.

This is deliberately *not* a semantic summary — no second LLM call, no
attempt to preserve "what mattered." It's a blunt safety valve: a swarm agent
running unattended for many rounds now degrades to "keep going with less
context" instead of growing its request without bound or eventually
overflowing the provider's context window with no one there to intervene.
`CompactContext` (the LLM can still ask for a real summary any time) is
unaffected and takes priority in practice, since a model that notices the
token warning and compacts on its own will usually do so before the mechanical
threshold is reached.

The other half of "recovery" is process-level: `docker-compose`'s
`restart: unless-stopped` on each agent container, so a `panic = "abort"` kill
(meow's panic strategy, `Cargo.toml`) restarts the process rather than ending
the debate. Round-to-round conversation state doesn't need to survive
that restart, because it isn't stored in meow's own session at all — see
below.

## Debate protocol: state lives in the mailbox, not in meow's session

Each round is one **stateless** `meow -c` invocation, not a long-lived
conversation:

1. Read `/swarm/roster.json` (`SwarmPeers`) — who's in this debate.
2. Read `/swarm/inbox/<self>/` (`SwarmInbox`) — what's been said so far.
3. Read some amount of Akuma source (`FileRead`/`CodeSearch` against a
   read-only mount of the repo).
4. Form or update an opinion; `SwarmSend` it to the peer, tagged with the
   current round number.
5. On the final round, additionally `FileWrite` a verdict to
   `/swarm/reports/<self>.md`.

An orchestrator shell loop just calls `meow -c "<round prompt>"` N times per
agent; it does not thread meow's own conversation across invocations. This
means:

- meow's context-window/compaction machinery only has to survive **one round**
  (however much source-reading and tool-calling that round involves) — this is
  exactly what `auto_compact_if_needed` now protects.
- the **debate's** memory across rounds is the mailbox files on the shared
  volume, which is also what makes it inspectable afterward: `cat
  /swarm/inbox/*/*.json` and `/swarm/reports/*.md` after the run is the whole
  transcript, no session-log archaeology needed.

## Control plane (not built yet)

For this first pass, "the control plane" is `docker compose logs -f` plus
reading the `/swarm` volume directly — sufficient to watch a two-agent debate
run in a terminal. A real dashboard (tail the mailbox + reports over a shared
volume, render who-said-what-when, diff the two final reports for agreement/
disagreement) is a natural fast-follow once there's more than one debate
transcript worth looking at, not before.

## Running it

Two `qwen3.5-0.8b` instances (`bootstrap/models/qwen3.5-0.8b-q4.gguf`) served
locally via Homebrew's `llama.cpp` (`llama-server`, Metal-accelerated — much
faster than the CPU-only official Docker image for this box), one meow agent
container per model, talking to the host via `host.docker.internal`:

```bash
# Host: two model servers (same weights for now; point at different .gguf
# files once more are in bootstrap/models to get genuinely different models)
llama-server -m bootstrap/models/qwen3.5-0.8b-q4.gguf --host 0.0.0.0 --port 8081 -c 8192 --alias akuma-fan &
llama-server -m bootstrap/models/qwen3.5-0.8b-q4.gguf --host 0.0.0.0 --port 8082 -c 8192 --alias akuma-skeptic &

# Build meow for linux-net (see userspace/meow/README.md), then per agent,
# each with its own /etc/meow/config (swarm_agent_name + base_url) and a
# shared /swarm volume + a read-only mount of the akuma repo:
docker run --platform linux/arm64 --rm \
  -v "$PWD/userspace/meow/target/aarch64-unknown-linux-musl/release/meow:/usr/local/bin/meow:ro" \
  -v "$PWD/etc-fan/config:/etc/meow/config:ro" \
  -v "$PWD/swarm:/swarm" \
  -v "$PWD:/akuma-src:ro" \
  alpine:3.20 /usr/local/bin/meow --no-tui -N -c "<round prompt>"
```

A `docker-compose.yml` wiring this up for N rounds × 2 agents is the next
step under `userspace/meow/swarm/` — not committed yet, since the manual run
above is what's been verified so far.

## Next steps, roughly in order

1. `docker-compose.yml` + a small round-loop entrypoint script, checked in
   under `userspace/meow/swarm/`, so the manual `docker run` above becomes
   `docker compose up`.
2. A second, genuinely different small model in `bootstrap/models/` (the
   debate is more interesting model-vs-model than weights-vs-themselves).
3. A report-diff script: read both `/swarm/reports/*.md`, ask a model (or a
   human) to summarize agreement/disagreement — this is "read their report and
   see where they disagreed" from the original ask.
4. If the mailbox model turns out too coarse (e.g. wanting >2 agents to
   genuinely *discover* each other rather than reading a launcher-written
   roster), revisit with a real UDP-broadcast discovery tool — that would be a
   more faithful simulation of Akuma's flat network, and was the original
   framing, but the file-based roster is enough to get a first debate running
   without new networking code.
5. A real control-plane dashboard once there's a transcript worth watching
   live rather than after the fact.
