# Meow TUI Prompt & Footer Improvements (Feb 2026)

This document summarizes the recent UI/UX enhancements made to the Meow TUI, specifically focusing on the prompt prefix, footer consistency, and multiline editing.

## 1. Prompt Prefix & Layout Fixes
The prompt line in the footer has been refined for better visual clarity and accuracy.
- **Accurate Cursor Positioning**: Implemented a `visual_length` helper that calculates string width while ignoring ANSI escape sequences (colors). This ensures the user's cursor starts exactly after the `> ` invitation, regardless of how many colors are used in the prefix.
- **Eliminated Dead Space**: Removed redundant spaces between the system metrics and the cyberpunk cat-face invitation `(=^･ω･^=)`.
- **Queued Message Logic**: The `[QUEUED: X]` indicator now only appears when messages are actually waiting, preventing unnecessary gaps in the prompt line when idle.

## 2. Persistent History Monitoring
The memory indicator in the footer is now stable and always visible.
- **State Persistence**: Switched from passing history size through function arguments to using a `LAST_HISTORY_KB` static variable.
- **Continuous Display**: The history size (e.g., `Hist: 8K`) now remains visible during streaming and while the user is typing, providing constant feedback on memory pressure.

## 3. Multiline Edit Padding
To improve readability during complex queries, multiline input now features consistent indentation.
- **Left Padding**: Lines after the first (created via `Shift+Enter` or automatic wrapping) are now indented by 4 spaces.
- **Coordinated Logic**: The indentation is synchronized across the wrapping calculator, cursor positioning engine, and the actual rendering loop to ensure a seamless "block" look for multiline messages.

## 4. Implementation Details
- **Location**: All changes are contained within `userspace/meow/src/tui_app.rs`.
- **Constants**: Added `PROMPT_MULTILINE_INDENT: usize = 4`.
- **State**: Added `LAST_HISTORY_KB: static mut usize`.
