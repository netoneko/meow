# UI Improvements for `meow`

This document summarizes the user interface improvements and optimizations implemented for the `meow` chat client, focusing on adapting it to the Akuma OS's `no_std` environment and addressing performance concerns.

## 1. Initial State & Challenges

The initial UI plan for `meow` envisioned using `ratatui` and `crossterm`. However, due to Akuma OS's `no_std` nature, these libraries (which heavily rely on the Rust standard library `std`) were incompatible without significant porting efforts or an extensive `std` compatibility layer.

The first implementation attempt using `ratatui` and `crossterm` failed compilation, highlighting the need for a `no_std` native solution.

## 2. Phase 1: Basic `no_std` TUI with `libakuma` Syscalls

To address the `no_std` constraint, the `tui_app.rs` module was refactored to:
*   Remove all dependencies on `std`, `ratatui`, and `crossterm`.
*   Directly utilize `libakuma`'s terminal syscalls (`set_terminal_attributes`, `set_cursor_position`, `hide_cursor`, `show_cursor`, `clear_screen`, `poll_input_event`, `write`) for all terminal interactions.
*   Implement a minimal text-based UI, providing a chat history area and an input line.

**Initial Performance Issue:** This initial implementation suffered from severe flickering and input lag because the `render()` function indiscriminately called `clear_screen()` and redrew the entire UI in every loop iteration, even when only minor changes occurred. This resulted in an excessive number of kernel syscalls.

## 3. Phase 2: Performance Optimizations (Flicker & Lag Reduction)

To mitigate flickering and input lag, the following optimizations were implemented:

### 3.1. Granular Dirty Flags & Partial Rendering
*   The monolithic `dirty: AtomicBool` flag was replaced with two granular flags: `input_dirty: AtomicBool` and `history_dirty: AtomicBool`. These flags indicate whether only the input line or the chat history (or both) need redrawing.
*   The `render()` method was split into two specialized functions:
    *   `render_history()`: Responsible for clearing and redrawing only the chat history area.
    *   `render_input()`: Responsible for clearing and redrawing only the input line and positioning the cursor.
*   The main `run_tui()` loop now conditionally calls `render_history()` or `render_input()` only when their respective `dirty` flags are set. This ensures that only the minimum necessary parts of the screen are updated, significantly reducing redraws.
*   `AtomicBool` was used for dirty flags to ensure thread-safety, anticipating potential future multi-threading in `meow` (e.g., for background LLM processing).

### 3.2. Optimized Cursor Management
*   The `hide_cursor()` and `show_cursor()` calls were centralized in the `run_tui()` loop. They are now executed only once per render cycle, and only if a redraw is actually needed (`needs_render` is true). This prevents unnecessary cursor blinking/flickering.

### 3.3. Adaptive Input Polling
*   The `poll_input_event()` timeout was made adaptive:
    *   When no UI elements are "dirty" (`needs_render` is false), `poll_input_event()` blocks indefinitely (`u64::MAX`). This yields CPU cycles efficiently until new input arrives, preventing busy-waiting.
    *   When UI elements are "dirty" (`needs_render` is true), `poll_input_event()` uses a short timeout (e.g., `1ms`). This allows the loop to quickly redraw and then re-check for input, ensuring responsiveness.

## 4. Kernel-Side Performance Enhancements

To further reduce performance overhead from kernel activity, two new configuration options were introduced in `src/config.rs` and implemented in `src/syscall.rs`:

*   **`STDOUT_TO_KERNEL_LOG_COPY_ENABLED: bool = false;`**: This option controls whether standard output from userspace applications is mirrored to the kernel's debug log. Disabling this (`false` by default) prevents excessive logging to the kernel console, which can be a significant performance bottleneck, especially when the terminal driver involves locks.
*   **`SYSCALL_DEBUG_INFO_ENABLED: bool = false;`**: This option suppresses the `[syscall]` debug prints that were previously generated for every terminal-related syscall (e.g., `sys_set_cursor_position`, `sys_hide_cursor`). By default, these verbose logs are now disabled, reducing kernel processing overhead and cleaning up debug output.

These combined efforts have transitioned `meow` from a basic, inefficient terminal application to a more responsive and performant `no_std` TUI client within the Akuma OS environment.
