# Bye-Bye Classic UI: Migration Plan

This document outlines the steps to remove the "classic" interactive mode from the `meow` client and consolidate all interactive logic into the TUI. This refactoring is necessary to fix input blocking and enable message queuing.

## Phase 1: Preparation & Cleanup

1.  **Remove CLI Flags**:
    *   Delete `--classic` from `main.rs`.
    *   Make `--tui` the implicit only interactive mode.
    *   Keep one-shot mode (message as positional argument).

2.  **Consolidate Command Handling**:
    *   Currently, both `main.rs` and `tui_app.rs` call `handle_command`. Ensure all interactive commands are routed through the TUI version.

3.  **Delete Dead Code**:
    *   Remove `read_line` from `main.rs`.
    *   Remove the `Interactive mode` loop block in `main.rs`.
    *   Remove the `TUI_ACTIVE` atomic flag and `print` redirection logic.

## Phase 2: TUI Enhancement (Fixing Input/Queuing)

1.  **Refactor Input State**:
    *   Replace the static `GLOBAL_INPUT` string with a structured `TuiState` or enhance it.
    *   Implement a `MessageQueue` (using `VecDeque`) to store pending user messages.

2.  **Non-Blocking Event Loop**:
    *   Update `tui_handle_input` to recognize `` or `
`.
    *   When Enter is pressed during a model response, push the current input to the `MessageQueue` instead of ignoring it.
    *   Update the UI to show "Queued" messages or a processing indicator.

3.  **Refactor Rendering**:
    *   Cleanup the cursor management (`SAVE_CURSOR`, `RESTORE_CURSOR`).
    *   Ensure the footer (prompt area) is always updated independently of the scrollback zone.

## Phase 3: Verification

1.  **Test One-shot**: `meow "hi"` should still work without TUI.
2.  **Test Interactive**: `meow` should launch TUI immediately.
3.  **Test Queuing**: Type a message while the AI is responding and hit Enter. Verify it processes immediately after the current response finishes.
