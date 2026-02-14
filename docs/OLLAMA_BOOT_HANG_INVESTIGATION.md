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

## Proposed Fixes
1. **Asynchronous/Non-blocking Initialization:** Move the `query_model_info` call inside the TUI or perform it in the background so it doesn't block the UI from appearing.
2. **Robust Timeouts:** Implement a proper wall-clock timeout (e.g., 2 seconds) for the entire `query_model_info` operation.
3. **Failure Tolerance:** If `query_model_info` takes more than a few hundred milliseconds, abort and use the default context window.
4. **Improved Logging:** Add debug prints (or a "Neko is thinking..." splash screen) during boot so the user knows what the app is waiting for.
