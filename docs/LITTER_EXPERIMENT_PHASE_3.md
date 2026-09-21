# Litter experiment — Phase 3 handoff (2026-09-20)

Status: snapshot for resuming in a fresh session, same genre as Phase
0/1/2. Phase 2 ended with "the full four-model debate rerun is the first
thing a fresh session should do". That rerun could not have worked: the
tree did not build for aarch64 at all, and the yard deadlocked within ~20
seconds of every start. This phase is those fixes plus the task-table
rewrite onto `LITTER_WORKFLOW.md`.

## What was actually broken

Four defects, all found by running the thing rather than by reading it.

### 1. HEAD did not build for aarch64

`raft_entry`'s `raw_write` debug instrument (added with the Phase 2
checkpoint) was x86_64 inline asm with no `cfg` gate: `invalid register
'rax'` ×6 on `aarch64-unknown-linux-musl`. This is Phase 2's bug 5
exactly, mirrored — that one was unconditional *aarch64* asm breaking
amd64. Fixed the same way: gate both arches, `compile_error!` on a third.
`write` is nr 64 on aarch64 (x8, args x0-x2, `svc #0`).

The cross-build recipe had also been lost. It is:

```bash
RUSTFLAGS="-C linker=aarch64-linux-musl-gcc -C link-self-contained=no -C link-arg=-nostartfiles" \
  cargo +nightly build --release -Zbuild-std=core,alloc \
    -Zbuild-std-features=compiler-builtins-mem \
    --target aarch64-unknown-linux-musl
```

All three additions are load-bearing on macOS: the default `cc` is
Apple's `ld` and rejects every GNU flag; `link-self-contained=no` +
`-nostartfiles` stop rustc's `crt1.o` colliding with meow's own raw
`_start`; `compiler-builtins-mem` supplies the `memcpy`/`memmove` that
`-Zbuild-std=core,alloc` otherwise leaves undefined.

### 2. The hub deadlocked on its own lock (the wedge)

Symptom: every client call — including `Join` — failed with "went silent
before answering", while the container burned 300% CPU and the leader's
raft log stopped one line after `raft tick 5 top`.

Root cause: `serve::drain(&listener, &mut state.lock())` holds the
`PMutex<HubState>` across each connection's deadline-bounded read; on a
would-block the deadline loop calls its poll hook; the hook is
`local_drain`, which did `ctx.state.lock()` — the same non-reentrant
mutex, on the same thread.

The evidence, which is worth keeping because it identifies the class
without a debugger — `/proc/<tid>/syscall` for both leader threads:

```
tid 12: 98 0xffff94116e70 0x80 0x1 0x0 0x0 0x0 0xfffff198b970 0x4045d8
tid 14: 98 0xffff94116e70 0x80 0x1 0x0 0x0 0x0 0x47f6e0     0x4045d8
```

Same futex word, same op (`FUTEX_WAIT_PRIVATE`), same expected value
(`1` = locked), same PC (the single `futex_wait` call site in
`PMutex::lock`); the only difference is the stack pointer — the main
thread's, and the raft thread's slot in `rt.rs`'s static `STACKS`. A
non-reentrant mutex sitting at `1` with *every* thread that could store
`0` asleep can only mean the holder is among the waiters. That is a
recursion proof, and it also proves the hang is permanent rather than
slow.

Both hooks (`serve::deadline::set_io_poll_hook` and `hub::set_drain_hook`)
point at `local_drain`, which is why the raft thread was caught too — mid
peer-probe, before it could print `tick 5 probed`.

Fixed twice over, deliberately: `PMutex::try_lock` + `local_drain`
skipping when it cannot take the lock (stops the hang), **and** `drain`
restructured so the lock never spans I/O at all (removes the class). See
`LITTER_STATE_MACHINE.md` § "Lock discipline".

### 3. Every sleep was a no-op under `linux-abi`

