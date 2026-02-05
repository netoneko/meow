# Three-Pane TUI Architecture

## Layout

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ OUTPUT PANE (scrolling region)                                              │
│                                                                             │
│  > read README.md and summarize                                             │
│                                                                             │
│          Nya~! Let me read that file for you! (=^・ω・^=)                    │
│                                                                             │
│          [*] Tool executed successfully nya~!                               │
│          Contents of 'README.md':                                           │
│          ```                                                                │
│          # Akuma Kernel                                                     │
│          A bare-metal ARM64 kernel written in Rust...                       │
│          ```                                                                │
│                                                                             │
│          Okay, here's the summary nya~! This is a bare-metal kernel that    │
│          boots on QEMU's ARM virt machine. It has SSH, threading, async     │
│          networking, ext2 filesystem, and userspace support! Pretty cool    │
│          for a hobby OS! ฅ^•ﻌ•^ฅ                                            │
│                                                                             │
│          [*] Intent phrases: 0, tools called: 1                             │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│ STATUS PANE (1-2 lines, fixed position above footer)                        │
│  [MEOW] [jacking in..] waiting ~(=^‥^)ノ [5.3s]                             │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│ FOOTER PANE (separator + provider info + prompt)                            │
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > _                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Pane Definitions

### OUTPUT PANE (Top)
- **Location**: Row 1 to `h - status_height - footer_height - gap`
- **Behavior**: Terminal scroll region, content scrolls up when full
- **Contains**: User input echo, LLM responses, tool outputs, system messages
- **Cursor tracking**: `OUTPUT_ROW`, `OUTPUT_COL`

### STATUS PANE (Middle, fixed)
- **Location**: Row `h - footer_height - gap - status_height` to `h - footer_height - gap`
- **Behavior**: Fixed position, never scrolls, cleared and redrawn each update
- **Contains**: Connection status, progress dots, timing info
- **Height**: 1-2 lines (1 for status, optional 1 for continuation info)
- **Cursor tracking**: Not needed - always redrawn from scratch

### FOOTER PANE (Bottom)
- **Location**: Row `h - footer_height` to `h`
- **Behavior**: Fixed position, can grow/shrink for multiline input
- **Contains**: Separator line, provider/model info, input prompt
- **Cursor tracking**: Existing `CURSOR_IDX`, `PROMPT_SCROLL_TOP`

## Row Calculations

```
h = terminal height (e.g., 40)
footer_height = 2 + prompt_lines (e.g., 4 for 2-line prompt)
status_height = 1 (or 2 if showing continuation)
gap = 1 (visual separator)

output_top = 1
output_bottom = h - footer_height - gap - status_height  (e.g., 40 - 4 - 1 - 1 = 34)
status_row = output_bottom + 1  (e.g., 35)
footer_top = status_row + status_height + gap  (e.g., 37)
```

## State Structure

```rust
struct TuiPanes {
    // Output pane
    output_row: u16,      // Current row in output area
    output_col: u16,      // Current column in output area
    
    // Status pane (no cursor needed - always redrawn)
    status_text: String,  // e.g., "[MEOW] [jacking in..] waiting"
    status_dots: u8,      // Number of dots to show
    status_time: Option<u64>, // Elapsed time in ms
    
    // Footer pane (existing)
    footer_height: u16,
    cursor_idx: u16,
    prompt_scroll_top: u16,
}
```

## Rendering Flow

### On each status update (dots, timing):
```rust
fn update_status(text: &str, dots: u8, time_ms: Option<u64>) {
    save_cursor();
    move_to(status_row, 0);
    clear_line();
    print!("  {} {}", text, ".".repeat(dots));
    if let Some(ms) = time_ms {
        print!(" ~(=^‥^)ノ [{}ms]", ms);
    }
    restore_cursor();
}
```

### On LLM output chunk:
```rust
fn print_output(s: &str) {
    // Uses OUTPUT_ROW/OUTPUT_COL
    // Scroll region is set to output pane only
    // Status and footer are outside scroll region, unaffected
    set_cursor(output_col, output_row);
    for char in s.chars() {
        // Handle wrapping, newlines, etc.
        // If output_row reaches output_bottom, content scrolls up
    }
    save_output_cursor();
    return_to_prompt();
}
```

