# Allocation review: useless double-allocations in meow

**Status: OPEN — audit only, no fixes applied.** Written 2026-08-10 while
verifying `docs/MLX_SERVER_TOOL_CALLS.md`. Scope: `userspace/meow/src`
(6,468 lines), no_std + `alloc`, talc allocator, running on VMs as small as
4 MB RAM. Every avoidable allocation is a real cost here, not a style nit.

## Summary

meow is *not* architecturally careless about memory — the request path
(`api/client.rs::send_with_retry_inner`) already does the right things: it
streams the conversation log to a temp file instead of holding it in memory,
reuses the two ~17 KB TLS record buffers across retry attempts instead of
reallocating per attempt, and streams the HTTP response through a fixed
`StackBuffer` rather than building a `String`. `config::OPENAI_TOOLS_JSON` is
a `const &str`, not rebuilt per request. That discipline is good and should
be the model for the rest of the codebase.

The problem is smaller and more mechanical: dozens of call sites build a
`String` with `format!()` and then immediately copy it into *another*
allocation instead of consuming it directly. Each instance is cheap in
isolation (tens to low-hundreds of bytes), but they cluster on the tool-result
path — meaning every single tool call a model makes pays for several of these
back-to-back. None of this is currently visible in `cargo clippy` at its
default lint level; it only shows up under `-W clippy::pedantic`.

## Finding 1: `ToolResult::err(&format!(...))` — 22 sites, one alloc each

`ToolResult::err` (`tools/mod_types.rs:21-23`):

```rust
pub fn err(message: &str) -> Self {
    Self { success: false, output: String::from(message) }
}
```

takes `&str` and immediately copies it into a fresh `String`. Every call site
that wants a formatted error message does `ToolResult::err(&format!(...))` —
`format!` allocates a `String`, `&` borrows it, `err` allocates a *second*
`String` and copies the same bytes in, then the first `String` is dropped.
Two allocations and a copy to do the work of one.

22 sites, all in the tool-dispatch layer (i.e. on the path every tool call
takes):

```
tools/fs.rs:19,34,74,95,143,153,166,188,202,204,304,340,368,401,413,428  (16)
tools/net.rs:34,39,45,101                                                (4)
tools/mod.rs:100,107                                                     (2)
```

**Fix:** change the signature to take an owned `String` (or `impl
Into<String>`, so existing `err("literal")` call sites keep working via
`&str: Into<String>`):

```rust
pub fn err(message: impl Into<String>) -> Self {
    Self { success: false, output: message.into() }
}
```

then drop the `&` at each `format!` call site (`err(&format!(...))` →
`err(format!(...))`). Mechanical, ~22 one-line edits.

## Finding 2: `String.push_str(&format!(...))` — 17 sites

Same double-allocation shape, this time building up a result string
incrementally. This is exactly `clippy::format_push_string`, which only
fires under `-W clippy::pedantic` — it's silent at the default lint level,
which is presumably why 17 of these accumulated unnoticed:

```
app/commands.rs:61,89,119
code_search.rs:51,56,61
tools/fs.rs:135,353,445,448
tools/shell.rs:152,154
tools/pretend_shell.rs:293,298,313
tools/mod_types.rs:58
util.rs:13
```

`tools/fs.rs:445-448` is representative — building a diff, one `format!` +
allocation + copy per line of the edit:

```rust
for line in &old_lines {
    diff.push_str(&format!("- {}\n", line));
}
for line in &new_lines {
    diff.push_str(&format!("+ {}\n", line));
}
```

**Fix:** `String` implements `core::fmt::Write`, so this collapses to a
single in-place write with no intermediate allocation:

```rust
use core::fmt::Write;
for line in &old_lines {
    let _ = write!(diff, "- {}\n", line);
}
```

(`write!` returns a `Result` that's infallible for a `String` target — the
existing codebase already uses `let _ = write!(...)` for this in
`api/client.rs`, so the pattern is established, just not applied here.)

## Finding 3: `Write::write_str(&format!(...))` — 4 sites, same bug, different method

`tools/pretend_shell.rs:366,378,385,414` builds a shell-command report via a
custom `Write` sink (`report`) but feeds it through `format!` first instead of
writing into it directly — identical waste to Finding 2, just not caught by
`clippy::format_push_string` because that lint only pattern-matches
`.push_str`, not `.write_str`:

```rust
report.write_str(&format!("[{} > {}] {}\n", cmd.argv[0], r.target, e));
```

**Fix:** same as Finding 2 — `write!(report, "[{} > {}] {}\n", ...)` directly.

## Finding 4: `extract_field_value` — up to 48 throwaway `String`s per tool-call render

`ui/tui/stream.rs:200-205`, called from `extract_tool_info` once per rendered
tool-call notification (when a streamed `\`\`\`json ... \`\`\`` block or
balanced-brace JSON blob completes — not per character, but on a path every
visible tool call goes through):

```rust
fn extract_field_value(json: &str, field: &str) -> Option<String> {
    let patterns = [
        alloc::format!("\"{}\"", field),
        alloc::format!("'{}'", field),
        alloc::format!("{}:", field),
    ];
    for pattern in patterns {
        if let Some(pos) = json.find(&pattern) { ... }
    }
    ...
}
```

`extract_tool_info` calls this once for `"tool"` plus once per candidate
field in a 16-entry list (`filename`, `path`, `cmd`, `url`, ...) — up to
17 × 3 = 51 short-lived `String` allocations to render *one* tool-call line,
all just to search for three quoting variants of a field name that's already
known at compile time as a `&'static str` literal in the `fields` array.

**Fix:** these patterns don't need to be materialized as `String`s at all —
search for the field name directly and check what character precedes/follows
it (`json.match_indices(field)`, then check the byte before is `"`/`'`/
nothing and the byte after is the matching quote or `:`), or at minimum
build the three variants into one reused stack buffer
(`crate::util::StackBuffer`, already used elsewhere in the codebase for
exactly this kind of formatting) instead of three heap `String`s per call.

## Not flagged (reviewed, judged fine)

- `api/client.rs` request/response path — already allocation-disciplined,
  see Summary. Not touched.
- `config::OPENAI_TOOLS_JSON` — `const &str`, zero runtime cost.
- The 16 `.clone()` call sites (`main.rs`, `tui_app.rs`, `tools/shell.rs`,
  `tools/context.rs`, `app/commands.rs`, `app/history.rs`, `app/state.rs`,
  `app/chat.rs`) — all on config/input/state paths that run at most once per
  user action, not in a loop. Not worth the readability cost of avoiding.
- `tools/context.rs::get_working_dir` / `get_sandbox_root` clone a `String`
  on every call, and every `tools/fs.rs` tool function calls both via
  `resolve_path_or_err`. Individually cheap (short path strings) and calls
  are bounded by tool invocations, not loop iterations — lower priority than
  Findings 1-4, mentioned here in case someone's tracing a specific
  allocation and lands on it.
- `tools/context.rs::is_within_sandbox` — `path.starts_with(&format!("{}/",
  sandbox))` allocates a `String` just to build a prefix for one comparison,
  once per `Cd` tool call. Same shape as Finding 2/3 but low frequency;
  `path.starts_with(sandbox) && path[sandbox.len()..].starts_with('/')` would
  avoid it if someone's already in that function for another reason.

## Reproducing the counts

```bash
cd userspace/meow
cargo clippy --release -- -W clippy::format_push_string   # Finding 2 (17)
grep -rn "err(&format!" src/                               # Finding 1 (22)
grep -rn "write_str(&format!" src/                          # Finding 3 (4)
```

Clippy at its default level (`cargo clippy --release`, no extra flags) is
clean — these only surface under `-W clippy::pedantic` / `-W
clippy::nursery`, which meow's standalone `Cargo.toml` doesn't opt into
(unlike `userspace/Cargo.toml`'s `[workspace.lints]`, which meow isn't a
member of — see the "temporarily removed from the workspace" note there).

## Background

- `docs/MLX_SERVER_TOOL_CALLS.md` — the tool-call parsing bug fix that
  prompted this audit; unrelated to allocations but touched the same
  `api/client.rs` file this review clears of concerns.
- `tools/mod_types.rs` — `ToolResult`, `MAX_TOOL_OUTPUT_SIZE`,
  `handle_output_overflow` (the temp-file spill path for oversized tool
  output — already allocation-conscious, not flagged above).