`libakuma::sleep`/`sleep_ms` passed `nanosleep`'s arguments in registers.
Akuma's own `sys_nanosleep` accepts that (it sniffs `a0` and treats
anything below a page as a raw second count), but a real Linux kernel
takes only a `*const timespec` — so in Docker every sleep returned
`EFAULT` instantly, and neither function has a return value to notice
with. Measured: a 1-second tick loop ran ~6400 iterations/second, and
four agents pinned 300% CPU while doing nothing. `sleep_ms` was the worse
of the two — it passed `a0 = 0`, a NULL `timespec`, so it failed
unconditionally rather than address-dependently.

Fixed by building a real `timespec` under `feature = "linux-abi"` (the
Akuma path is untouched). `sleep_ms` now also splits seconds from
nanoseconds: Linux rejects `tv_nsec >= 1e9` with `EINVAL`, so
`sleep_ms(1500)` would not have slept even with the pointer right.

After the fix, the same yard idles at **1.2% CPU** with ticks at 1/s.

### 4. `TcpStream::connect` has no timeout — and the yard shipped a peer

`probe_peers_io` documents itself as running "on a probe budget" of 500ms,
but `call_addr_timeout` bounds only the read and write; `connect` is a
plain blocking connect. The probe runs inside the thread that serves the
hub, so one unreachable static peer parks the hub for the kernel's full
SYN-retry backoff on every pulse.

`litter/yard.sh` defaulted `LITTER_STATIC_PEERS=ryzen@192.168.1.126:7700`
— an address that is not there on a single-host run. Default is now
empty, with the hazard documented at the top of the script.

**Still open**: the unbounded `connect` itself. The 500ms claim in
`probe_peers_io` is false until a nonblocking connect with a deadline
lands in `libakuma::net`.

## What the workflow rewrite changes

`tasks.rs` went from a flat table (one `[task]`, auto-assigned round-robin
to the least-loaded agent, `[done: tN]` deletes it) to the parent →
directed sub-task → claim → submit → clear → artifact lifecycle the
committed `LITTER_WORKFLOW.md` describes. The wire spelling, the
authority rules, the timers and the four pinned divergences are in that
doc's "Implementation" section; it is the design of record, not this file.

Two shape decisions worth repeating here:

- **`[plan: tN]` is one atomic message carrying every sub-task.** Without
  that the table can never know planning has finished, so "all sub-tasks
  cleared" — the trigger for the final artifact — is undecidable.
- **The table tells the leader the exact verb to type** (`[plan-needed:]`,
  `[clearance-needed:]`, `[artifact-needed:]`), re-sent on a nag interval.
  A 0.8B model will not infer `[artifact: t1]` from a design document, and
  a directive delivered once would stall a parent permanently if dropped.

## Environment notes

- Ollama must be reachable from the container at `192.168.65.254:11434`,
  which means `OLLAMA_HOST=0.0.0.0 ollama serve` — the default binds
  127.0.0.1, which containers cannot reach (Phase 2 said the same; it
  does not survive a reboot).
- The four persona models (`qwen3:4b`, `gemma4-yolo-4b:latest`,
  `gemma4:e4b`, `qwen3.5:0.8b`) are all present locally.
- `/proc/<pid>/task/*/syscall` inside the container is the fastest wedge
  triage there is: syscall 98 on aarch64 is futex, 203 is connect. Busybox
  `ps` in `alpine:3.20` prints nothing useful here; read `/proc` directly.

## Live run, 2026-09-20/21 (yard, 4 personas / 4 Ollama models)

The workflow was driven end to end against real models. What the protocol
did, in order, from the event log:

```
[event] parent task t1 opened by root
[event] t1.1 assigned to hercules / t1.2 assigned to zenigata / t1.3 assigned to ressler
[event] t1.1 offered to hercules  / t1.2 offered to zenigata  / t1.3 offered to ressler
[event] t1.3 claimed by ressler   / t1.2 claimed by zenigata  / t1.1 claimed by hercules
[still yours: t1.3] ... (reminder 1 of 3)      <- delivered to ressler after its claim
[event] t1.3 submitted by ressler, awaiting clearance
[clearance-needed: t1] ...                     <- delivered to sherlock
```

