# Bugs found running meow under `linux-net` (2026-09-19)

Status: **all fixed except one, which is worked around.** Found while building
the litter/swarm experiment (`docs/LITTER_EXPERIMENT.md`) — none of these are
swarm-specific, they affect any `linux-net` build. They surfaced in this order
because that's the order the experiment happened to exercise the affected
code paths, not because of any dependency between them.

## 1. `libakuma::uptime()` silently returns garbage under `linux-net`

**Symptom**: tool-call duration stats printed nonsense (`Stream: 4027490ms`,
i.e. 67 minutes, for a call that took well under a second), and a
timestamp-based filename came out as `18446744073709551615-sherlock.json` —
`u64::MAX`.

**Cause**: `libakuma::uptime()` issues Akuma's own syscall 319, which has no
handler on a real Linux kernel. The call returns an error, which — cast to
`u64` at the call site — becomes `u64::MAX`, not a small growing number. A
`linux-net`-aware replacement (`linux_net::uptime_us()`, real
`clock_gettime(CLOCK_MONOTONIC)`) already existed, but was only used by
`api::client`'s private `now_us()`. Seven other call sites across the crate
called `libakuma::uptime()` directly: the TUI's frame timing
(`ui/tui/{layout,render,input}.rs`), `app::session`'s session-id derivation,
`tools::mod_types`'s temp-file naming, `app::chat`'s tool-duration stats, and
— found because it was new code written in the same session — the litter
mailbox's own message-filename timestamps.

**Fix**: promoted `now_us()` out of `api::client` into `util::now_us()`
(same `#[cfg(feature = "linux-net")]` / not split as before, just shared),
and switched all seven other call sites to it.
`src/util.rs`, `src/api/client.rs`, `src/app/{chat,session}.rs`,
`src/tools/mod_types.rs`, `src/tools/litter/mod.rs`, `src/ui/tui/{layout,render,input}.rs`.

**Verified**: a tool call that previously reported "Stream: 4027490ms" now
reports single-digit-to-low-triple-digit milliseconds; a fresh litter mailbox
run produces sane filenames (`<real-microsecond-timestamp>-<sender>.json`).

## 2. `FileDelete` claimed `unlink` didn't exist — it does

**Symptom**: `tools::fs::tool_file_delete` unconditionally returned "Delete
not yet implemented", with a comment claiming `libakuma` had no `unlink`
syscall.

**Cause**: stale comment. `libakuma::unlink` exists
(`userspace/libakuma/src/lib.rs:1512`, `UNLINKAT` — a real Linux syscall
number, so this one worked under `linux-net` too, once wired up) — it had
simply never been connected to the tool. Found by a live user correction
mid-session, not by testing; a reminder that a comment asserting "X doesn't
exist" is a claim about the state of a dependency at some point in the past,
not a fact to trust without grepping for it.

**Fix**: `tool_file_delete` now calls `libakuma::unlink` and reports the
real result. `src/tools/fs.rs`.

## 3. `spawn()` — and therefore the entire `Shell` tool — never worked under `linux-net`

