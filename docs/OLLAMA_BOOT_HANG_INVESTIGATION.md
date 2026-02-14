# Investigation: Meow Hanging on Boot with Ollama

## Symptom
`meow` sometimes gets stuck during the boot sequence when Ollama is already running. Interestingly, it loads instantly if Ollama is turned off, and then works fine if Ollama is turned back on after `meow` has started.

## Suspected Root Cause
The hang likely occurs in `src/main.rs` during the initialization phase, specifically in the call to `api::query_model_info`.

### Analysis of `src/main.rs`
```rust
let context_window = match api::query_model_info(&model, &current_provider) {
    Some(ctx) => ctx,
    None => DEFAULT_CONTEXT_WINDOW,
};
```
This call happens *before* the TUI starts. If this function blocks indefinitely, the application appears stuck.

### Analysis of `src/api/mod.rs`: `query_model_info` and `read_response`
The `query_model_info` function calls `read_response`, which has the following loop:

```rust
fn read_response(stream: &TcpStream) -> Result<String, ProviderError> {
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    let mut retries = 0;

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                retries = 0;
                if response.len() > 256 * 1024 { break; }
            }
            Err(e) => {
                if e.kind == libakuma::net::ErrorKind::WouldBlock
                    || e.kind == libakuma::net::ErrorKind::TimedOut
                {
                    retries += 1;
                    if retries > 100 { break; } // This is only 100ms!
                    libakuma::sleep_ms(1);
                    continue;
                }
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}
```

#### Potential Issues:
1. **Short Retry Window:** The "timeout" is only 100 iterations of 1ms sleeps (total ~100ms). If Ollama takes longer than 100ms to start sending the *first* byte of the body after the connection is established, `read_response` might return an empty string or a partial header.
2. **Blocking `TcpStream::read`:** If `libakuma`'s `TcpStream::read` is in blocking mode and doesn't return `WouldBlock`, the thread will hang there until data arrives or the connection is closed.
3. **Infinite Loop / Improper Error Handling:** If the `Err` case doesn't trigger correctly or if `read` keeps returning `WouldBlock` without ever receiving data (and the retry limit is hit too slowly or ignored), the boot sequence stalls.

## Why it works when Ollama is OFF
When Ollama is off, `connect(provider)` likely fails immediately with a "Connection refused" error. This error is caught, and `query_model_info` returns `None` quickly, allowing the boot sequence to continue with `DEFAULT_CONTEXT_WINDOW`.

## Why it hangs when Ollama is ON
When Ollama is on, the connection is accepted (`connect` succeeds). `meow` then sends a `POST /api/show` request and waits for a response. If Ollama is busy, slow, or the network stack in the OS/Kernel has latency, `meow` enters the `read_response` loop and may get stuck if the kernel's `read` call blocks or if the retry logic isn't robust enough for a slow response.

## Resolution

The hang was addressed by improving the robustness of the network response handling and providing user feedback during the boot sequence.

### Changes Made:
1. **Wall-clock Timeouts:** In `src/api/mod.rs`, `read_response` now uses `libakuma::uptime()` to enforce a strict 5-second timeout. This prevents the application from hanging indefinitely if the provider accepts a connection but never sends data.
2. **Reduced CPU Polling:** Increased the sleep duration in the retry loop from 1ms to 10ms to be more respectful of system resources while waiting for data.
3. **User Feedback:** Added a status message in `src/main.rs` (`[*] Jacking in to the matrix...`) that appears while `meow` is querying model information. This ensures the user knows the application is active and what it is waiting for.
4. **Graceful Degradation:** If the timeout is reached, the error is caught and `meow` correctly falls back to the `DEFAULT_CONTEXT_WINDOW`, allowing the session to proceed.
