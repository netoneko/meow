# Meow-chan TUI: Final Extra Plan Mockup

This mockup establishes the layout for the "Natural Scrollback" interface. It uses a fixed scrolling region for chat history and a persistent, multi-line footer for metrics and input.

## Startup Greeting
```text
[ GREETING AREA ]
(src/akuma_40.txt rendered with \x1b[38;5;236m background)
  
  ███╗   ███╗███████╗ ██████╗ ██╗    ██╗
  ████╗ ████║██╔════╝██╔═══██╗██║    ██║
  ██╔████╔██║█████╗  ██║   ██║██║ █╗ ██║
  ██║╚██╔╝██║██╔══╝  ██║   ██║██║███╗██║
  ██║ ╚═╝ ██║███████╗╚██████╔╝╚███╔███╔╝
  ╚═╝     ╚═╝╚══════╝ ╚═════╝  ╚══╝╚══╝ 
```

## Main Interface (Standard 25x100)
Lines 1-21 are the "Scrollback Zone".
Lines 22-25 are the "Footer Zone".

```text
║  [MEOW] *ears twitch* Sure thing nya~! I'll jack into the VFS and get        ║
║         a listing for you. Just a sec...                                     ║
║                                                                              ║
║  [*] >> EXECUTING: FileList { path: "/" }                                    ║
║  [*] << SUCCESS: bin/ etc/ public/ scripts/ tmp/ var/                        ║
║                                                                              ║
║  [MEOW] Here's the local grid layout choom! Anything else you need?          ║
║                                                                              ║
╟──────────────────────────────────────────────────────────────────────────────╢
║ [ 342 / 128k tokens ] [ 💾 1.2M RAM ]                                        ║
║ > type your message here_                                                    ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

## Visual Rules & Logic

### 1. Colors (Tokyo Night Theme)
*   **User Input:** `\x1b[38;5;177m` (Violet)
*   **Meow Output:** `\x1b[38;5;111m` (Sky Blue)
*   **System/Tools:** `\x1b[38;5;120m` (Emerald Green)
*   **Frames/Borders:** `\x1b[38;5;242m` (Dark Gray)
*   **Metrics:** `\x1b[38;5;215m` (Soft Orange)

### 2. The Scroll Region (`DECSTBM`)
*   To avoid "overwriting" the prompt when the AI speaks, we set a terminal scroll region from line 1 to `Height - 4`.
*   When text is printed, the terminal handles scrolling *only* within those lines.
*   The Footer (Lines `Height-3` to `Height`) remains stationary and is never scrolled away.

### 3. Alignment & Wrapping
*   **Indentation:** Assistant messages start with `[MEOW] ` (7 chars). Every subsequent line of that message *must* be indented by 7 spaces to match.
*   **Prefixes:**
    *   User: `> ` (Color: Violet)
    *   Meow: `[MEOW] ` (Color: Blue)
    *   System: `[*] ` (Color: Green)

### 4. Concurrency Flow
1.  User types in the footer area.
2.  Cursor stays at the end of the user's current input line.
3.  AI data arrives:
    *   Save cursor position (`\x1b[s`).
    *   Move to the bottom-most line of the *Scroll Region*.
    *   Print chunk.
    *   Restore cursor position (`\x1b[u`).
4.  User sees their typing interrupted only by the millisecond-fast cursor jump, making it look like simultaneous action.

### 5. Expanding Input
*   As the `input.len()` exceeds `terminal_width - 4`, the footer border moves *up* one row, and the scroll region size is decreased by one.