**Symptom**: every `Shell` tool call failed: `[uname] Failed to spawn
'/bin/uname' (not found?)`, `[sleep] Failed to spawn '/bin/sleep' (not
found?)` — for binaries that demonstrably exist (`/bin/sleep` resolves via
`resolve_binary`'s own existence check before `spawn()` is ever called).
Looked at first like a busybox-symlink or `$PATH` problem; it wasn't — the
differential test was running `uname -a` (no symlink involved) and getting
the identical failure.

**Cause**: `libakuma::spawn()` issues Akuma's own `SPAWN` syscall (301) — a
single private syscall bundling fork+exec+pipe-setup that has no handler on a
real Linux kernel at all. Every call returned `None`. This had apparently
never been exercised under `linux-net` before this session — the feature's
own test script (`docker-linux-net-test.sh`) only exercises the chat/HTTP
path, not the Shell tool.

**Fix**: added a `#[cfg(feature = "linux-abi")]` alternative `spawn_full` in
`libakuma` (the existing feature `linux-net` already turns on for meow, via
`linux-net = ["libakuma/linux-abi"]` in `Cargo.toml` — the same gate
`getpid()` already uses for an analogous "this page/syscall isn't a thing on
real Linux" problem) built from genuine Linux primitives: `pipe2` (already a
real Linux number, unmodified), [`fork`] (already a real `CLONE`, unmodified —
this half was never broken), `dup3` onto the child's stdout, then `execve`.
Two new syscall numbers added (`EXECVE = 221`, `DUP3 = 24`, aarch64 — real
asm-generic Linux numbers, additive, no existing call site touched).
Stdin-piping and the pty flag are **not** wired up on this path — nothing in
meow currently calls `spawn_with_stdin`/`spawn_pty`, and adding a second pipe
/ pty allocation for callers that don't exist yet isn't worth the code.
`userspace/libakuma/src/lib.rs`.

**Verified**: `uname -a` through the Shell tool now returns the real
container's `uname` output with exit code 0.

## 4. `waitpid()`/`waitpid_status()` — same bug, one level up

**Symptom**: after fixing #3, a `Shell` call would actually run and its
output would appear — but the whole turn then hung for exactly 30 seconds
(`tools::shell::drain_child`'s hardcoded give-up timeout) before reporting
success.

**Cause**: same shape as #3. `waitpid_status` issues Akuma's own `WAITPID`
syscall (303), also unhandled on real Linux; every call fell into the
function's own "no such child" branch, which is indistinguishable from "child
still running" in its return type. `drain_child`'s polling loop therefore
could never observe a fast-exiting child as finished and always ran to its
timeout, even though the child (and the pipe read that captured its output)
had completed within milliseconds.

**Fix**: added a `#[cfg(feature = "linux-abi")]` `waitpid_status` built from
real `wait4(pid, &status, WNOHANG, NULL)` — non-blocking on the *specific*
pid, preserving the existing function's "`None` = still running" contract
that `drain_child` already relies on. `userspace/libakuma/src/lib.rs`.

**Verified**: the same `uname -a` call now completes in well under a second
end to end, not 30 seconds.

## 5. Not fixed: infinite retry on a permanently-oversized request

**Symptom**: when the model provider rejects a request because it exceeds
the server's context window (`request (8847 tokens) exceeds the available
context size (8192 tokens)`), meow retries the *identical* request forever —
observed nine retries with growing backoff before the run was killed
manually.

**Cause**: meow doesn't know the provider's actual context window (it assumes
`DEFAULT_CONTEXT_WINDOW` unless told otherwise) and has no "this class of
error will never succeed on retry" classification — a context-size rejection
gets the same treatment as a transient network error.

**Status**: worked around for the litter experiment by raising the test
`llama-server` instances' `-c` to 32768. The underlying retry-forever-on-a-
permanent-error behavior in `api::client::send_with_retry` is unfixed —
flagging it here rather than losing it, since it's a real robustness gap for
any unattended (e.g. litter) agent that reads a large file early in a
session.

## Investigated, not a bug: nested `fork()` from inside an already-forked child

Not a bug found in shipped code — a parallel tool-call dispatch was
prototyped (fork a child per `Shell`/`HttpFetch` call in `app::chat`, in
parallel with `Shell`'s *own* internal `fork()` from fix #3) and reverted
after the outer child was observed to die before writing its result to its
handoff tempfile, in every run attempted in this session's Docker/Alpine
environment. Not root-caused (candidates: the custom chunked allocator not
being fork-safe across a second, nested fork; something else specific to
nested `fork()` under this kernel/container combination) and deliberately
not shipped enabled-by-default in that state. See the comment left in
`app/chat.rs` where the code used to be, and revisit as its own investigation.
