# Meow-chan Hotkey Specification (February 2026)

This document defines the standard hotkeys for the Meow-chan TUI and the required handling logic to ensure consistent behavior across different terminal states (Idle vs. AI Streaming).

## Core Principles

1.  **Unified Handling**: Input processing must use the same logic regardless of whether the AI is responding or the application is idle.
2.  **Sequence Robustness**: The parser must handle multi-byte ANSI/CSI sequences atomically to avoid misinterpreting a leading `ESC` (0x1B) as a "Cancel" or "Quit" command.
3.  **Non-Blocking**: The parser must operate on buffered input without blocking the UI thread.

## Hotkey Definitions

### Navigation
| Key | Sequence (Common) | Action |
| :--- | :--- | :--- |
| **Left Arrow** | `\x1b[D` | Move cursor one character left |
| **Right Arrow** | `\x1b[C` | Move cursor one character right |
| **Up Arrow** | `\x1b[A` | Previous history item (Idle only) |
| **Down Arrow** | `\x1b[B` | Next history item (Idle only) |
| **Home** / `Ctrl+A` | `\x1b[H` / `\x01` | Move cursor to start of line |
| **End** / `Ctrl+E` | `\x1b[F` / `\x05` | Move cursor to end of line |
| **Alt+Left** / `Alt+B` | `\x1b[1;3D` / `\x1b b` | Move back one word |
| **Alt+Right** / `Alt+F`| `\x1b[1;3C` / `\x1b f` | Move forward one word |

### Editing
| Key | Sequence | Action |
| :--- | :--- | :--- |
| **Backspace** | `\x7f` / `\x08` | Delete character before cursor |
| **Delete** | `\x1b[3~` | Delete character at cursor |
| **Ctrl+W** | `\x17` | Delete previous word |
| **Ctrl+U** | `\x15` | Clear entire input line |

### Execution & Control
| Key | Sequence | Action |
| :--- | :--- | :--- |
| **Enter** | `` (0x0D) | Submit current input to queue |
| **Shift+Enter** | `\x1b[13;2u` / `
` | Insert a newline (`
`) at cursor |
| **Alt+Enter** | `\x1b` | Insert a newline (`
`) at cursor |
| **ESC** | `\x1b` (alone) | Cancel AI response (Streaming) / Exit (if configured) |
| **Ctrl+L** | `\x0c` | Force UI redraw / Re-probe terminal size |

## Handling Strategy: The Input State Machine

To prevent "premature terminations" (where a sequence like `\x1b[D` is split into `ESC` and `[D`), the input handler must:

1.  **Buffer All Bytes**: Read all available bytes from the input stream into a temporary buffer.
2.  **Check for Sequences**:
    *   If the buffer starts with `0x1B`:
        *   If it's just `0x1B` and no more data is arriving (short timeout), it's a standalone **ESC**.
        *   If it matches a known sequence (CSI/SS3), process it and consume those bytes.
        *   If it's an incomplete sequence, wait for more data or timeout.
    *   If the buffer starts with a standard ASCII/UTF-8 character:
        *   Process as text input.

## Multi-line Input Handling

When a "Newline" action is triggered (Shift+Enter, Alt+Enter):
1.  Insert `
` at the current `CURSOR_IDX`.
2.  Increment `CURSOR_IDX`.
3.  Trigger a footer redraw.
4.  The renderer must continue to display the multi-line input in the footer area (Rows h-2 and h-1).

## AI Streaming State
During streaming, the `poll_sleep` loop calls `tui_handle_input`. This function MUST support navigation (Left/Right) and multi-line editing so the user can prepare their next prompt while the AI is talking.