### On footer resize:
```rust
fn resize_footer(new_height: u16) {
    let old_height = footer_height;
    if new_height > old_height {
        // Footer growing: shrink output area
        let shrink_by = new_height - old_height;
        // Adjust status_row
        // Adjust scroll region
        // If output_row > new output_bottom, clamp it
    }
    // Redraw status pane (it moved)
    // Redraw footer
}
```

## Advantages of This Layout

1. **Status never interferes with output**: Status pane is below output scroll region
2. **Output scrolls naturally**: LLM content flows top-to-bottom in its own region
3. **Status updates are cheap**: Just clear and redraw 1-2 lines
4. **Footer resize only affects boundaries**: Output and status adjust positions, but content is preserved
5. **Clear visual hierarchy**: Output (main content) → Status (progress) → Footer (input)

## Visual States

### Idle (waiting for input):
```
│ <previous output...>                                                        │
│                                                                             │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > _                                              │
```

### Connecting:
```
│ <previous output...>                                                        │
│                                                                             │
│  > explain async/await                                                      │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [MEOW] [jacking in..] waiting....                                          │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > _                                              │
```

### Streaming:
```
│  > explain async/await                                                      │
│                                                                             │
│          Nya~! Async/await is a way to write asynchronous code that looks   │
│          like synchronous code! Instead of callbacks everywhere, you can    │
│          just `await` a future and the compiler handles the state machine   │
│          nya~! ▌                                                            │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [MEOW] streaming... ~(=^‥^)ノ [2.1s]                                       │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > _                                              │
```

### Continuation (tool call):
```
│          [*] Tool executed successfully nya~!                               │
│          <tool output...>                                                   │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [MEOW] [continuing..] waiting...                                           │
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > _                                              │
```

## Dynamic Footer Handling

The footer can grow from 2 lines (single-line prompt) to 10+ lines (multiline input). This requires careful coordination.

### Footer Growth Sequence

When user presses Shift+Enter to add a newline to input:

```
BEFORE (footer_height = 3):
┌─────────────────────────────────────────────────────────────────────────────┐
│ OUTPUT PANE (rows 1-33)                                                     │
│          ...LLM output streaming here...                                    │
│          The quick brown fox jumps over the lazy dog nya~                   │  ← row 33
├─────────────────────────────────────────────────────────────────────────────┤
│ STATUS PANE (row 34)                                                        │
│  [MEOW] streaming... ~(=^‥^)ノ [2.1s]                                       │  ← row 34
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│ FOOTER PANE (rows 36-38)                                                    │
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │  ← row 37
│  [1.2k/128k|45K] (=^･ω･^=) > some input_                                    │  ← row 38
└─────────────────────────────────────────────────────────────────────────────┘

AFTER (footer_height = 4, user added newline):
┌─────────────────────────────────────────────────────────────────────────────┐
│ OUTPUT PANE (rows 1-32) ← shrunk by 1                                       │
│          ...LLM output streaming here...                                    │
│          The quick brown fox jumps over the lazy dog nya~                   │  ← row 32
├─────────────────────────────────────────────────────────────────────────────┤
│ STATUS PANE (row 33) ← moved up by 1                                        │
│  [MEOW] streaming... ~(=^‥^)ノ [2.1s]                                       │  ← row 33
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│ FOOTER PANE (rows 35-38) ← grew by 1                                        │
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │  ← row 36
│  [1.2k/128k|45K] (=^･ω･^=) > some input                                     │  ← row 37
│  more input on second line_                                                 │  ← row 38
└─────────────────────────────────────────────────────────────────────────────┘
```

### Resize Algorithm

