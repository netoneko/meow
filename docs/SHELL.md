# Shell Tool

meow's `Shell` tool (`src/tools/shell.rs`) runs a command line and returns its
stdout + exit code to the model.

## How it works

The tool routes the command line through a real shell so shell grammar works:

```rust
spawn("/bin/busybox", ["sh", "-c", command])
```

busybox multiplexes on `argv[0]`'s basename (`busybox`), runs the `sh` (ash)
applet, and `-c command` is interpreted with full shell semantics: `&&`, `||`,
`|`, `;`, `>`, `<`, globbing, `$VARS`, subshells.

If `/bin/busybox` is **not** present, the tool falls back to a legacy path:
tokenize on whitespace (quote-aware) and exec the first token directly via
`resolve_binary` + `run_and_capture`. In fallback mode **no operators work** —
the command is a single program invocation.

Output handling (`run_and_capture`): drains child stdout with a 30s timeout and
a 1 MB output cap (the child is killed if either is exceeded).

## The bug this fixed

Previously `tool_shell` was **not a shell**. It tokenized on whitespace and
exec'd `tokens[0]` directly with `tokens[1..]` as argv. Any shell operator was
passed through as a literal argument. For example:

```
tcc -o /tmp/h hello.c && /tmp/h
```

tokenized to `["tcc","-o","/tmp/h","hello.c","&&","/tmp/h"]`, so meow exec'd
`tcc` with `&&` and `/tmp/h` as **extra arguments**. tcc tried to treat `&&` as
an input filename and failed; the `&& /tmp/h` chain never ran. This was a bug in
meow's tool, **not** in the in-kernel shell (`akuma:/>`), which parses `&&`
correctly via `execute_command_chain`.

## Requirement: busybox on disk

busybox is **not** installed by `apk` at runtime (apk crashes on the extreme
kernel during its post-install fork — see Known issues). Instead a fully static
binary is shipped offline:

- Source: Alpine `busybox-static` (static-pie aarch64 — same format as `apk`/
  `tcc`; the system has no musl dynamic loader, so a dynamically-linked Alpine
  busybox would NOT load).
- Staged at `bootstrap/bin/busybox`, written to `disk.img` by
  `scripts/populate_disk.sh` (`--bin-only` for a fast `/bin` refresh).

## Verified behaviour (extreme profile)

Tested by spawning `/bin/busybox sh -c "..."` (the exact call the tool makes):

| Case | 7 MB | 6 MB |
|------|------|------|
| busybox execs, applets run | ok | ok |
| `sh -c "echo a && echo b"` (`&&`) | ok | ok |
| fork+exec chain (`true && echo OK`) | ok | ok |
| `tcc hello.c && /tmp/h` (compile + run) | ok | OOM* |
| tcc compile alone / run alone | ok | ok |

No kernel exceptions in any case. busybox forks cleanly on the extreme kernel.

\* At 6 MB the *chained* compile-and-run hits `Failed to load ELF: Out of memory
for user page`: busybox sh + tcc's process tree + loading the freshly-built
third ELF are all live at once, exceeding the ~2.9 MB user-page pool. It fails
gracefully (no crash); the same steps run fine **separately** at 6 MB. This is a
memory-capacity ceiling, not a tool defect.

## Known issues

- **apk on the extreme kernel.** `apk` downloads/extracts/installs fine but
  crashes the kernel when it forks post-install triggers (`EC=0x25` data abort
  at fork "step7: spawning child thread", wild `x8` base). Suspected lazy
  thread-stack ENOMEM going unchecked in the fork path on `kernel_profile_size`.
  This is why busybox is installed offline. Open kernel issue.

- **`&&` does not short-circuit on a failed exec.** When a child fails to load
  (e.g. the 6 MB ELF OOM above), the failure is not surfaced as a non-zero exit
  to the parent shell, so a following `&& cmd` still runs. Pre-existing kernel/
  spawn behaviour, unrelated to the Shell tool itself.

- **In-kernel shell quoting (testing only).** When invoking
  `busybox sh -c "..."` from the `akuma:/>` SSH prompt, the in-kernel shell can
  leak the closing quote into the last token (e.g. `echo X2"`). This is an
  artifact of the in-kernel shell's quote handling during manual testing, not
  meow — meow passes the command string to busybox as a single `-c` argument.