Every step is a real typed tool call: `TaskPlan` from the leader,
`TaskUpdate{status:"claim"}` and `TaskUpdate{status:"done"}` from the
workers. **Six of the eight lifecycle steps are proven live**; `clear`
and `artifact` were delivered to the leader and not yet acted on.

### What the live run found that tests could not

Four defects, each invisible to a unit test because each is a property of
*how long a model takes* or *who is in a roster*:

1. **A claimed sub-task was never worked on.** Claiming ends a turn, and
   nothing then addressed the holder again — the replicated record is
   non-waking by design. Two agents claimed cleanly and neither ever
   reported. Fixed with bounded holder nudges (see `LITTER_WORKFLOW.md`
   § "Nobody is left holding work in silence").
2. **The operator was assigned a sub-task.** `operator` was an ordinary
   roster member, so a leader splitting work four ways gave it a quarter —
   unclaimable, and the artifact requires every sub-task cleared, so the
   parent could never close. It now joins as the reserved `root` and is
   never assignable.
3. **`CLAIM_WINDOW_US` was shorter than a turn** (180 s vs a measured
   120-200 s), so offers lapsed and re-offered while their assignee was
   still thinking about the first copy. Now 600 s.
4. **`max_tokens` was below the floor for a reasoning model.** At 2048,
   qwen3:4b streamed 117 s and emitted zero visible tokens — the whole
   budget went to thinking, so no tool call was made and the task could
   not progress. The cap had been protecting the hub from a long agent
   turn; single ownership removed that coupling, so it is now 8192.

Plus one that was pure waste rather than a stall: the election wake fired
unconditionally, so the first leader of a brand-new litter spent its
entire opening turn (194 s) roll-calling four idle agents about work
nobody was holding. It is now takeover-only.

### The remaining limit is turn latency, not the protocol

The leader was last observed **22.5 minutes inside one turn**, messaging
agents individually. Nothing is wedged — the hub idles at ~1% CPU and the
owner loop keeps serving — but a parent task cannot advance faster than
its leader can finish a turn, and on a 4B reasoning model behind a tool
loop that is tens of minutes.

Two things worth trying, in order: cap the tool-loop iterations per wake
(a turn that has made its decision should stop), and prefer a
non-reasoning model for the leader, whose job is dispatch rather than
analysis.

## Trap: rebuilding kills a running yard

`litter/yard.sh` used to bind-mount the build output
(`target/aarch64-unknown-linux-musl/release/meow`) straight into the
container. A bind-mounted *file* is bound to an inode, and `cargo build`
replaces the binary by rename — so a rebuild while the yard is running
pulls the executable out from under every agent.

Measured 2026-09-21: all four agents died mid-turn, and the container
stayed up, because `yard_init.sh` is PID 1 and its `wait` loop survives
its children. The symptom is a yard that answers nothing while
`docker ps` says it is healthy; the tell is `docker stats` showing ~1 MB
instead of ~4 MB.

`yard.sh start` now copies the binary to `target/yard/meow` and mounts
that, so rebuilding is safe while a litter is running. Restarting the
yard picks up the new build, as before.

## Four agents, one temp file: the request-body bug

Every request-level failure chased in this session was one defect.

`request_body_path()` returned a **single fixed path**,
`/tmp/.meow_request.json`. meow stages each outgoing request body in that
file and then streams it to the socket. That is fine for one `meow`
process and catastrophic for several sharing a filesystem — and the litter
runs four agents in one container. Each opened the same file `O_TRUNC`,
wrote its body, and read it back to send; they clobbered each other
mid-request. Agent A writes an 8 KB body, agent B truncates the file and
writes 5 KB, agent A then streams 5 KB under a `Content-Length` of 8 KB.

It presented as three unrelated faults, which is why it took so long:

| What was seen | What it was |
|---|---|
| `500 parse error ... missing closing quote; last read: '"ListPeer'` | short body whose length happened to match the declared one; server parsed incomplete JSON |
| agent stuck at `[jacking in..] waiting` forever | declared length exceeded the bytes sent; server waited for a body that was never coming |
| intermittent, worse as the litter grew | it scales with the number of concurrent agents |

The path is now per-process (`/tmp/.meow_request.<pid>.json`). After the
fix: **zero** truncation errors across four agents.

