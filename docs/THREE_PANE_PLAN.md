# Three-Pane TUI Implementation Plan

Based on ANOTHER_MOCKUP.md - Output Pane (top), Status Pane (middle), Footer Pane (bottom).

## Current State (Single Stream Fix)

The single stream fix eliminates cursor sync issues by using one output stream:
- `[MEOW]` is printed once
- `[jacking in...]`, dots, timing all flow in sequence
- LLM output follows naturally
- No dynamic status updates

This works but provides less visual feedback during connection.

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ OUTPUT PANE - Scroll region, rows 1 to output_bottom                        │
├─────────────────────────────────────────────────────────────────────────────┤
│ STATUS PANE - Fixed, 1-2 rows, outside scroll region                        │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│ FOOTER PANE - Fixed, dynamic height, outside scroll region                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Implementation Phases

### Phase 1: Pane State Structure

Create a struct to manage pane state instead of scattered atomics.

```rust
// src/tui_app.rs

pub struct PaneLayout {
    pub term_width: u16,
    pub term_height: u16,
    
    // Output pane
    pub output_top: u16,        // Always 1
    pub output_bottom: u16,     // Calculated
    pub output_row: u16,        // Current cursor row
    pub output_col: u16,        // Current cursor col
    
    // Status pane
    pub status_row: u16,        // Single row for status
    pub status_text: String,    // Current status (e.g., "[MEOW] [jacking in..]")
    pub status_dots: u8,        // Number of dots
    pub status_time_ms: Option<u64>,
    
    // Footer pane  
    pub footer_top: u16,        // Separator row
    pub footer_height: u16,     // Total footer height (2 + prompt lines)
    pub prompt_scroll: u16,     // Scroll offset within prompt
    pub cursor_idx: u16,        // Cursor position in input
}

impl PaneLayout {
    pub fn new(width: u16, height: u16) -> Self { ... }
    pub fn recalculate(&mut self, new_footer_height: u16) { ... }
    pub fn output_max_row(&self) -> u16 { self.output_bottom }
}
```

**Files to modify:**
- `src/tui_app.rs` - Add PaneLayout struct
- Replace atomic variables with PaneLayout instance

**Estimated changes:** ~150 lines

---

### Phase 2: Scroll Region Management

Ensure scroll region only covers OUTPUT PANE.

```rust
impl PaneLayout {
    pub fn set_scroll_region(&self) {
        // Only output pane scrolls
        print!("\x1b[{};{}r", self.output_top, self.output_bottom);
    }
    
    pub fn reset_scroll_region(&self) {
        print!("\x1b[1;{}r", self.term_height);
    }
}
```

**Key changes:**
- Call `set_scroll_region()` on TUI init
- Call it again after footer resize
- Status and footer rendering explicitly position cursor outside scroll region

**Files to modify:**
- `src/tui_app.rs` - scroll region functions

**Estimated changes:** ~30 lines

---

### Phase 3: Status Pane Rendering

Status pane is always redrawn from scratch (no cursor tracking needed).

```rust
impl PaneLayout {
    pub fn render_status(&self) {
        // Save cursor, move to status row, clear, draw, restore
        print!("\x1b[s");  // Save cursor
        print!("\x1b[{};1H", self.status_row);  // Move to status row
        print!("\x1b[2K");  // Clear line
        
        // Draw status content
        print!("  {}", self.status_text);
        if self.status_dots > 0 {
            for _ in 0..self.status_dots {
                print!(".");
            }
        }
        if let Some(ms) = self.status_time_ms {
            print!(" ~(=^‥^)ノ [{}ms]", ms);
        }
        
        print!("\x1b[u");  // Restore cursor
    }
    
    pub fn update_status(&mut self, text: &str, dots: u8, time: Option<u64>) {
        self.status_text = String::from(text);
        self.status_dots = dots;
        self.status_time_ms = time;
        self.render_status();
    }
}
```

**Files to modify:**
- `src/tui_app.rs` - status rendering
- `src/main.rs` - call `update_status()` instead of `print()`

**Estimated changes:** ~80 lines

---

### Phase 4: Output Pane with Proper Boundaries

Output writing respects pane boundaries.

```rust
pub fn write_output(layout: &mut PaneLayout, s: &str) {
    // Save position (might be in prompt)
    print!("\x1b[s");
    
    // Move to output cursor
    print!("\x1b[{};{}H", layout.output_row, layout.output_col);
    
    for c in s.chars() {
        if c == '\n' {
            layout.output_row += 1;
            layout.output_col = 9;  // Indented continuation
            
            if layout.output_row > layout.output_bottom {
                // At scroll boundary - print newline to scroll
                layout.output_row = layout.output_bottom;
                print!("\n");
            }
            print!("\x1b[{};{}H", layout.output_row, layout.output_col);
        } else {
            print!("{}", c);
            layout.output_col += 1;
            
            if layout.output_col >= layout.term_width {
                // Wrap to next line
                layout.output_row += 1;
                layout.output_col = 9;
                if layout.output_row > layout.output_bottom {
                    layout.output_row = layout.output_bottom;
                    print!("\n");
                }
                print!("\x1b[{};{}H", layout.output_row, layout.output_col);
            }
        }
    }
    
    // Restore cursor to prompt
    print!("\x1b[u");
}
```

**Files to modify:**
- `src/tui_app.rs` - replace tui_print with write_output
- `src/main.rs` - use new output function

**Estimated changes:** ~100 lines

---

### Phase 5: Footer Resize with Pane Coordination

When footer grows/shrinks, update all pane boundaries.

