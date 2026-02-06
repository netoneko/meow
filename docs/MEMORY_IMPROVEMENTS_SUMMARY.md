# Meow Memory Improvements (Feb 2026)

This document summarizes the memory usage optimizations and new monitoring features implemented for Meow.

## 1. Tool Output Overflow to Disk
To prevent Out of Memory (OOM) crashes when tools produce large amounts of data, Meow now offloads results exceeding a specific limit to disk.
- **Limit**: `MAX_TOOL_OUTPUT_SIZE` set to 32KB (in `src/config.rs`).
- **Mechanism**: Outputs larger than 32KB are written to a unique file in the sandbox's `/tmp/` directory (e.g., `/tmp/meow_tool_1738854000.txt`).
- **LLM Feedback**: The assistant receives a truncated preview and the path to the full file, with instructions on how to access it (using `FileReadLines` or `CodeSearch`).

## 2. Shell Tool Hard Limit
A hard limit has been added to the `Shell` tool to catch runaway processes.
- **Limit**: 1MB.
- **Action**: If a shell command produces more than 1MB of output, the process is killed, and an error is returned to the assistant.

## 3. NDJSON Streaming Optimization
The streaming response parser previously used inefficient string re-allocation for processing lines.
- **Fix**: Replaced `pending_lines = String::from(&pending_lines[newline_pos + 1..])` with `pending_lines.drain(..newline_pos + 1)`.
- **Result**: Significant reduction in heap fragmentation and allocation overhead during LLM streaming.

## 4. Proactive History Management
Message history is now managed more aggressively to keep the heap footprint small.
- **Frequent Compaction**: `trim_history()` and `compact_history()` (which calls `shrink_to_fit()` on all strings) are now called after every tool iteration and at the end of every user turn.

## 5. Memory Monitoring (TUI)
A new indicator in the TUI footer helps users monitor memory pressure in real-time.
- **History Indicator**: Displays `Hist: XX KB` in the status bar.
- **Color Coding**: 
  - **Yellow**: > 128KB (Warning: consider `/clear` or `CompactContext`).
  - **Red**: > 256KB (Critical: high risk of OOM).

## 6. Implementation Files
- `src/config.rs`: Constants definition.
- `src/tools.rs`: Overflow and shell limit logic.
- `src/main.rs`: Streaming parser and history management updates.
- `src/tui_app.rs`: Memory indicator and history calculation.