### Hardening found on the way

Three real defects of the same shape — a partial operation treated as
complete — were fixed while hunting it, and are worth keeping regardless:

1. **`fd_write_str` did not loop.** `write(2)` may write fewer bytes than
   asked and return the count; that is the contract, not an error. It
   treated any short write as fatal and stopped, truncating the body.
2. **`post_from_fd` (TLS) broke on any non-positive read.** `if n <= 0
   { break }` treats a read error exactly like EOF.
3. **`send_post_request_from_fd` (plain HTTP)** — the path Ollama and
   `llama-server` actually use — had the identical bug and was missed on
   the first pass.

Both send paths now count bytes and fail with `Request body ended early
(truncated request)` rather than sending a short body. That error message
is what finally localized the shared-path bug: it turned a silent
corruption into a statement about *this* process.

Ruled out along the way, so it is not re-investigated: the conversation
JSONL is valid, the tools schema is valid in all four feature variants,
`TcpStream::write_all` loops correctly, and `write_chat_body` propagates
errors properly. A capture proxy confirmed a fixed client sends
`declared Content-Length = 7472, actually received = 7472, parses: YES`.

## The timing metric was lying (and took three wrong answers with it)

Corrected 2026-09-21. Everything previously written here about "30-40
minute turns" was reading a broken number.

`src/api/client.rs` declared `let mut stream_start_us = 0;` and only set it
when the **first content token** arrived. Three of the four sites that read
it guarded on `first_token_received`; the early-return paths did not, so
they computed

```rust
stream_us: now_us() - stream_start_us      // = now_us() - 0
```

which is the raw `CLOCK_MONOTONIC` value — inside a container, the Docker
VM's uptime. Any response that carried only `reasoning_content` and no
content token therefore reported the host clock as its own duration.

The tell was there all along and was missed twice: an agent reported
`Duration: 68m 42s` in a container that had been up fifteen minutes, and
consecutive readings differed by exactly the real elapsed time
(`68m42.034s` → `69m02.856s` = 20.8 s). A cumulative-looking counter on a
process too young to have accumulated it is a broken baseline, not a slow
model.

With the guard applied to all six unguarded sites, the same litter on the
same hardware reports **11.7 s, 28.2 s, 44.1 s, 34.9 s** per request.

### What this invalidated

Three conclusions were drawn from that number and all three were wrong:

1. "The models are slow, use bigger ones."
2. "meow spends ~90% of each turn idle after the server finishes" — drawn
   from comparing meow's durations against the server's `total time`
   lines, which are only written for *completed* requests, so it compared
   finished short requests against unfinished long ones.
3. "Ollama is swapping 19 GB models per request" — the context cap
   (`OLLAMA_CONTEXT_LENGTH=8192`, 19 GB → 11 GB) is still worth having,
   but it was not what the 68 minutes measured.

### What is actually true about performance

- **Prompt caching works.** First request evaluates the full prompt (2090
  tokens); later ones only the new tokens (55-580), at 167 ms - 4.4 s.
- **Generation is 17-57 tok/s**, depending on how many agents share the GPU.
- **A reasoning model is still the wrong choice**, for a real reason rather
  than the imagined one: it spends its whole `max_tokens` budget on
  `reasoning_content` before emitting an answer or a tool call. gemma does
  not do this. `--reasoning off`, `--reasoning-budget 0` and
  `--chat-template-kwargs '{"enable_thinking":false}'` were each measured
  **not** to suppress it for qwen3:4b; they only move the thoughts between
  response fields.
- **Cap Ollama's context.** The default 131072 inflates a 9.6 GB model to
  19 GB resident, so `OLLAMA_MAX_LOADED_MODELS` cannot honour its setting
  and models thrash. `OLLAMA_CONTEXT_LENGTH=8192` brings it to 11 GB.

### The lesson worth keeping

Three separate wrong conclusions came from one unvalidated metric, and each
time the instinct was to explain the number rather than to check it. A
duration that exceeds its own process's lifetime, or a metric that only
exists on success being used to characterise failures, is a measurement bug
until proven otherwise.
