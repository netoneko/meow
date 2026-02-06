# Meow-chan Memory Leak Refactor

## Problem Analysis
The `meow` application encountered an `OUT OF MEMORY` error in the Akuma kernel. Despite the "net memory" being relatively low (~244 KB), the total allocated memory throughout the process lifetime reached over 800 MB. This indicated extreme **memory churn**—repeatedly allocating and deallocating strings and objects in tight loops, eventually exhausting the custom allocator's capacity or causing heap fragmentation.

## Key Work & Optimizations

### 1. Zero-Clone State Accessors (`src/app/state.rs`)
Previously, accessors like `get_global_input()` and `get_model_and_provider()` returned cloned `String` objects. In the TUI render loop (running at ~20-50Hz), this caused thousands of unnecessary allocations per second.
- **Solution:** Introduced `with_global_input` and `with_model_and_provider` closure-based helpers that provide immutable references (`&str`) to the internal state, eliminating clones.

### 2. Buffer-Based JSON Construction (`src/app/history.rs`)
The `Message::to_json()` method previously created a new `String` for every message, and `chat_once` concatenated them into another new `String`.
- **Solution:** Added `Message::write_json(&self, out: &mut String)`. This allows the chat logic to pre-allocate a single buffer based on estimated token counts and append JSON data directly, significantly reducing intermediate allocations.

### 3. TUI Render Loop Optimization (`src/ui/tui/render.rs`)
The rendering logic used `String::repeat()` and `format!()` for UI elements like separators and status lines.
- **Solution:** Replaced high-frequency `format!()` calls with direct `akuma_write` calls. Replaced `.repeat(w)` with manual loops that write single characters/bytes to avoid creating temporary row-length strings.

### 4. Redundant Logic Removal (`src/tui_app.rs`)
The TUI loop was recalculating history tokens and cloning the global input even when no input events occurred.
- **Solution:** 
    - Moved token calculation to be reactive to state changes.
    - Added checks to `tui_handle_input` to skip rendering if no events were processed and no streaming is active.
    - Cached the "last history KB" value in the global state to avoid repeated calculation.

## Results
These changes moved the application from a "high churn" model to a "static/buffer" model. Memory usage should now remain stable during idle TUI usage and scale linearly with conversation length rather than exponentially with UI refresh rates.
