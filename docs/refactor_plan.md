# Meow-chan Codebase Refactoring & Simplification Plan

## Goals
- **Modularity**: Break down massive files (`main.rs`, `tools.rs`, `tui_app.rs`) into smaller, domain-specific modules.
- **Memory Efficiency**: Implement stricter memory management, avoid unnecessary allocations, and ensure buffers are properly shrunk or reused.
- **Thread Safety**: Prepare for a multi-threaded future using `core::sync::atomic` primitives for shared state instead of raw `UnsafeCell` where possible.
- **Interface Stability**: Maintain the existing CLI and TUI interface.

## Proposed Structure

### 1. `src/app/` - Core Application Logic
- `history.rs`: Message history management.
    - `Message` struct and JSON serialization.
    - `trim_history`, `compact_history`, and token estimation.
- `chat.rs`: High-level chat orchestration.
    - `chat_once` implementation.
    - Tool execution loop and retry logic.
- `commands.rs`: Slash command handling (`/model`, `/clear`, etc.).

### 2. `src/api/` - Provider & Communication
- `client.rs`: Unified HTTP/HTTPS streaming client.
    - Retry logic with exponential backoff.
    - Header management and status code handling.
- `ollama.rs` & `openai.rs`: Provider-specific request building and response parsing.
- `types.rs`: Shared API types (ModelInfo, StreamStats, etc.).

### 3. `src/tools/` - Tooling Framework
- `context.rs`: Working directory and sandbox management using Atomics.
- `registry.rs`: Tool dispatcher and JSON parsing helpers.
- `fs.rs`: File system operations (read, write, list, edit, etc.).
- `net.rs`: Network operations (`HttpFetch`).
- `shell.rs`: Process spawning and pipe reading.
- `git.rs` & `chainlink.rs`: Specialized wrappers for external binaries.
- `discovery.rs`: Logic for finding tool calls in LLM output.

### 4. `src/ui/` - User Interface
- `tui/`:
    - `layout.rs`: `PaneLayout` and screen boundary management.
    - `input.rs`: Keyboard event parsing and input handling.
    - `render.rs`: Footer and status bar rendering.
- `output.rs`: TUI-aware printing, word-wrapping, and color management.
- `one_shot.rs`: Simple CLI output for non-interactive mode.

### 5. `src/main.rs` - Entry Point
- Minimalist: handles argument parsing, config loading, and starts the requested mode.

---

## Memory Efficiency & Leak Prevention
- **Buffer Reuse**: Use `Vec::clear()` instead of reallocating buffers in the streaming loop.
- **Aggressive Compaction**: Ensure `shrink_to_fit()` is called on large strings (like LLM responses) after processing.
- **History Limits**: Strictly enforce `MAX_HISTORY_SIZE` and implement token-based pruning.
- **Leak Tracking**: In `no_std`, we must be careful with `alloc`. Ensure all `Vec` and `String` objects have a clear owner and lifecycle.

## Thread Safety & Future Proofing
- Replace any existing `UnsafeCell` patterns with `AtomicBool`, `AtomicUsize`, or `AtomicPtr` where appropriate.
- Use `Ordering::SeqCst` for critical state transitions (like TUI activation or cancellation).
- Encapsulate global state into a `State` struct managed by `AtomicPtr` if dynamic reconfiguration is needed.

## Implementation Phases

### Phase 1: Tool Decoupling
1. Split `tools.rs` into `tools/fs.rs`, `tools/shell.rs`, and a central `tools/mod.rs`.
2. Move JSON parsing helpers to a utility module.
3. Replace `WorkingDirState` with an atomic-backed implementation.

### Phase 2: API & Client Refactor
1. Move HTTP/HTTPS streaming logic from `main.rs` to `api/client.rs`.
2. Standardize `Provider` interactions via a trait or unified enum.
3. Clean up the manual JSON string building in favor of a simpler builder pattern.

### Phase 3: TUI Modularization
1. Extract `InputEvent` and parsing logic to `ui/tui/input.rs`.
2. Move `PaneLayout` to `ui/tui/layout.rs`.
3. Separate the "Application Loop" from the "Rendering Logic".

### Phase 4: Core Logic & main.rs Cleanup
1. Move `chat_once` and related logic to `app/chat.rs`.
2. Move slash commands to `app/commands.rs`.
3. Shrink `main.rs` to < 200 lines.
