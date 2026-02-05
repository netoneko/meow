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
Lines 1 to (Height-3) are the "Scrollback Zone".
Lines (Height-2) to Height are the "Footer Zone".

```text
  [MEOW] *ears twitch* Sure thing nya~! I'll jack into the VFS and get
         a listing for you. Just a sec...

  [*] >> EXECUTING: FileList { path: "/" }
  [*] << SUCCESS: bin/ etc/ public/ scripts/ tmp/ var/

  [MEOW] Here's the local grid layout choom! Anything else you need?

----------------------------------------------------------------------------------------------------
 [ TOKENS: 342 / 128k ] [ MEM: 1.2M ]
 > type your message here_
```

## Visual Rules & Logic

### 1. Colors (Tokyo Night Theme)
*   **User Input:** `\x1b[38;5;177m` (Violet)
*   **Meow Output:** `\x1b[38;5;111m` (Sky Blue)
*   **System/Tools:** `\x1b[38;5;120m` (Emerald Green)
*   **Frame/Separator:** `\x1b[38;5;242m` (Dark Gray)
*   **Metrics:** `\x1b[38;5;215m` (Soft Orange)

### 2. The Scroll Region (`DECSTBM`)
*   Separates the chat history from the footer.
*   The Footer remains stationary.

### 3. Alignment & Wrapping
*   **Indentation:** Assistant messages are indented by 7 spaces on wrap.
*   **Prefixes:**
    *   User: `> ` (Violet)
    *   Meow: `[MEOW] ` (Blue)
    *   System: `[*] ` (Green)

### 4. Interactive Consistency
*   User can type in the prompt area at any time.
*   Commands (`/help`, etc.) are intercepted and results are printed into the scroll region.
*   The prompt disappears from the input box and appears in the history as soon as Enter is pressed.
