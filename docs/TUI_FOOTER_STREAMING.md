# TUI Footer and Streaming Behavior

This document summarizes the work done on the meow TUI footer rendering and streaming behavior, along with known remaining issues.

## Architecture Overview

The TUI has three main areas:
1. **Scroll Region** (rows 1 to `h - footer_height - gap`): LLM output area
2. **Gap** (3-5 lines depending on terminal height): Buffer zone between output and footer
3. **Footer** (separator + status + prompt lines): User input area

Key state variables:
- `STREAMING`: True while LLM is outputting
- `CANCELLED`: Set by ESC/Ctrl+C to signal cancellation request
- `FOOTER_HEIGHT`: Current footer height in lines
- `CUR_ROW`/`CUR_COL`: Current LLM output cursor position

## Footer Resize Behavior

### When NOT Streaming
- Footer can grow or shrink freely
- Scroll region is adjusted accordingly
- Old footer lines are cleared when shrinking

### When Streaming
- Footer can **GROW** (to show multiline input)
  - Newlines are printed to scroll LLM content up
  - Scroll region is shrunk
  - `CUR_ROW` is adjusted to stay in valid range
- Footer **cannot SHRINK** (deferred until streaming ends)
  - Prevents double separator line artifacts
  - Uses `effective_footer_height = max(old, new)`

## Gap Clearing Logic

Only clears when footer shrinks:
```rust
if effective_footer_height < old_footer_height {
    let old_separator_row = h - old_footer_height;
    for row in old_separator_row..separator_row {
        // Clear old footer lines now in gap area
    }
}
```

This ensures we never clear LLM output - only the exact rows that transitioned from footer to gap.

## Known Remaining Issues

### Keyboard Shortcuts Not Working
- **Ctrl+C**: Does not stop LLM request
- **Ctrl+W**: Does not delete word backward  
- **Ctrl+U**: Does not clear input line
- **ESC**: Does not cancel LLM request

These are parsed in `parse_input()` and handled in `handle_input_event()`, but the cancellation signal may not be reaching the streaming code, or the terminal may not be in proper raw mode.

### Display Issues During Streaming + Multiline Edit
- When switching to multiline edit during streaming, `[MEOW]` line appears one line below where it should be
- Continuation dots appear one line above `[MEOW]`
- Suggests cursor position calculation issue when footer grows during streaming

The root cause appears to be a race condition or off-by-one error in the `CUR_ROW` adjustment when printing newlines to scroll content up during footer growth.

## Code Locations

- `render_footer_internal()`: Main footer rendering logic (~line 674)
- `tui_handle_input()`: Input handling during streaming (~line 622)
- `tui_print()`: LLM output with cursor management (~line 130)
- `output_footer_gap()`: Dynamic gap calculation (~line 21)
- `STREAMING` flag: Set around `chat_once()` call (~line 926)

## Future Work

1. Debug why Ctrl+C/ESC don't trigger `CANCELLED` flag properly
2. Fix cursor positioning when footer grows during streaming
3. Consider whether the gap is actually needed or if proper scroll region management would suffice
4. Investigate if terminal raw mode is correctly configured for control character handling
