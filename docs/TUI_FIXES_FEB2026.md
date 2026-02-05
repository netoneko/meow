# TUI Fixes - February 2026

This document summarizes the issues found and resolutions implemented during TUI development.

## Issue 1: Hotkeys Not Working (Ctrl+W, Ctrl+U, ESC, Ctrl+C)

### Symptom
Control key combinations like Ctrl+W (delete word), Ctrl+U (clear line), ESC (cancel), and Ctrl+C (interrupt) were not being recognized.

### Root Cause
Modern terminals (Kitty, Ghostty, WezTerm, iTerm2 with extended keyboard enabled) use the **Kitty keyboard protocol** (CSI u sequences) instead of sending raw control characters.

Instead of sending:
- Ctrl+W as `0x17`
- Ctrl+U as `0x15`
- ESC as `0x1B`

The terminal sends CSI u escape sequences:
- Ctrl+W: `ESC [ 119 ; 5 u` (keycode 119='w', modifier 5=Ctrl)
- Ctrl+U: `ESC [ 117 ; 5 u` (keycode 117='u', modifier 5=Ctrl)
- ESC: `ESC [ 27 u` (keycode 27)

### Resolution
Added CSI u sequence parsing in `parse_input()` in `tui_app.rs`:

```rust
b'u' => {
    // CSI u (Kitty keyboard protocol)
    // Format: ESC [ keycode ; modifier u
    // Modifier bits: 1=Shift, 2=Alt, 4=Ctrl (value is 1 + bits)
    
    // Parse keycode and modifier, map to InputEvents
    match keycode {
        97 => CtrlA,   // a
        99 => Interrupt, // c (Ctrl+C)
        101 => CtrlE,  // e
        106 => ShiftEnter, // j (Ctrl+J = LF)
        108 => CtrlL,  // l
        117 => CtrlU,  // u
        119 => CtrlW,  // w
        27 => Esc,     // ESC
        // ...
    }
}
```

### Diagnostic Tool
Added `/rawtest` command to show raw byte codes for 10 seconds, useful for debugging keyboard issues:
```
/rawtest
```

---

## Issue 2: Status Bar Text Shifting During Animation

### Symptom
The streaming status text would shift position as dots animated (1 to 5 dots).

### Resolution
Changed to fixed-width formatting - always 5 characters for dots area, padding with spaces:

```rust
// Animated dots (1-5), padded to 5 chars
for _ in 0..layout.status_dots {
    write!(stdout, ".");
}
for _ in layout.status_dots..5 {
    write!(stdout, " ");
}
```

Result:
```
[MEOW] jacking in.     (1 dot + 4 spaces)
[MEOW] waiting...      (3 dots + 2 spaces)  
[MEOW] streaming.....~(=^‥^)ノ [2.5s]  (time stays fixed)
```

---

## Issue 3: Extra Status Appearing When Shrinking Multiline Prompt

### Symptom
When going from multiline input back to single-line, the old status row would remain visible.

### Root Cause
Footer shrinking logic only cleared the old footer area, not the old status row above it.

### Resolution
Extended the clear range to include the old status row:

```rust
if effective_footer_height < old_footer_height {
    let old_separator_row = h - old_footer_height as u64;
    let old_status_row = old_separator_row.saturating_sub(1);
    // Clear from old status row through to the new separator
    for row in old_status_row..separator_row {
        set_cursor_position(0, row);
        write!(stdout, "{}", CLEAR_TO_EOL);
    }
}
```

---

## Issue 4: Duplicate Status Messages in Output Area

### Symptom
`[MEOW] [jacking in..] waiting......` appearing both in the output area and status bar.

### Resolution
Added `is_tui` flag to streaming functions in `main.rs`. Inline prints are now conditional:

```rust
let is_tui = tui_app::TUI_ACTIVE.load(Ordering::SeqCst);

// Only print inline for non-TUI mode
if !is_tui {
    libakuma::print("[jacking in");
}

// Always update status bar (TUI mode will show it there)
tui_app::update_streaming_status("[MEOW] jacking in", 0, None);
```

---

## Issue 5: Word Breaking at Wrong Points

### Symptom
LLM output was wrapping mid-word at character boundaries.

### Resolution
Implemented word buffering in `tui_print()`:

1. Buffer characters until space/newline
2. Before printing buffered word, check if it fits on current line
3. If not, wrap to next line first, then print word
4. Force-break words longer than line width

```rust
let flush_word = |word_buf, col, row, w| {
    if col + word_display_len > w - 1 && col > indent {
        // Word doesn't fit - wrap first
        wrap_line(row, col);
    }
    // Print the buffered word
    for c in word_buf { print(c); }
};
```

---

## Issue 6: Dots Animation Too Fast

### Symptom
Status bar dots were cycling too quickly.

### Resolution
Added `repaint_counter` to `PaneLayout`, dots only update every 10th repaint (~500ms at 50ms poll rate):

```rust
layout.repaint_counter = layout.repaint_counter.wrapping_add(1) % 10000;

if layout.repaint_counter % 10 == 0 {
    layout.status_dots = (layout.status_dots % 5) + 1;
}
```

---

## Issue 7: No Idle Status Indicator

### Symptom
Status bar was empty when not streaming.

### Resolution
Added "awaiting user input" status when idle:

```rust
if !is_streaming && layout.status_text.is_empty() {
    layout.status_text = String::from("[MEOW] awaiting user input");
}
```

---

## Files Modified

- `userspace/meow/src/tui_app.rs` - Input parsing, rendering, PaneLayout
- `userspace/meow/src/main.rs` - Streaming functions, /rawtest command
- `userspace/meow/docs/THREE_PANE_PLAN.md` - Implementation status added

## Testing

Use `/rawtest` or `/keytest` to verify keyboard input is being received correctly:
```
/rawtest
(press keys for 10 seconds, see hex byte codes)
```

For terminals using Kitty protocol, you should see sequences like:
- ESC: `1B 5B 32 37 75` = `ESC [ 27 u`
- Ctrl+W: `1B 5B 31 31 39 3B 35 75` = `ESC [ 119 ; 5 u`
