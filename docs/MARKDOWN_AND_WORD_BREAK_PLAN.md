# Plan: Markdown Rendering and Improved Word Breaking

This document outlines the plan to improve text rendering in Meow-chan, focusing on better word breaking and terminal Markdown support using `pulldown-cmark`.

## 1. Word Breaking Improvements

The goal is to make the terminal output more readable by handling line wraps more intelligently.

### Current Issues
- Long words are broken abruptly when they reach the terminal edge.
- Punctuation can sometimes be separated from its preceding word.
- Limited set of word boundaries (mostly just spaces and newlines).

### Proposed Improvements
- **Expanded Boundaries**: Treat characters like `-`, `/`, `\`, and `:` as potential wrap points.
- **Punctuation Sticky-ness**: Ensure punctuation (`,`, `.`, `!`, `?`, `;`, `:`) stays attached to the preceding word.
- **Hyphenation for Long Words**: If a word is longer than the available width, break it with a hyphen where appropriate (or just wrap it cleanly).
- **Refined Indentation**: Better handling of multi-line indentation for both user input (prefixed with ` > `) and LLM responses.
- **Unicode Support**: Use `unicode-width` equivalent logic to handle wide characters (if supported by the terminal).

### Technical Detail: Enhanced `flush_word`
The new logic will not only flush on spaces but also on other delimiters, while ensuring the delimiter stays with the correct "chunk".

Example of improved `flush_word`:
```rust
let is_delimiter = |c: char| c == ' ' || c == '\t' || c == '-' || c == '/' || c == '\\' || c == ':';

// During iteration:
if is_delimiter(c) {
    word_buf.push(c);
    word_display_len += 1;
    flush_word(&mut word_buf, &mut word_display_len, ...);
}
```
This ensures that something like `very-long-word` can be broken at the hyphens.

### Technical Detail: Punctuation Handling
Punctuation should be peeked or buffered to avoid being orphaned on a new line.
```rust
let is_punctuation = |c: char| c == ',' || c == '.' || c == '!' || c == '?' || c == ';' || c == ':';
```
If the next character is punctuation, it should be flushed with the current word even if it exceeds the column limit slightly (up to the last column).

### Technical Approach
Refactor the logic in `userspace/meow/src/ui/tui/render.rs`:
- Introduce a `TextState` struct to track current terminal position, indentation, and active ANSI styles.
- Implement a `process_text` function that takes a string and handles wrapping logic based on the `TextState`.

## 2. Markdown Rendering with `pulldown-cmark`

To make LLM responses more readable, we will integrate `pulldown-cmark` to render Markdown directly in the terminal using ANSI colors and styles.

### Research: `pulldown-cmark` in `no_std`
- `pulldown-cmark` supports `no_std` by disabling default features and enabling the `alloc` feature.
- It provides a pull-based API (iterator of `Event`s), which is memory-efficient and fits well with our architecture.

### Implementation Details
- **Dependency**: Add `pulldown-cmark = { version = "0.12", default-features = false, features = ["alloc"] }` to `Cargo.toml`.
- **Markdown Renderer**:
    - Create a new module (e.g., `userspace/meow/src/ui/tui/markdown.rs`) to handle the translation of Markdown events to ANSI sequences.
    - Support:
        - **Bold/Italic**: Use ANSI bold and underline/italic.
        - **Headers**: Larger/colored text or underlined.
        - **Code Blocks**: Different background or color (e.g., DIM or specialized gray).
        - **Lists**: Bullet points and numbered lists with proper indentation.
        - **Links**: Display as `Text [URL]`.

### Streaming vs. Final Rendering
- **Streaming**: Markdown parsing is tricky during streaming because tags might be incomplete.
    - *Option A*: Continue printing raw text during streaming, then clear and re-render the full message as Markdown once finished.
    - *Option B*: Implement a "lookahead" or buffer-based streaming parser that can handle common tags (like bold) as they appear.
- **Initial Phase**: We will start with **Option A** for simplicity and reliability.

## 3. Integration Plan

1.  **Phase 1: Word Break Refactor**
    - Update `tui_print_with_indent` in `render.rs` to use a more robust wrapping algorithm.
    - Test with long URLs and technical paths.

2.  **Phase 2: Add `pulldown-cmark`**
    - Update `Cargo.toml`.
    - Implement the `MarkdownRenderer` in a new module.

3.  **Phase 3: LLM Response Rendering**
    - Update `chat_once` in `chat.rs` to use the Markdown renderer for the final assistant message.
    - Update user input printing to use the improved word breaking.

4.  **Phase 4: Optimization**
    - Optimize memory usage of the Markdown parser.
    - Explore incremental rendering for a better streaming experience.