```rust
impl PaneLayout {
    pub fn resize_footer(&mut self, new_height: u16) {
        let old_height = self.footer_height;
        if new_height == old_height { return; }
        
        // Calculate new boundaries
        let status_height = 1;
        let gap = 1;
        let new_output_bottom = self.term_height - new_height - gap - status_height;
        let new_status_row = new_output_bottom + 1;
        let new_footer_top = new_status_row + status_height + gap;
        
        if new_height > old_height {
            // GROWING: scroll output up if needed
            let growth = new_height - old_height;
            if self.output_row > new_output_bottom {
                // Print newlines to scroll content up
                print!("\x1b[{};1H", self.output_bottom);
                for _ in 0..growth {
                    print!("\n");
                }
                self.output_row = self.output_row.saturating_sub(growth);
            }
        }
        
        // Update boundaries
        self.output_bottom = new_output_bottom;
        self.status_row = new_status_row;
        self.footer_top = new_footer_top;
        self.footer_height = new_height;
        
        // Update scroll region
        self.set_scroll_region();
        
        // Redraw status and footer at new positions
        self.render_status();
        self.render_footer();
    }
}
```

**Files to modify:**
- `src/tui_app.rs` - resize logic in PaneLayout

**Estimated changes:** ~60 lines

---

### Phase 6: Input Handling Integration

Connect input events to footer resize.

```rust
fn handle_input_event(layout: &mut PaneLayout, event: InputEvent) {
    match event {
        InputEvent::ShiftEnter => {
            // Add newline to input
            insert_char('\n');
            // Recalculate footer height
            let new_height = calculate_footer_height(&input);
            layout.resize_footer(new_height);
        }
        InputEvent::Backspace => {
            // Remove char, might shrink footer
            remove_char();
            let new_height = calculate_footer_height(&input);
            if new_height < layout.footer_height && !STREAMING.load() {
                layout.resize_footer(new_height);
            }
        }
        // ... other events
    }
}
```

**Files to modify:**
- `src/tui_app.rs` - handle_input_event integration

**Estimated changes:** ~40 lines

---

### Phase 7: Streaming Coordination

During streaming, coordinate output with status updates.

```rust
// In main.rs send_with_retry()

// Before connecting
update_status("[MEOW] [jacking in", 0, None);

// During connection attempts
update_status("[MEOW] [jacking in", dots, None);
dots += 1;

// When first token received
update_status("[MEOW] streaming", 0, Some(elapsed_ms));

// Continuation
update_status("[MEOW] [continuing", dots, None);
```

**Files to modify:**
- `src/main.rs` - streaming functions

**Estimated changes:** ~50 lines

---

## Migration Strategy

1. **Phase 1-2 first** - Get PaneLayout working, keep existing rendering
2. **Phase 3 next** - Status pane independent, verify no artifacts
3. **Phase 4** - Output pane with proper boundaries
4. **Phase 5** - Footer resize with coordination
5. **Phase 6-7** - Polish input and streaming

Each phase should be testable independently.

## Testing Checklist

- [ ] Basic output scrolling works
- [ ] Status updates don't affect output
- [ ] Footer can grow from 1 to 10 lines without artifacts
- [ ] Footer can shrink when not streaming
- [ ] Multiline input + streaming together works
- [ ] Ctrl+C/ESC cancellation works
- [ ] Terminal resize (Ctrl+L) works
- [ ] Long LLM responses scroll correctly
- [ ] Tool output displays correctly

## Estimated Total Changes

- ~500 lines of new/modified code
- Can be done incrementally
- Each phase adds functionality without breaking existing behavior

---

## Implementation Status (Feb 2026)

### Completed Features

**PaneLayout struct** (`tui_app.rs`):
- Manages all three panes: output, status, footer
- Fields: `term_width`, `term_height`, `output_top`, `output_bottom`, `output_row`, `output_col`, `status_row`, `status_text`, `status_dots`, `status_time_ms`, `footer_top`, `footer_height`, `prompt_scroll`, `cursor_idx`, `input_prefix_len`, `repaint_counter`

**Status pane** (1 row above footer separator):
- Shows connection/streaming status with animated dots
- Dots cycle 1→5 every 10th repaint (~500ms at 50ms poll rate)
- Status states:
  - `[MEOW] awaiting user input.....` - idle, animated dots
  - `[MEOW] jacking in.....` - connecting, animated dots
  - `[MEOW] waiting.....` - request sent, animated dots  
  - `[MEOW] streaming ~(=^‥^)ノ [1.2s]` - response time, no dots

**Footer resizing**:
- Dynamic height for multiline input (up to 10 lines)
- During streaming: footer can grow but not shrink (prevents artifacts)
- Buffer shifts up correctly when entering multiline mode
- Clears transitional areas when resizing

**Scroll region management**:
- Output pane uses ANSI scroll region `\x1b[<top>;<bottom>r`
- Explicit cursor positioning via `set_cursor_position(col, row)`
- No reliance on `\x1b[s` / `\x1b[u` cursor save/restore (unreliable)

**TUI/non-TUI mode**:
- `is_tui` flag controls inline printing vs status bar updates
- Non-TUI mode prints connection status inline as before
- TUI mode shows status only in status bar (no duplicate output)

### Key Implementation Details

**Repaint frequency**: Main loop uses `poll_input_event(50, ...)` = ~50ms per frame (20 FPS)

**Dots animation**: Managed centrally in `render_footer_internal()`, not in streaming functions

**Status update logic** (`update_status`):
- Only resets dots/counter when status text changes
- Prevents flicker from repeated calls with same text

**Files modified**:
- `userspace/meow/src/tui_app.rs` - PaneLayout, rendering, input handling
- `userspace/meow/src/main.rs` - streaming functions use `is_tui` flag
