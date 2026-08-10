# Open TUI issues

User-observed, not yet root-caused with a live repro/gdb session — written from
reading the relevant code paths cold. Two unrelated issues, kept in one doc
since both were reported together 2026-08-10.

## Issue 1: Emoji rendering is broken

**Status: OPEN — root cause not yet confirmed against a live repro,** but the
code has an obvious, specific gap.

### Likely root cause

Every cursor/line-position calculation in the TUI counts **Unicode scalar
values**, not terminal display columns:

```
tui_app.rs:102   InputEvent::Delete => { if idx < input.chars().count() { ... } }
tui_app.rs:104   InputEvent::Right  => { if idx < input.chars().count() { ... } }
tui_app.rs:121,143  CURSOR_IDX.store(input.chars().count() as u16, ...)
tui_app.rs:149   InputEvent::End | InputEvent::CtrlE => { CURSOR_IDX.store(input.chars().count() as u16, ...) }
tui_app.rs:166   let len = input.chars().count();
ui/tui/input.rs:196  return input.chars().count();
```

`.chars().count()` treats every `char` as **one column wide**. Most emoji
(and CJK characters) render as **two columns wide** in a terminal. There is no
`unicode-width`-style East-Asian-Width/emoji-width table anywhere in this
crate (`grep`ped for `unicode_width`, `char_width`, `is_wide`, `display_width`
— nothing). So the moment a line contains an emoji, every cursor-position,
line-wrap, and redraw offset computed downstream of these counts is off by
however many wide characters preceded it — the classic symptom being
misplaced cursor, overlapping/garbled redraws, or characters appearing to
render in the wrong place, which matches "rendering is broken" as reported.

### Why this is a real gap, not a quick fix

This is a `no_std` binary with a hand-rolled terminal renderer (no crate
dependencies for text layout — see `crate::util::StackBuffer` and the direct
ANSI byte-writing throughout `ui/tui/`). A correct fix needs a width table
(East Asian Wide + emoji ranges, likely a `match` over `char as u32` range
buckets rather than pulling in the `unicode-width` crate, to stay
dependency-light and `no_std`-friendly) threaded through every place that
currently does `.chars().count()` for cursor/layout math — not a one-line
patch.

### Reproducing

Not yet done live. Expected: type or receive a message containing an emoji in
TUI mode and watch cursor position / line wrapping desync from the actual
text.

## Issue 2: Terminal scrollback doesn't work — mouse scroll instead cycles input history

**Status: OPEN.** Reported behavior: scrolling up in the terminal (trackpad /
scroll wheel) does not scroll back through the conversation the way normal
terminal scrollback would — instead it cycles backward through the **input
prompt history** (the same thing Up-arrow does). Wanted: standard terminal
scrollback (mouse/trackpad scroll) should scroll the conversation pane;
Up/Down arrow keys only should navigate input history. Right now both actions
appear to route to the same place.

### Root cause (two compounding causes, both confirmed in code)

**1. meow's TUI runs in the alternate screen buffer:**

```
tui_app.rs:253   akuma_write(fd::STDOUT, b"\x1b[>1u\x1b[?1049h");
tui_app.rs:328   akuma_write(fd::STDOUT, b"\x1b[<u\x1b[?1049l");
```

`\x1b[?1049h` is DECSET 1049 (alternate screen buffer + save cursor) — the
same mode `vim`/`less`/`htop` use. Terminal emulators do not extend their
normal scrollback buffer into the alternate screen; scrolling only reveals
whatever the app itself has drawn into its own (typically fixed-size,
managed-by-`set_scroll_region`) viewport, not an arbitrarily-long history.
This is standard, expected terminal behavior for any alt-screen app, not a
meow bug by itself — but it's the reason "just scroll the terminal normally"
can't work as-is while meow is in this mode.

**2. No mouse reporting is enabled, so the terminal falls back to
synthesizing arrow-key presses for scroll-wheel input:**

`grep`ping the whole TUI code for any mouse-tracking DECSET sequence
(`\x1b[?1000h`, `\x1b[?1006h`, or similar SGR/UTF-8 mouse modes) finds
nothing — meow never asks the terminal to report scroll-wheel events as
distinct escape sequences. This is a well-documented terminal-emulator
convention: **when an app is in the alternate screen buffer and has not
enabled mouse reporting, most terminal emulators translate scroll-wheel
events into synthetic Up/Down arrow-key presses**, specifically so
alt-screen apps that don't understand mouse events (like `less`) still
respond usefully to the wheel. meow's input handler has no way to
distinguish a real arrow-key press from the terminal's synthesized one, and
Up/Down are wired directly to input-history navigation
(`tui_app.rs:115-143`, `app/state.rs`'s `history_index`) — so scroll-wheel
input lands exactly where the report says it does.

### What a real fix needs

Both causes point at the same fix: **enable mouse reporting** (SGR mouse mode,
`\x1b[?1000h\x1b[?1006h` or similar) so scroll-wheel events arrive as their
own distinct escape sequences instead of being reinterpreted as arrow keys,
then handle those sequences by scrolling meow's own rendered conversation
buffer (which would also need to retain enough off-screen history to scroll
back into, since the alt-screen viewport alone doesn't). This is two pieces
of new work — input parsing for mouse sequences, and a scrollable
conversation-history buffer in the renderer — not a one-line fix.

An alternative that avoids new rendering work: **don't use the alternate
screen buffer at all** for the conversation pane, so the terminal's own
native scrollback (of the primary screen) just works. That would need
`set_scroll_region`'s current fixed-viewport model (`ui/tui/layout.rs:75-83`)
rethought, since it currently relies on the alt-screen's isolation to redraw
the input/status panes in place without disturbing conversation text already
above them — not evaluated here which approach is less work.

### Reproducing

Trivial: open `meow` in TUI mode, generate more output than fits on screen,
scroll up with the trackpad/wheel — observe the input line change to a
previous command instead of the conversation scrolling.

## Background

- `userspace/meow/src/tui_app.rs` — TUI entry/exit (`\x1b[?1049h`/`l`), input
  event dispatch, history navigation (Issue 2).
- `userspace/meow/src/ui/tui/layout.rs` — `set_scroll_region`/
  `reset_scroll_region` (DECSTBM), the fixed-viewport model Issue 2's
  alternative fix would need to change.
- `userspace/meow/src/ui/tui/input.rs`, `app/state.rs` — cursor/history index
  tracking (Issue 1's `.chars().count()` sites, Issue 2's `history_index`).
