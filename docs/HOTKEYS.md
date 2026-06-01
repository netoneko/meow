# Keyboard Shortcuts

## Navigation

| Key | Action |
|-----|--------|
| Left / Right arrow | Move cursor one character |
| Up arrow | Previous history item (or move up in multi-line input) |
| Down arrow | Next history item (or move down in multi-line input) |
| Home / Ctrl+A | Move to start of line |
| End / Ctrl+E | Move to end of line |
| Alt+Left / Alt+B | Move back one word |
| Alt+Right / Alt+F | Move forward one word |

## Editing

| Key | Action |
|-----|--------|
| Backspace | Delete character before cursor |
| Delete | Delete character at cursor |
| Ctrl+W | Delete previous word |
| Ctrl+U | Clear entire input line |

## Submission & Control

| Key | Action |
|-----|--------|
| Enter | Submit input |
| Shift+Enter / Ctrl+J | Insert newline (multi-line input) |
| ESC / Ctrl+C | Cancel active AI request |
| Ctrl+L | Force redraw and re-probe terminal size |

## Notes

- Some terminals intercept Ctrl+W, Ctrl+U, or Ctrl+C before meow sees them.
- ESC exits the app if `exit_on_escape=true` is set in `/etc/meow/config`.
- Multi-line input is supported: the footer grows to show additional lines as you type.

## Input State Machine

The parser buffers all available bytes before interpreting them to avoid splitting multi-byte ANSI sequences. An ESC byte (`0x1B`) is only treated as a standalone Escape if no continuation bytes arrive within a short timeout, preventing false cancels from arrow keys.
