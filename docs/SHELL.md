# Shell Tool

meow's `Shell` tool (`src/tools/shell.rs`) runs a command line and returns its
stdout + exit code to the model.

## Default: the in-process "pretend shell"

By default (`config::USE_PRETEND_SHELL = true`) the tool routes the command line
through meow's own mini-shell in `src/tools/pretend_shell.rs` — **no external
shell binary is involved**.

Why "pretend"? The kernel's `spawn` syscall only hands back the child's stdout
fd; there is no `dup2` to wire a child's stdout into an arbitrary fd *before*
exec. So meow can't plumb file descriptors like a real shell. Instead it
*pretends*: it parses the operators itself, runs each command, captures the
stdout `spawn` already gives it, and re-writes that output to a redirect
backend.

### Supported grammar (deliberately minimal)

| Form | Meaning |
|------|---------|
| `cmd1 && cmd2` | run `cmd2` only if `cmd1` exited `0` |
| `cmd1 \|\| cmd2` | run `cmd2` only if `cmd1` exited non-zero |
| `cmd > target` | truncate-write `cmd`'s stdout to `target` |
| `cmd >> target` | append `cmd`'s stdout to `target` |

`&&`/`||` are left-associative with equal precedence (matching POSIX sh for
these two operators). Words are quote-aware (`'...'`, `"..."`, `\` escapes).
There is **no** piping (`|`), `;`, globbing, or `$VARS` — by design. "Basic
commands, nothing fancy."

### Redirect backends: files and sockets

A redirect `target` is resolved to one of two sinks (`write_to_sink`):

- **File** — any plain path. Resolved within the meow sandbox via
  `context::resolve_path`, then opened `O_WRONLY|O_CREAT` plus `O_TRUNC` (`>`)
  or `O_APPEND` (`>>`).
- **Socket** — a `tcp:HOST:PORT` target (e.g. `echo hi > tcp:10.0.2.2:4000`).
  meow opens an `AF_INET`/`SOCK_STREAM` socket, `connect`s, streams the captured
  stdout with `send`, then `shutdown(SHUT_WR)` + `close`.

A failed redirect (sandbox violation, connect failure, write error) marks the
command as failed so a following `&&` short-circuits correctly.

### Execution & reporting

Commands run left to right with short-circuit semantics carried via the last
exit code. Non-redirected stdout is accumulated into the report returned to the
model; redirected stdout is replaced by a `[cmd > target] wrote N bytes` note.
The reported exit code is that of the last command that actually ran.

The per-child capture loop (`shell::spawn_and_collect`) drains stdout with a 30s
timeout and a 1 MB cap (the child is killed if either is exceeded).

## Fallback: busybox (`USE_PRETEND_SHELL = false`)

Set the flag false to restore the legacy path: route the line through a real
shell if one is installed —

```rust
spawn("/bin/busybox", ["sh", "-c", command])
```

busybox multiplexes on `argv[0]`'s basename, runs the `sh` (ash) applet, and
`-c command` is interpreted with full shell semantics: `&&`, `||`, `|`, `;`,
`>`, `<`, globbing, `$VARS`, subshells. If `/bin/busybox` is absent, it falls
back further to tokenizing on whitespace and exec'ing the first token directly
(no operators).

### Shipping busybox (only needed for the fallback)

busybox is **not** installed by `apk` at runtime (apk crashes on the extreme
kernel during its post-install fork — see Known issues). A fully static binary
is shipped offline instead:

- Source: Alpine `busybox-static` (static-pie aarch64 — same format as `apk`/
  `tcc`; the system has no musl dynamic loader, so a dynamically-linked Alpine
  busybox would NOT load).
- Staged at `bootstrap/bin/busybox`, written to `disk.img` by
  `scripts/populate_disk.sh` (`--bin-only` for a fast `/bin` refresh).

With the pretend shell as the default, this offline staging is **optional** —
only required if you flip `USE_PRETEND_SHELL` off.

## The bug this design fixes

Previously `tool_shell` was **not a shell**. It tokenized on whitespace and
exec'd `tokens[0]` directly with `tokens[1..]` as argv. Any shell operator was
passed through as a literal argument. For example:

```
tcc -o /tmp/h hello.c && /tmp/h
```

tokenized to `["tcc","-o","/tmp/h","hello.c","&&","/tmp/h"]`, so meow exec'd
`tcc` with `&&` and `/tmp/h` as **extra arguments**. tcc tried to treat `&&` as
an input filename and failed; the `&& /tmp/h` chain never ran. The pretend shell
parses `&&` correctly and short-circuits on `tcc`'s exit code.

## Known issues

- **`&&` does not short-circuit on a failed exec.** When a child fails to *load*
  (e.g. ELF OOM under a tight memory profile), the kernel/spawn path does not
  always surface a non-zero exit to the parent, so a following `&& cmd` may
  still run. Pre-existing kernel/spawn behaviour, independent of which shell
  path is used. (The pretend shell does report exit `127` when `spawn` itself
  returns no child.)

- **apk on the extreme kernel.** `apk` downloads/extracts/installs fine but
  crashes the kernel when it forks post-install triggers (`EC=0x25` data abort
  at fork "step7: spawning child thread", wild `x8` base). Suspected lazy
  thread-stack ENOMEM going unchecked in the fork path. This is why busybox is
  installed offline. Open kernel issue.
