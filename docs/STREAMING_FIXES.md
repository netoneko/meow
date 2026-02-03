# Streaming Response Fixes

This document describes issues found in meow's streaming response handling and the fixes applied.

## Issue 1: Model Output Truncation (Primary)

### Symptom

Models would start responding, mention they're about to use a tool, but then stop without outputting the tool call JSON:

```
[jacking in..] ~(=^‥^)ノ [10.2s]
I'll check the src/main.rs file for any issues! (◕‿◕)

First, let me confirm if the file exists~

(=^･ω･^=) >
```

The model intended to call `FileRead` but the response ended before the JSON block.

### Cause

The API requests did not specify a `max_tokens` (OpenAI) or `num_predict` (Ollama) parameter. Models have conservative default output limits (often 2048-4096 tokens). When the model is verbose before outputting a tool call, it can hit this limit and get truncated mid-response.

### Fix

Added `DEFAULT_MAX_TOKENS = 16384` and included it in all API requests:

```rust
// Ollama
"options":{"num_predict":16384}

// OpenAI
"max_tokens":16384
```

This parameter limits only the **output** of each individual response, not the entire session. The full conversation history is still sent with each request. If the model has less capacity available (due to long history), it will generate fewer tokens - this is just an upper bound.

## Issue 2: Incomplete Stream Processing (Secondary)

### Symptom

Occasionally, the final content from a streaming response could be lost.

### Cause

Both HTTPS and HTTP streaming handlers only processed lines that ended with `\n`. If the final chunk of data from the server didn't have a trailing newline, it would remain in the pending buffer when the connection closed, and never be processed.

**HTTPS path:**
```rust
StreamResult::Done => {
    break;  // Exited without processing remaining pending_lines
}
```

**HTTP path:**
```rust
Ok(0) => {  // EOF
    break;  // Exited without processing remaining pending_data
}
```

### Fix

Added processing of remaining data when the stream ends:

**HTTPS path:**
```rust
StreamResult::Done => {
    let remaining = pending_lines.trim();
    if !remaining.is_empty() {
        if let Some((content, _done)) = parse_streaming_line(remaining, provider) {
            // Process remaining content
        }
    }
    break;
}
```

**HTTP path:**
```rust
Ok(0) => {
    if let Ok(remaining_str) = core::str::from_utf8(&pending_data) {
        for line in remaining_str.trim().lines() {
            // Process remaining lines
        }
    }
    break;
}
```

## Issue 3: Stream Interruption Recovery

### Symptom

The model's response gets cut off mid-stream (connection closes before `done=true`), but meow would either:
1. Silently accept the truncated response as complete, or
2. Retry from scratch, losing the partial response and re-evaluating the entire context

Example output showing truncation:
```
[continuing..] ~(=^‥^)ノ [18.6s]
I'll create the files with the prompts~ (ฅ^•ﻌ•^ฅ)

First, let me write the coding prompt to prompts/002.txt:Intent phrases: 3, tools called: 0
```

The response ended at "002.txt:" without the tool call JSON.

### Cause

When the server closes the connection (`Ok(0)` in HTTP, `StreamResult::Done` in TLS) before sending the done signal, the code was returning whatever partial data was received as a successful response. This meant:
- Partial responses were treated as complete
- The retry logic never triggered (only triggers on `Err`)
- Even if retry did trigger, it would regenerate the entire response from scratch

### Fix: Append Partial + Continue

Instead of retrying from scratch, meow now detects incomplete streams and asks the model to continue:

1. **Added `StreamResponse` enum**:
```rust
enum StreamResponse {
    Complete(String),  // Server sent done signal
    Partial(String),   // Connection closed before done signal
}
```

2. **Streaming functions track completion**:
```rust
let mut stream_completed = false;
// ... set to true only when done signal received ...

if !stream_completed && !full_response.is_empty() {
    print("\n[!] Stream interrupted, will continue...\n");
    return Ok(StreamResponse::Partial(full_response));
}
```

3. **Main loop handles partial responses**:
```rust
let stream_result = send_with_retry(model, provider, history, iteration > 0)?;

let assistant_response = match stream_result {
    StreamResponse::Complete(response) => response,
    StreamResponse::Partial(partial) => {
        // Add partial as assistant message
        history.push(Message::new("assistant", &partial));
        // Ask model to continue
        history.push(Message::new("user", 
            "[System: Your response was cut off mid-stream. Please continue exactly where you left off.]"));
        continue;  // Next iteration continues from where we left off
    }
};
```

### Benefits

- **No regeneration**: The partial response is preserved, model continues from where it stopped
- **KV cache efficiency**: Ollama caches key-value states for prompts, so the unchanged prefix evaluates much faster
- **No duplicate content**: Model sees its own partial output and continues naturally
- **Bounded by MAX_TOOL_ITERATIONS**: Safety limit (20) prevents infinite continuation loops

### User-visible behavior

When a stream is interrupted:
```
[jacking in..] ~(=^‥^)ノ [5.2s]
I'll read the file to check...
[!] Stream interrupted, will continue...
[continuing..] ~(=^‥^)ノ [1.8s]

```json
{"command": "FileRead", "path": "src/main.rs"}
```

The second request is typically faster due to Ollama's KV cache.

## Notes

- The `max_tokens` fix is the primary solution for the "forgets to call tools" issue
- The stream processing fix is a safety net for edge cases where connections close unexpectedly
- The stream continuation fix handles cases where the connection drops mid-response
- When the model properly sends its done signal (`"done":true` for Ollama, `data: [DONE]` for OpenAI), we return immediately
- The 16384 token limit is generous enough for most responses while being within typical model context windows

## Related Files

- `src/main.rs`: `StreamResponse`, `build_chat_request()`, `read_streaming_with_http_stream_tls()`, `read_streaming_response_with_progress()`, `chat_once()`
