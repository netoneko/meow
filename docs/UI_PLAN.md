# UI Plan for `meow` using `ratatui` and `crossterm` for SSH

## 1. Goals:

*   To provide an interactive, character-based user interface for the `meow` LLM chat client.
*   To enhance user experience beyond basic command-line input/output by offering structured display of chat history, model responses, and tool outputs.
*   To be robust and efficient when run over SSH, considering character-based output and keyboard-only interaction.

## 2. Technology Stack:

*   **UI Framework:** `ratatui` for building the terminal user interface components.
*   **Terminal Backend:** `crossterm` for low-level terminal manipulation, including raw mode, event handling (keyboard input), and screen drawing.
*   **Rust:** The existing language of the `meow` project.

## 3. Architectural Overview:

*   **Event-Driven Model:** The TUI will operate on an event loop, processing user input events (keyboard), application events (LLM response chunks, tool outputs), and rendering events.
*   **Application State:** A central `App` struct will manage the application's state, including chat history, current input buffer, selected model, LLM status (typing, waiting, error), and tool output.
*   **UI Components:** `ratatui` widgets will be used to construct the visual layout. Key components will include:
    *   **Chat History Panel:** Displays past messages, LLM responses, and tool outputs. This should support scrolling.
    *   **Input Box:** For typing user prompts and commands.
    *   **Status Bar:** Displays current model, connection status, processing indicators, and possibly short help messages.
    *   **Command Palette/Help Overlay:** A temporary overlay for displaying available commands or help information.
*   **Terminal Management:** `crossterm` will be responsible for:
    *   Entering and exiting raw mode.
    *   Entering and exiting the alternate screen buffer.
    *   Hiding and showing the cursor.
    *   Capturing keyboard events.
    *   Clearing the screen and moving the cursor.

## 4. Core Features & Implementation Details:

*   **Terminal Initialization & Cleanup:**
    *   Set terminal to raw mode (`crossterm::terminal::enable_raw_mode`).
    *   Enter alternate screen buffer (`crossterm::terminal::EnterAlternateScreen`).
    *   Hide cursor (`crossterm::cursor::Hide`).
    *   Ensure proper cleanup on exit (restore normal mode, leave alternate screen, show cursor).
*   **Main Event Loop:**
    *   Continuously poll for events (`crossterm::event::poll`).
    *   Handle `KeyEvent` for user input (typing, commands like `/clear`, `/exit`).
    *   Handle `ResizeEvent` to adapt UI layout.
    *   Integrate with `meow`'s existing LLM communication and tool calling logic.
*   **Chat History Display:**
    *   Use a `ratatui::widgets::Paragraph` or similar to display messages.
    *   Implement scrolling for the history panel.
    *   Distinguish user input, LLM responses, and tool outputs (e.g., using different colors or prefixes).
    *   Streaming LLM responses should update the last message in real-time.
*   **Input Handling:**
    *   Capture alphanumeric characters for the input buffer.
    *   Handle special keys: Enter (submit), Backspace/Delete, Arrow keys (cursor movement, history navigation), Ctrl+C/Ctrl+D (exit).
    *   Implement command parsing (e.g., `/model <name>`).
*   **Status Bar:**
    *   Display the current active LLM model.
    *   Show a simple indicator for LLM processing (e.g., "Thinking...", animated dots).
    *   Display brief error messages or connection status.
*   **SSH Considerations:**
    *   **Character-based focus:** Rely entirely on text and block-based layouts. Avoid complex Unicode unless terminal support is guaranteed.
    *   **Keyboard navigation:** Ensure all interactions are possible via keyboard.
    *   **Efficiency:** Minimize redraws; `ratatui`'s diffing mechanism helps with this.

## 5. Development Workflow (Initial Rust File):

*   Create `src/tui_app.rs` to house the TUI application logic.
*   Define `App` struct for application state.
*   Implement an `App::run()` method that encapsulates the event loop, drawing logic, and terminal management.
*   In `main.rs`, integrate by calling `tui_app::run()` when the program starts in interactive mode.
*   Add `ratatui` and `crossterm` to `Cargo.toml`.

## 6. Future Enhancements (Beyond Initial Scope):

*   Syntax highlighting for tool outputs (e.g., JSON).
*   Tab completion for commands or file paths.
*   More sophisticated loading animations.
*   Configurable UI themes.