```rust
fn resize_footer(new_footer_height: u16) {
    let h = TERM_HEIGHT.load();
    let old_footer_height = FOOTER_HEIGHT.load();
    let status_height = 1;
    let gap = 1;
    
    if new_footer_height == old_footer_height {
        return; // No change
    }
    
    // Calculate new boundaries
    let new_output_bottom = h - new_footer_height - gap - status_height;
    let new_status_row = new_output_bottom + 1;
    let new_footer_top = new_status_row + status_height + gap;
    
    if new_footer_height > old_footer_height {
        // FOOTER GROWING
        let growth = new_footer_height - old_footer_height;
        
        // 1. First, scroll output content up if cursor is near bottom
        let output_row = OUTPUT_ROW.load();
        if output_row > new_output_bottom {
            // Content at bottom needs to scroll up
            // Position at old output bottom and print newlines
            set_cursor(0, old_output_bottom);
            for _ in 0..growth {
                print!("\n");
            }
            // Adjust output cursor
            OUTPUT_ROW.store(output_row.saturating_sub(growth));
        }
        
        // 2. Update scroll region to new smaller size
        set_scroll_region(1, new_output_bottom);
        
        // 3. Clear old status row (it's moving up)
        clear_row(old_status_row);
        
        // 4. Redraw status at new position
        redraw_status(new_status_row);
        
        // 5. Redraw footer (it now has more lines)
        redraw_footer(new_footer_top, new_footer_height);
        
    } else {
        // FOOTER SHRINKING
        let shrink = old_footer_height - new_footer_height;
        
        // 1. Update scroll region to new larger size
        set_scroll_region(1, new_output_bottom);
        
        // 2. Clear old footer lines that are now in the gap/status area
        for row in old_footer_top..new_footer_top {
            clear_row(row);
        }
        
        // 3. Redraw status at new position
        redraw_status(new_status_row);
        
        // 4. Redraw footer (it now has fewer lines)
        redraw_footer(new_footer_top, new_footer_height);
    }
    
    FOOTER_HEIGHT.store(new_footer_height);
}
```

### Key Invariants

1. **Scroll region only covers OUTPUT PANE**: `\x1b[1;{output_bottom}r`
2. **Status and footer are OUTSIDE scroll region**: They don't scroll with output
3. **Footer growth steals from output area**: Output bottom moves up
4. **Footer shrink gives back to output area**: Output bottom moves down
5. **Status always sits between output and footer**: Moves with footer growth/shrink
6. **Output cursor is clamped**: If output_row > output_bottom after resize, clamp it

### Multiline Footer States

```
1 line prompt (footer_height = 3):
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > short input_                                   │

3 line prompt (footer_height = 5):
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > this is a longer input                         │
│  that spans multiple lines because I pressed                                │
│  Shift+Enter to add newlines_                                               │

Max prompt (footer_height = 12, capped at h/3):
├━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
│  [Provider: gemini] [Model: gemini-2.0-flash]                               │
│  [1.2k/128k|45K] (=^･ω･^=) > line 1                                         │
│  line 2                                                                     │
│  line 3                                                                     │
│  ... (scrollable within prompt area)                                        │
│  line 10_                                                                   │
```

### Streaming + Multiline Footer

This is the tricky case. During streaming:
1. LLM output is being written to OUTPUT PANE
2. User might type in the prompt (queued for later)
3. User might add newlines to their input (footer grows)

Solution:
- **Pause output cursor updates during footer resize**
- **Save/restore output position around status/footer redraws**
- **Status pane is redrawn independently of output streaming**

```rust
fn on_streaming_chunk(content: &str) {
    // Write to output pane at OUTPUT_ROW/OUTPUT_COL
    save_cursor();  // Save current position (might be in prompt)
    
    set_cursor(OUTPUT_COL, OUTPUT_ROW);
    write_output(content);  // Handles wrapping, scroll if needed
    save_output_position();
    
    restore_cursor();  // Return to prompt
}

fn on_status_update(text: &str) {
    // Redraw status pane without affecting output
    save_cursor();
    
    let status_row = calculate_status_row();
    set_cursor(0, status_row);
    clear_line();
    print!("  {}", text);
    
    restore_cursor();
}

fn on_footer_input(event: InputEvent) {
    // Handle input, potentially resize footer
    let old_height = FOOTER_HEIGHT.load();
    process_input(event);  // May add newline
    let new_height = calculate_footer_height();
    
    if new_height != old_height {
        resize_footer(new_height);
    }
}
```

## Implementation Notes

1. **Scroll region**: Set to output pane only (`\x1b[1;{output_bottom}r`)
2. **Status pane**: Outside scroll region, manually positioned
3. **Footer pane**: Outside scroll region, manually positioned
4. **Gap line**: Can be a visual separator (─) or just empty space between status and footer
5. **Footer resize during streaming**: Must coordinate with output cursor, use save/restore
6. **Atomic updates**: Footer resize should be a single transaction - clear old, draw new
