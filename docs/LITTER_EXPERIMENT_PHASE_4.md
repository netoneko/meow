# Litter experiment — Phase 4: real hardware joins a laptop's hub (2026-09-21)

Status: session narrative, same genre as Phase 0-3. Where those phases ran
every agent inside one container on one machine, this phase is the first to
split the litter across **two physically separate hosts on the LAN**: the
Ryzen laptop (`pop-os`, plain Linux, Ollama) as hub/leader, and the real
Akuma kernel running on the HP trashcan box as a single joining peer.

Not to be confused with the earlier, unrelated attempt in
`docs/archive/LITTER_TRASHCAN_RYZEN_JOIN.md`, which tried to join **two
Akuma-kernel litters** (trashcan bare metal + a Firecracker Akuma guest on
Ryzen) via the cross-litter relay and hit an unfixed kernel bug (a spawned
thread that starts and never runs its body, blocking the Firecracker guest's
own hub). This phase avoids that failure mode by construction: Ryzen runs
`meow` as an ordinary **Linux** process (not inside an Akuma guest), so it
never needs the broken thread-spawn path, and the trashcan's Akuma kernel is
only ever a **client** connecting out — it never has to serve a listening
socket itself.

## Goal

1. A bigger model on Ryzen (Ollama, already running several) backs a `meow`
   agent that acts as hub **and** leader — the swarm's "sync source".
2. A single `meow` instance on the real, already-booted Akuma kernel (the HP
   box, reachable as `ssh akuma`) joins that hub as an ordinary peer.
3. Prove the akuma-hosted agent can be **commanded** — sent a real task over
   the litter protocol and observed doing something.

## Topology chosen, and why

- **Hub + leader (`mimi`) on Ryzen**, plain `x86_64-unknown-linux-musl` build
  of `meow` (the same `linux-net`/`linux-abi` compatibility path Phase 0-3
  used for Docker), run as a native process — no container needed, Ryzen is
  already a normal Linux host. Model: `gemma3:27b` (17 GB, the largest local
  model Ryzen has), via its own already-running Ollama at `127.0.0.1:11434`
  — no new exposure needed since the agent and the model share one host.
- **Peer (`akuma`) on the real HP box**, using the `/bin/meow` already staged
  there (native Akuma amd64 build), joining Ryzen's hub at
  `192.168.1.126:7700` over the LAN.
