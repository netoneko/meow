# Summary of Meow UI Development Session

This document provides a chronological summary of the efforts to implement and optimize the Terminal User Interface (TUI) for the `meow` LLM client in the Akuma `no_std` environment.

## 1. The `no_std` Pivot
The initial plan to use standard TUI crates like `ratatui` and `crossterm` was abandoned. These libraries have deep dependencies on `std`, making them incompatible with Akuma's userspace. 
*   **Action:** Developed a custom TUI engine from scratch using `libakuma` system calls.

## 2. Eliminating Jitter and Flickering
The early custom implementation redrew the entire screen on every loop iteration, causing extreme flickering and performance lag.
*   **Optimization 1:** Introduced a "dirty" flag to only render when the state changed.
*   **Optimization 2:** Made the dirty flag atomic (`AtomicBool`) to prepare for future multi-threading and ensure safe signaling.
*   **Optimization 3:** Split rendering into granular functions (`render_history` and `render_input`) with separate dirty flags, ensuring typing only redrew the input line.

## 3. Kernel Performance Tuning
Observed that userspace performance was being throttled by synchronous kernel logging.
*   **Action:** Introduced `STDOUT_TO_KERNEL_LOG_COPY_ENABLED` and `SYSCALL_DEBUG_INFO_ENABLED` in `src/config.rs`.
*   **Result:** Disabled mirroring userspace `stdout` and verbose `[syscall]` prints to the kernel log, significantly reducing I/O contention.

## 4. Cyberpunk Aesthetic (Tokyo Night)
Transformed the UI from a basic text box into a high-tech terminal.
*   **Palette:** Implemented the Tokyo Night color scheme:
    *   **User:** Violet (`\x1b[38;5;177m`)
    *   **Meow:** Blue (`\x1b[38;5;111m`)
    *   **Frames:** Shades of Gray (`\x1b[38;5;240m` / `242m`)
*   **Greeting:** Integrated a cyberpunk cat ASCII greeting (`src/akuma_40.txt`) on a dark grey pane at startup.

## 5. Responsive "Natural Scrollback" Architecture
To handle terminal resizing and avoid "mangled" output during AI streaming, the layout was fundamentally changed.
*   **DECSTBM Scroll Region:** Implemented a terminal-native scroll region (Lines 1 to Height-4). History now flows through the terminal's native buffer, while the footer remains stationary.
*   **Concurrency:** Interleaved AI output with user input. Using `\x1b[s` (Save Cursor) and `\x1b[u` (Restore Cursor), Meow can "inject" text into the scroll area while the user continues to type in the prompt area.
*   **UTF-8 Robustness:** Fixed a critical panic where word-wrapping logic was slicing strings at invalid byte offsets. Switched to `char_indices` for boundary-safe wrapping.

## 6. Current State
`meow` now defaults to a high-performance, responsive TUI. 
*   **Command Support:** `/help`, `/model`, and `/clear` are fully integrated.
*   **Dynamic Sizing:** Uses ANSI probe codes (`\x1b[6n`) to detect terminal dimensions automatically.
*   **Legacy Mode:** The old interactive interface is preserved behind the `--classic` flag.
