# Meow-chan UI Improvements: Extra Phase

This plan outlines the next set of refinements for the `meow` TUI, focusing on responsiveness, terminal integration, and aesthetic flair.

## 1. Natural Scrollback & Minimalist Layout
*   **Remove Frame Persistence:** Eliminate the top bar and side grid lines. Instead, allow the terminal to use its natural scrollback buffer.
*   **Bottom Persistent Footer:** The only fixed UI element will be the bottom bar (status) and the expanding input box.
*   **Expansion Logic:** The input box will grow upwards from the bottom based on the amount of text being typed.

## 2. Concurrent Input & Output
*   **Non-Blocking Chat:** Refactor the TUI loop to allow user input even while the AI is responding or waiting.
*   **Cursor Management:** 
    *   The cursor will stay at the input prompt position.
    *   AI output will be "injected" into the terminal scrollback area by temporarily moving the cursor up, printing, and then restoring it to the input line.
*   **Streaming Fix:** AI responses will no longer break the UI "boxes" as the boxes will be simplified or eliminated in favor of direct line printing.

## 3. Aesthetics & Colors
*   **User Input Color:** Set user text to Violet (`\x1b[38;5;177m`).
*   **Refined Color Mapping:** 
    *   User text: Violet.
    *   Meow-chan text: Blue (`\x1b[38;5;111m`).
*   **Greeting Sequence:** On startup, display `src/akuma_40.txt` (black cat on dark grey pane) with a "MEOW" message.

## 4. Interaction Logic
*   **Input Clearing:** When Enter is pressed, the input box content is pushed to history (and thus the scrollback area) and cleared from the footer immediately.
*   **ESC Behavior:** Add `config.exit_on_escape` flag (default: `false`) to prevent accidental session termination.

## 5. Technical Implementation Details
*   **Cursor Persistence:** Use `\x1b[s` (Save) and `\x1b[u` (Restore) or manual coordinate management to keep the prompt responsive.
*   **Line Clearing:** Use `\x1b[K` (Erase to EOL) to keep the input area clean.
*   **Concurrency:** Implement a cooperative loop using `poll_input_event` and non-blocking TCP reads to interleave input and output without threads (if threading syscalls are unavailable).
