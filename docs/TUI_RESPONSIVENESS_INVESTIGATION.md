# Investigation: TUI Freezing During "Waiting" Phase

## Symptom
When `meow` sends a request to a provider (like Ollama), the TUI freezes completely while waiting for the response to begin. 
- The timer gets stuck (e.g., at `50ms`).
- The ticker (`. . .`) stops moving.
- User input (typing in the prompt box) is not reflected on screen.
- Responsiveness returns once the LLM starts "streaming" tokens.

## Root Cause: Blocking I/O
The investigation revealed that `libakuma::net::TcpStream::read` uses the `recv` syscall with `flags = 0`. In the Akuma kernel (and standard POSIX), this is a **blocking operation**.

### Why Streaming Works
During streaming, the LLM provider sends data packets frequently. The `read()` call returns almost immediately with new data, allowing the TUI loop to finish its iteration and reach the input-handling and rendering code before starting the next `read()`.

### Why Waiting Fails
When the LLM is "thinking," the provider sends nothing for several seconds. `meow` calls `stream.read()`, and the kernel puts the process to sleep until data arrives. Because the process is asleep inside the `read()` syscall, it cannot:
1.  Check for keyboard events (`tui_handle_input`).
2.  Increment the ticker/timer based on wall-clock time.
3.  Repaint the screen (`render_footer`).

## Fix Plan: Implementing Non-Blocking I/O

The goal is to allow the TUI to remain active even when no network data is available.

### Phase 1: libakuma Enhancement (Requires access to `../libakuma`)
1.  **Define Constants:** Add `pub const MSG_DONTWAIT: i32 = 0x40;` to `libakuma/src/lib.rs` inside `socket_const`.
2.  **Expose Non-Blocking Read:** Add a method to `TcpStream` (or modify the existing one) to support passing flags to the underlying `recv` syscall.
    ```rust
    // Proposed addition to TcpStream in libakuma/src/net.rs
    pub fn read_nonblocking(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let ret = crate::recv(self.fd, buf, crate::socket_const::MSG_DONTWAIT);
        if ret < 0 {
            Err(Error::from_errno((-ret) as i32))
        } else {
            Ok(ret as usize)
        }
    }
    ```

### Phase 2: meow Integration
1.  **Update Network Loops:** Modify `src/api/client.rs` to use non-blocking reads in `read_streaming_response_with_progress` and the TLS equivalent.
2.  **Handle `WouldBlock`:** When `read` returns `ErrorKind::WouldBlock`:
    - Call `tui_handle_input()` to process any pending keystrokes.
    - Call `render_footer()` to update the timer and ticker.
    - Yield the CPU for a short duration (e.g., `libakuma::sleep_ms(10)`) to prevent 100% CPU usage.
    - Loop back and try reading again.

## Expected Outcome
After these changes, the "waiting" phase will behave identically to the "streaming" phase from the user's perspective: the timer will tick smoothly, the ticker will animate, and characters will appear in the prompt box as they are typed, even while the LLM is still processing the request.
