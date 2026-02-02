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

## Notes

- The `max_tokens` fix is the primary solution for the "forgets to call tools" issue
- The stream processing fix is a safety net for edge cases where connections close unexpectedly
- When the model properly sends its done signal (`"done":true` for Ollama, `data: [DONE]` for OpenAI), we return immediately - the stream-close handling is a fallback
- The 16384 token limit is generous enough for most responses while being within typical model context windows

## Related Files

- `src/main.rs`: `build_chat_request()`, `read_streaming_with_http_stream_tls()`, `read_streaming_response_with_progress()`
