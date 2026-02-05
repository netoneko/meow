# Meow TUI Refactor (February 2026)

This document summarizes the major overhaul of the Meow-chan TUI to improve responsiveness, input handling, and overall user experience.

## Major Changes

### 1. Removal of Classic UI
The legacy non-TUI interactive mode has been removed. All interactive sessions now use the optimized TUI. One-shot mode remains available for single queries via command-line arguments.

### 2. Message Queuing & Non-Blocking Input
- Implemented a `MessageQueue` using `VecDeque` to allow typing and queuing messages while the AI is responding.
- Replaced blocking `sleep_ms` with `poll_sleep` during connection retries and streaming to keep the UI responsive.
- Integrated `tui_handle_input` into the streaming loop to process characters in real-time.

### 3. Advanced Line Editor
A robust line editor was implemented for the prompt box, supporting:
- **Cursor Navigation**: Left/Right arrows to move within the input line.
- **Traversal**: `Ctrl+A` (Home), `Ctrl+E` (End).
- **Word Editing**: `Ctrl+W` (Delete previous word), `Alt+B` / `Alt+F` (Jump back/forward by word).
- **Clearing**: `Ctrl+U` to clear the entire input line.
- **Command History**: Up/Down arrows to navigate through previous inputs (saved across AI responses).

### 4. Layout & Rendering Improvements
- **4-Line Footer**: 
  - Row h-3: Separator line.
  - Row h-2: Status bar showing current Provider and Model.
  - Row h-1 & h: Multi-line prompt area.
- **Absolute Positioning**: Replaced unreliable `SAVE_CURSOR`/`RESTORE_CURSOR` with absolute coordinate tracking (`CUR_ROW`, `CUR_COL`).
- **Atomic Coloring**: Fixed "color bleeding" by ensuring color codes and content are printed together and resetting colors in the prompt.
- **Alternate Screen Buffer**: Meow now uses the terminal's alternate screen buffer (`?1049h`), ensuring a clean exit that restores the original terminal state while preserving scrollback.

### 5. New Commands
- `/hotkeys` / `/shortcuts`: Displays a formatted table of all input shortcuts.
- Updated `/help` with better alignment and updated branding.

## Known Issues

### Shift+Enter (Multiline Input)
- **Status**: Not working.
- **Description**: Although the internal logic supports `
` characters for multiline input, most terminal emulators send the same byte code for `Enter` and `Shift+Enter` in raw mode, or use complex sequences that are not yet fully mapped in the `poll_input_event` loop. Currently, pressing Enter typically submits the message immediately.
