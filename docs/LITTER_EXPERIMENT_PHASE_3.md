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