- The akuma-side agent's own model backend is **this laptop's MLX server**
  (`mlx-community/Qwen3-Coder-30B-A3B-Instruct-4bit`, already listening on
  `0.0.0.0:8080` from the prior session's test), reachable from the akuma
  host directly (`192.168.1.203:8080`, confirmed with `wget` before wiring
  the config). This was the deliberate alternative to exposing Ryzen's
  Ollama externally: rebinding a systemd-owned Ollama needs `sudo` Ryzen's
  plain user account doesn't have non-interactively, and opening a new port
  on Ryzen for this is scope the operator didn't ask for — the auto-mode
  classifier declined that action and the MLX route sidesteps the question
  entirely by reusing a service that was already exposed.

## What was actually broken

Four defects, all found by trying to run the thing rather than by reading it
— same lesson as every prior phase.

### 1. `x86_64-unknown-linux-musl` links `static-pie` by default — segfault before `main`

The Phase 0-3 cross-build recipe (`-C linker=<triple>-gcc -C
link-self-contained=no -C link-arg=-nostartfiles`) was proven only on
`aarch64-unknown-linux-musl`. Applied unchanged to the `x86_64` target, the
linker produces a `static-pie` binary — and this runtime's raw `_start`
(built with `-nostartfiles`, deliberately skipping musl's normal `crt1.o`)
never performs the self-relocation a PIE binary needs before touching any
global. Every run crashed instantly: `AFTER_RC=139` (SIGSEGV), zero bytes of
output, not even `--help`.

```
$ file meow
meow: ELF 64-bit LSB pie executable, x86-64, ... static-pie linked, stripped
```

Fixed by forcing a plain static, non-PIE link:

```bash
RUSTFLAGS="-C linker=x86_64-linux-musl-gcc -C link-self-contained=no \
  -C link-arg=-nostartfiles -C relocation-model=static \
  -C link-arg=-no-pie -C link-arg=-static" \
  cargo +nightly build --release -Zbuild-std=core,alloc \
    -Zbuild-std-features=compiler-builtins-mem \
    --target x86_64-unknown-linux-musl
```

```
$ file meow
meow: ELF 64-bit LSB executable, x86-64, ... statically linked, stripped
```

After that, `--help` and `-c` both work immediately. **The aarch64 recipe is
not portable to x86_64 as-is** — add the relocation-model/no-pie/static
flags for every future x86_64-linux-musl build of this binary, or add a
`compile_error!`-style build-time check the way `raw_write`'s arch gate
already does for asm.

### 2. `MEOW_HOME` scoping doubles an absolute path when `CWD != /`

Every prior phase's launch convention was `cd / && MEOW_HOME=... meow litter
live`, and `litter/yard_init.sh`'s own comment says why: "every agent still
runs with CWD=/ so the file-tool sandbox can read /akuma-src." It turns out that line is load-bearing for a second,
undocumented reason: launched from `cd /home/netoneko/litter &&
MEOW_HOME=/home/netoneko/litter/agents/mimi meow litter live`, the config
landed at

```
/home/netoneko/litter/home/netoneko/litter/agents/mimi/etc/meow/config
```

— CWD prepended to an already-absolute path. `config::scoped()` (or
whatever underlies the `open()` call under `linux-abi`) does not recognize a
leading `/` as absolute; it concatenates `MEOW_HOME` onto `CONFIG_PATH`
faithfully, but then the syscall shim additionally resolves the result
relative to `CWD`. In every previous phase `CWD` was always `/`, so the same
bug would have produced `//etc/meow/config` — harmless, since multiple
leading slashes collapse — which is exactly why nobody hit this before. The
practical effect here: the agent silently wrote its signing key to a
throwaway path nested under itself, `Config::load()` at startup read a file
that had our `litter_agent_name` line but the *next* process (after a
restart) would have read nothing, and `live::run()`'s
`litter_agent_name must be set` check fired on a config that, read straight,
plainly had the key.

**Not yet fixed in source** — worked around operationally by always running
`cd /` before setting `MEOW_HOME`, matching the existing yard convention.
Worth a real fix in `libakuma`'s path handling under `linux-abi`: an
absolute path must never be prefixed with CWD.

### 3. `pkill -f <pattern>` can kill its own invoking shell over a one-line ssh command

Any command of the shape
`ssh host 'pkill -f "litter live"; ...; meow litter live'` is self-defeating:
the whole script is what `sshd` execs as `bash -c "<the entire string>"`, so
`ps` shows that literal string — including the word `litter live` from the
*trailing* launch command — as `pkill`'s own parent shell's command line.
`pkill -f` matches it, kills its own ancestor, and `ssh` reports an opaque
`exit 255` with **no output at all**, indistinguishable at a glance from a
dropped connection. Cost real time here: three different multi-line launch
attempts were misdiagnosed as connectivity or backgrounding problems before
the actual cause (a self-matching pattern) was found by testing a bare
`pkill`/`ps` pair in isolation.

Fix is procedural, not code: never combine a `pkill -f PATTERN` with a
command in the *same* remote invocation whose own text contains `PATTERN`.
Split into separate `ssh` calls.

### 4. `kill -9` on `meow litter live` reliably wedges the box's networking — REPRODUCED

With Ryzen's hub confirmed listening (`ss -tlnp` showed `0.0.0.0:7700`,
reachable), the akuma-hosted agent's first join attempt raced the hub's own
startup and got `ConnectionRefused` — expected, same as every other phase's
"could not reach hub" line before the bind race resolves. The retry needed
killing a duplicate stray process first (`kill 49 57`); moments after that
`kill`, the box stopped answering **anything** — no ssh (port 2222), no
ICMP.

At the time this read as possibly a recurrence of
`docs/archive/LITTER_TRASHCAN_RYZEN_JOIN.md` §2.1's RTL8169 `Silent` stall
(already fixed once). The operator power-cycled the box (physical, no
remote power control exists — confirmed against
`docs/runbooks/amd64-bare-metal-loop.md`), it came back in 32s, and the
join was retried. First retry also got `ConnectionRefused` — this time
confirmed, via `wget` from the akuma box itself, that the refusal was
**not** a real network-level rejection: `wget` to Ryzen's real ssh port 22
connected fine (reset by sshd on garbage HTTP, as expected), and `wget` to
the hub port 7700 itself got past `Connecting to...` and hung waiting for
an HTTP response — i.e., the TCP handshake to 7700 succeeds from userspace
tooling on the same box, at the same time meow's own `TcpStream::connect`
to the identical address reports `ConnectionRefused` (errno 111, taken at
face value from `sys_connect`'s return in `libakuma/src/net.rs`). One
plausible read: a connect() issued before ARP for a brand-new peer has
resolved gets a spurious `ECONNREFUSED` from Akuma's smoltcp stack instead
of blocking/retrying — untested directly, and now moot given what happened
next.

To rule that out, the stray `/bin/meow litter live` process was
`kill -9`'d to relaunch cleanly. **The box went dark again, immediately** —
no ssh, no ICMP, identical signature to the first wedge. This is now a
**reproduction, not a coincidence**: both times the box went unreachable,
it was within seconds of a `kill -9` on this exact process, and both times
recovery required (or is expected to require) a physical power cycle.
`meow litter live` on amd64 spawns a second thread (the raft owner thread,
`docs/archive/LITTER_TRASHCAN_RYZEN_JOIN.md`'s "spawned thread never runs"
territory) and holds an open socket to a peer that, from the kernel's own
`sys_connect` return, is in a slightly wrong state (the ECONNREFUSED
mismatch above) — `SIGKILL`ing a process astride an in-progress connect
and a second thread is exactly the kind of corner this kernel's amd64 port
has repeatedly gotten wrong in the SMP/thread-teardown paths documented
elsewhere in this tree (`AMD64_SWITCH_FREED_CR3_UAF`, the context-switch
page fault under BKL, the orphan/no-slot-recycler class). ~~**Do not `kill
-9` a `meow litter live` process on the real amd64 box** until this is
root-caused; use a clean shutdown path (a signal `meow` actually handles,
or let it exit on its own) instead.~~ **(2026-09-21: root-caused and fixed —
`docs/archive/AMD64_SMOLTCP_STALE_HANDLE_KILL9_WEDGE.md`. The kill path was
never at fault; a stale smoltcp socket handle left behind by the teardown the
kill triggered panicked inside `akuma-net`. The warning above is retired once
the fixed kernel is deployed to the box.)** The operator confirmed independently
that `kill -9` "does not work for unknown reason" on this box and is
handing that specific investigation to another agent — this phase treats
it as someone else's open item rather than re-chasing it, and simply avoids
`kill -9` on `meow` processes here going forward.

### 4b. Root cause of the wedge, from a photo of the console — REAL kernel panic

**SOLVED 2026-09-21** — this section's read was correct, and the full record
is [`docs/archive/AMD64_SMOLTCP_STALE_HANDLE_KILL9_WEDGE.md`](../../../docs/archive/AMD64_SMOLTCP_STALE_HANDLE_KILL9_WEDGE.md):
the stale handle was used by the **blocking syscall paths** in
`akuma-net/src/socket.rs` and `udp_api.rs` (the `poll()` sweeps and the async
connect path had guarded themselves; these had not). Every raw
`net.sockets.get*` there now goes through guarded accessors that answer
`None` for a dead handle, mapped onto the fallbacks those call sites already
carried. Verified in QEMU/TCG at SMP=4 with a 10-round spawn-`meow`-`kill -9`
loop: no wedge, no panic, ssh alive throughout. What remains is deploying the
fixed kernel to the box.

The third occurrence (after `meow-live` was disabled, no `kill -9` involved
this time — the box went dark on its own, ~5 minutes after akuma's agent
settled into the bogus self-bind from §4/§6) left a panic on the physical
screen, photographed by the operator:

```
[PANIC] .../smoltcp.../<socket set path>
        handle does not refer to a valid socket
```

This is **not a hang** — it is smoltcp's own internal invariant check
failing when something calls it with a `SocketHandle` the `SocketSet` no
longer holds (the socket was already removed). On bare metal a panic halts
the core, which is exactly the "no ssh, no ICMP, completely dark" signature
every wedge this session showed. So: three wedges, one root cause, and it
is a **stale smoltcp socket handle**, somewhere in Akuma's amd64 networking
glue (`akuma-net`/`amd64/src/sock.rs`) — a handle survives past the point
its socket was `remove_socket()`'d and gets used again (a retried connect,
a probe tick, or the bind-race's own bookkeeping are all candidates; not
yet isolated to one). This reframes both `kill -9`-adjacent wedges: killing
the process most likely just *triggered the cleanup path* that hit the
stale handle rather than being the direct cause, which also explains why
the third wedge needed no kill at all — the same cleanup/retry path can be
reached by ordinary operation (a failed connect being retried, or a probe
tick running after a socket the config's fallback-bind logic already
touched). **This is the priority finding for whoever picks up the `kill -9`
investigation** — the actual bug is a socket-handle lifetime violation in
the kernel's networking layer, not anything meow-specific or kill-signal
specific.

### 5. The deployed `/bin/meow` predates `linux-net` becoming default

Worth ruling out as a confound: `/bin/meow` on the box is **418904 bytes**,
an exact size match to a locally cached build artifact at
`target/x86_64-unknown-none/release/build/meow/48f8636f1ae0ccbb/out/meow`
built in the window between commits `ae2eb6c` ("more meow fixes", 2026-09-20
05:30 +0300) and `86712b4` ("oof", 06:43) — **13.5 hours before `82b63c0`**
("use linux-net for meow by default", 18:55 the same day) made `linux-net` a
default feature. So the binary this phase has been driving is a
**native-Akuma-ABI build**, not a `linux-net`/Linux-syscall-shim one; the
Phase 3 class of `linux-abi`-vs-real-kernel mismatch (the `nanosleep` bug,
etc.) does not apply to it. What *is* plausible: this old build, launched
into `litter live` with a config pointing at a genuinely reachable remote
peer, may be exercising a cross-host connection this exact binary has never
completed successfully before —
`docs/archive/LITTER_TRASHCAN_RYZEN_JOIN.md`'s own prior session on this
box never got a message to cross ("`Inbox for 'panther' is empty` after two
sends"). Not yet rebuilt against current `HEAD` to compare; worth doing once
the wedge itself is understood, so a rebuild doesn't get credited for a
kill-path fix that was actually the cause.

## State left behind

- **Ryzen**: `mimi` running natively at `/home/netoneko/litter/meow`
  (`MEOW_HOME=/home/netoneko/litter/agents/mimi`), holds the hub at
  `0.0.0.0:7700`, leader (term 1), backed by `gemma3:27b` on its own Ollama.
  Log: `/home/netoneko/litter/agents/mimi/log`.
- **This laptop**: MLX server unchanged, still serving
  `mlx-community/Qwen3-Coder-30B-A3B-Instruct-4bit` on `0.0.0.0:8080`.
- **`~/.ssh/config`**: gained a `ryzen` host entry (root, key-based) —
  `scripts/utils/hpbox.py`'s existing `RZ`/`ryzen()` helper still uses the
  plain `netoneko@` account and is unaffected.
- **The HP box**: wedged **twice** this session, both immediately after a
  `kill -9` on `/bin/meow litter live` (PIDs 49/57 the first time, PID 32
  the second). Between the two wedges the operator power-cycled it once
  (physical; recovered in 32s) and this session confirmed the join attempt
  itself gets a spurious `ConnectionRefused` against a verifiably-listening
  peer before the second `kill -9` was issued. `/etc/meow/config` there
  points at Ryzen's hub and this laptop's MLX server; unresolved whether
  that config or state is intact after the *second* wedge, since the box
  was still down when this doc was last updated. The operator has since
  **disabled `/bin/meow-live`** (the pre-existing, unrelated stuck litter —
  see below) and is rebooting again, removing that confound going forward.
  Needs another physical
  power cycle. The pre-existing, unrelated `/bin/meow-live` (a stuck,
  years-old 4-agent litter under `sherlock`/`zenigata`/`hercules`/`watson`,
  permanently parked at `stage=1` across many reboots — see "A pre-existing,
  unrelated litter was already on the box" below) was still there after the
  first reboot and is presumably still there now.

## A pre-existing, unrelated litter was already on the box

Not part of this phase's work, but worth recording so a future session
does not mistake it for something related: the akuma box already runs
`/bin/meow-live litter live` (a *different* binary from the `/bin/meow`
this phase pushed), supervised so it restarts on every boot, with four
agent homes under `/agents/{sherlock,zenigata,hercules,watson}` and a
top-level `raft.log` per agent. `sherlock`'s alone is 6570 lines, spanning
many reboots (its own line-numbered timestamps reset each boot and its
content is appended across them). Inspected because the operator, on
returning, asked what "their conversation" had covered — but the log shows
no conversation: every agent is permanently stuck at `agent tick N sees
RAFT_TICKS=0 alive=true stage=1`, never advancing past bootstrap. This
matches `docs/archive/LITTER_TRASHCAN_RYZEN_JOIN.md`'s still-open finding
(a spawned thread that starts and never runs its body) exactly — this is
that same broken deployment, left running and silently failing across
however many reboots have happened since 2026-09-20. It shares the box's
network stack with whatever this phase runs, and section 4 above cannot
yet rule it out as a contributing factor to the wedge.

## Open

- **The akuma↔ryzen join was never confirmed** — both attempts ended with
  the box going dark before a join event reached either side's log.
- **`kill -9` on `meow litter live` reliably wedges the box — reproduced
  twice, ~~root cause unknown~~ SOLVED 2026-09-21.** Root cause and fix:
  `docs/archive/AMD64_SMOLTCP_STALE_HANDLE_KILL9_WEDGE.md` (a stale smoltcp
  socket handle dereferenced by the blocking socket-syscall paths after a
  sibling thread's teardown closed the fd — see §4b). Remaining work:
  deploy the fixed kernel to the box and confirm the wedge is dead on the
  metal; until then the "do not `kill -9` meow" rule stands.
- **No remote power control for this box** — confirmed against
  `docs/runbooks/amd64-bare-metal-loop.md`; recovery is "hold the power
  button ~5s, or pull the plug," physical actions this session cannot
  perform. The first wedge needed one; the second wedge (at the time of
  writing) has not yet been resolved.
- **The step-3 goal (command the akuma agent, get a report) was not
  reached.** Once a clean, `kill -9`-free way to manage the akuma agent's
  lifecycle exists and the join is confirmed, open a parent task from the
  operator asking the same canonical question Phase 0's example used —
  "debate if this codebase even works and produce a report" — and watch
  whether the akuma-hosted agent, backed by a real coding model over the
  LAN, can execute tool calls against its own live filesystem.
- The spurious `ConnectionRefused` against a confirmed-listening peer
  (section 4) is itself worth root-causing independent of the wedge — if
  it is a real smoltcp/ARP-timing bug, every phase's "could not reach hub,
  won the bind race instead" path is quietly demoting a joinable litter to
  a lonely one on the first connect attempt after a fresh boot.
- Whether the pre-existing, unrelated `/bin/meow-live` litter (four agents,
  permanently stuck — see above) shares any responsibility for the wedge
  by competing for the box's network stack is untested; the cleanest next
  repro is single-`meow`-process only.

## §6: the static-peer relay is blocked by the same bug Phase-adjacent work already found

With the config corrected (§5's fix applied for real this time: `akuma`
binds `127.0.0.1:7700` locally and lists Ryzen as `litter_static_peers`,
the way `LITTER_RELAY_TOPOLOGY.md` describes, instead of pointing its own
`litter_hub_addr` at Ryzen's IP), the akuma-hosted agent correctly became
its **own** local hub — no more foreign-IP bind. But the cross-litter
relay to Ryzen never happened, and the reason is visible directly:

```
$ ssh akuma 'wc -l /tmp/meow/raft.log; ps aux | grep 28'
18 /tmp/meow/raft.log
   28 0         0:02 /bin/meow litter live
# ... waited, checked again ...
18 /tmp/meow/raft.log
   28 0         0:04 /bin/meow litter live
```

The line count never grows past the initial `start` / `state=leader`
lines — no `owner tick N top`, no `probed (…)`, no `drained (…)`, the
periodic lines `mimi`'s raft log on Ryzen is full of. CPU time crept from
0:02 to 0:04 once and then flatlined; the process is alive but its owner
thread's actual loop body is not running. This is **not a new bug** — it
is the exact signature `docs/archive/AMD64_SPAWNED_THREAD_NEVER_RUNS.md`
already documented for the original trashcan↔ryzen join a day earlier: the
thread starts, performs its one-time setup, and never reaches its first
loop iteration. Static-peer probing happens inside that same loop
(`owner tick N top` → `tick N probed (…)` in `mimi`'s log is literally
where a peer probe would fire), so **no config on either side can make
this relay work until that thread bug is fixed** — this was never a
reachability or address-format problem downstream of it.

## §7: narrowed further — it is the *native* thread-spawn path, not amd64 as a whole

Challenged (rightly) on §6: is this really the same bug, given the native
`/bin/meow` and Ryzen's `linux-net` build are not remotely the same binary
(different commit, different target, different syscall path)? Tested
directly rather than assumed — pushed Ryzen's exact `linux-net`
(`x86_64-unknown-linux-musl`) binary onto the **akuma box itself** under a
different name (`/bin/meow-linuxnet`), with its own isolated config
(`MEOW_HOME=/tmp/lntest`, port 7701, no interaction with the native
process already running on 7700). Same kernel, same physical hardware,
only the binary's syscall path differs.

**Its owner thread ticks perfectly:**

```
[763] tick 1 drained (served=1)
[764] owner tick 2 top
[765] tick 2 probed (0 results)
[765] tick 2 drained (served=3)
... (continues normally, tick count climbing, confirmed still advancing
     after several more checks — 18 → 27 → 31 raft.log lines over time)
```

Same box, same amd64 core, same kernel binary the native `/bin/meow` (still
running alongside on port 7700, still stuck at zero ticks) shares. The only
variable is the syscall path: `linux-net` routes thread creation through
Akuma's Linux-ABI-compatible amd64 syscall dispatch (whatever `clone`-style
call that resolves to); the native build calls `litter_spawn_thread` — an
`extern "C"` FFI import in `src/rt.rs:179` — straight into Akuma's own
native thread-creation primitive.

**So "spawned thread never runs its body" is not an amd64-wide limitation —
it is specific to Akuma's native (non-`linux-net`) thread-spawn path on
amd64.** The Linux-ABI path spawns a thread that actually executes fine on
the exact same hardware. That is a much sharper lead than the archive doc
had: whoever picks this up should diff what `litter_spawn_thread`'s native
implementation does against whatever the `linux-net`/`clone`-syscall path
does for thread creation on amd64 — the difference between "starts and
never runs" and "runs and ticks correctly" lives in that gap, not in
anything broader about the platform.

**Practical unblock, independent of the kernel fix:** a `meow` built with
`linux-net` on amd64, running natively on the real box (not cross-compiled
for a different OS — just a different feature flag), sidesteps this bug
entirely today. If commanding the akuma agent is the near-term goal, that
is the path — build `/bin/meow` fresh with `linux-net` (default since
`82b63c0`) targeting `x86_64-unknown-linux-musl`, not `x86_64-unknown-none`,
and stage that instead of the current native binary.

This closes out this phase's own investigation. Three kernel-side findings
handed to whoever picks up the networking/threading work: the native
amd64 thread-spawn bug is now scoped to a specific code path (§7, sharper
than the archive's original finding); ~~the `kill -9` wedge traces to a real
`smoltcp` panic — a stale socket handle (§4/§4b)~~ **the `kill -9` wedge was
root-caused to that stale socket handle and fixed 2026-09-21 —
`docs/archive/AMD64_SMOLTCP_STALE_HANDLE_KILL9_WEDGE.md`; §4b's "do not
kill -9 meow" caution is retired once the fixed kernel reaches the box**;
and the
`ConnectionRefused`-for-timeout mislabeling was already flagged in
`AMD64_PROXY_ARP_UNREACHABLE.md` §4, not new. Nothing further on the
`meow` config or protocol side is expected to move any of these until they
land — except the `linux-net` workaround above, which needs no kernel fix
at all.

## §8: four `meow`-side bugs found and fixed this round

With the `linux-net` build deployed as `/bin/meow` on akuma (§7's workaround),
sending it a real task ("does this codebase even work") surfaced problems in
`meow` itself, not the kernel — found live, fixed, redeployed to both boxes.

**1. Repeated identical tool calls had no loop-guard.** The exact loop from
earlier in this session (`python3 -c "import akuma; ..."`, retried 10+ times
verbatim, always `Exit code: 127` — there is no Python on Akuma) had nothing
stopping it short of `MAX_TOOL_ITERATIONS`. Fixed in `chat_once`
(`src/app/chat.rs`): a call repeated with byte-identical arguments a third
time is refused outright — no execution, no wasted model turn — with a
tool-result message telling the model plainly it is not allowed and to think
of something else, rather than a bare failure it might read as "try again".

**2. Every wake started a genuinely fresh, empty conversation.** `run_turn`
called `Conversation::new_session` — `O_TRUNC`, id regenerated — on *every*
wake, so a resident agent relearned everything from zero each time it was
nudged, restarted, or rebooted; the loop-guard above only bounded the damage
*within* one wake, not across the many wakes a stuck task produces overnight.
Fixed with `Conversation::resume_or_new` (`src/app/history.rs`) and a stable
per-agent id (`session::live_session_id()`, literally `"live"` — sessions are
already `MEOW_HOME`-scoped, so no per-agent-name id was needed). The
conversation is now built **once**, outside the main loop, and only ever
reset by the same local `auto_compact_if_needed` threshold that already
governed the interactive path — not by every wake, and not by any
cluster-level signal either, matching the design intent this was checked
against: compaction is a local decision a cluster event can inform, never an
outside authority that truncates out from under it.

**3. The raft log was never rotated.** `O_APPEND` forever, by design
("appended across restarts") — measured at 400+ KB after about a day for an
agent that was permanently stuck doing nothing but tick (the pre-existing
`sherlock` litter, §"A pre-existing, unrelated litter"). Fixed with a 4 MB
cap (`RAFT_LOG_CAP_BYTES`) checked every 10 ticks: past the cap, the log is
rotated by reopening the same path with `O_TRUNC` and atomically swapping the
shared fd — no `ftruncate` on this target, so this is the same "point at the
file, not the fd" trick `set_raft_log` already used to open it the first
time. A write racing the swap lands on whichever fd was current at that
instant; losing one line to that race is the acceptable side of a trade
against unbounded growth in a debug trail, not data anything depends on.

**4. Oversized tool output spilled to orphaned, uncleaned files.** Confirmed
on disk, not just in code: real `meow_tool_<timestamp>.txt` files from past
sessions were found still sitting in `/tmp` and under `/src`, with no code
path anywhere that ever deletes one. Redesigned rather than patched, per a
design conversation mid-session:

- Every tool call — not just ones that overflow — gets a session-scoped,
  sequential id (`tools::context::next_tool_output_seq`), shown to the model
  as `[tool #N]` in its result so a later turn can reference a specific past
  call by name instead of only its own paraphrase.
- A spill file lives **inside the session directory**, named after that same
  id (`tool_7.txt` for call `#7`) — findable next to the conversation that
  produced it, and inspectable later by a human or the model
  (`FileReadLines`/`CodeSearch`, as the overflow message already suggested).
- `Conversation::reseed` — the one place old history actually gets dropped —
  unlinks every `tool_<n>.txt` up to the current count before truncating the
  conversation and zeroing the counter. No `readdir` on this target, so
  cleanup only works *because* the naming is deterministic; a resumed session
  (fix 2) probes forward from `tool_1.txt` to find where it left off rather
  than risk overwriting a file the resumed history still points at.

Net effect: tool-output files now live exactly as long as the conversation
section that references them, never longer, with no directory listing
required anywhere in the mechanism.

All four fixed, self-tested (`meow test`: same two pre-existing, unrelated
failures as baseline — `StreamingRenderer` 2/4, `litter-tasks` 14/15 — no
regressions), and redeployed to both `mimi` (Ryzen) and `akuma` (the real
box, `linux-net` build) as of this writing.
