# Proposal: Eliminating Heap Churn via Zero-Allocation Formatting

## Status
**Proposed**

## Background
The `meow` application currently crashes after extended use due to "Virtual Address Space Exhaustion". This is caused by the combination of:
1.  **High-Frequency TUI Loop**: Updates the screen 20 times per second.
2.  **Heap-Based Formatting**: The `format!` macro creates a new `alloc::string::String` for every UI update.
3.  **Kernel Constraint**: The kernel's `mmap` allocator never reclaims virtual address ranges for a process, and `libakuma` performs a `mmap` for every allocation.

Every `format!` call consumes one of the process's ~196,320 lifetime allocation slots.

## Proposed Solution: Zero-Allocation UI Rendering
We should transition all high-frequency UI rendering from `format!` (which allocates on the heap) to `write!` (which formats directly into a stream/buffer).

### 1. Leverage Existing `Stdout` Wrapper
We already have a `Stdout` wrapper in `src/ui/tui/render.rs` that implements `core::fmt::Write`. We should make this the primary interface for TUI updates.

### 2. Implementation Strategy

#### A. Direct FD Writing
Replace:
```rust
let info = format!("  {}[Provider: {}]", COLOR_GRAY_DIM, prov_n);
akuma_write(fd::STDOUT, info.as_bytes()); // Consumes 1 slot
```
With:
```rust
use core::fmt::Write;
let mut stdout = Stdout;
write!(stdout, "  {}[Provider: {}]", COLOR_GRAY_DIM, prov_n); // Consumes 0 slots
```

#### B. Stack-Buffered Formatting (If needed)
For complex strings that need to be passed to existing functions, use a stack-allocated buffer:
```rust
let mut buf = [0u8; 128];
let mut wrapper = StackBuffer::new(&mut buf);
write!(wrapper, "Tokens: {}", count);
tui_print(wrapper.as_str());
```

## Impact
- **Memory Lifetime**: `meow` will be able to run indefinitely without triggering OOM.
- **Performance**: Reduced overhead from skipping `mmap`/`munmap` syscalls and page table modifications.
- **Stability**: Elimination of memory churn makes the application much more robust against kernel-level address space limitations.

## Action Plan
1.  Update `src/ui/tui/render.rs` to replace all `format!` calls in `render_footer` with `write!(Stdout, ...)`.
2.  Update `src/tui_app.rs` to use `write!` for status updates.
3.  Audit `src/api/client.rs` for progress-tracking `format!` calls during streaming.
