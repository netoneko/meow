# Plan: Atomic Refactor for Meow TUI

To improve code safety, remove `unsafe` blocks from general logic, and prepare for potential future multi-threading, all `static mut` variables in `src/tui_app.rs` will be replaced with thread-safe alternatives.

## 1. Simple Type Conversion
The following variables will be converted directly to their `Atomic` equivalents:
- `HISTORY_INDEX: usize` -> `AtomicUsize`
- `LAST_INPUT_TIME: u64` -> `AtomicU64`
- `LAST_HISTORY_KB: usize` -> `AtomicUsize`

## 2. Complex Type Wrapper (`ThreadSafeCell`)
Since Akuma userspace is currently single-threaded but we want to avoid `static mut`, we will implement a `ThreadSafeCell` wrapper that uses an `AtomicBool` for a simple "lock" and `UnsafeCell` for the data.

```rust
struct ThreadSafeCell<T> {
    data: UnsafeCell<Option<T>>,
    locked: AtomicBool,
}
```

This will be used for:
- `PANE_LAYOUT: Option<PaneLayout>`
- `GLOBAL_INPUT: Option<String>`
- `MESSAGE_QUEUE: Option<VecDeque<String>>`
- `COMMAND_HISTORY: Option<Vec<String>>`
- `SAVED_INPUT: Option<String>`
- `MODEL_NAME: Option<String>`
- `PROVIDER_NAME: Option<String>`
- `RAW_INPUT_QUEUE: Option<VecDeque<u8>>`

## 3. Implementation Steps
1. **Define `ThreadSafeCell`**: Implement a simple wrapper in `tui_app.rs` (or a common location).
2. **Refactor Statics**: Update variable definitions to use `Atomic*` and `ThreadSafeCell`.
3. **Update Call Sites**: Replace `unsafe` access with safe method calls (e.g., `.load()`, `.store()`, or `.with_data(|d| ...)`).
4. **Remove Unsafe Blocks**: Delete the now-redundant `unsafe` blocks throughout `tui_app.rs`.

## 4. Safety Justification
While `ThreadSafeCell` still uses `UnsafeCell` internally, it encapsulates the unsafety. In Akuma's single-threaded cooperative multitasking environment, this pattern provides sufficient protection against concurrent access while satisfying the Rust compiler's requirements for global state.
