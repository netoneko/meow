# Tool calls are silently dropped against mlx-server

**Status: OPEN.** Found 2026-08-10 while running
`acceptance/08_meow_clone_compile_run.md` against
`mlx-community/Qwen-AgentWorld-35B-A3B-oQ4` served by `mlx-server` on the host.

## Symptom

`meow -c "…"` against an mlx-server provider returns an **empty** response and
exits 0. No tool runs, no error. The debug trace shows the request succeeding:

```
[meow:debug] POST http://10.0.2.2:8080/v1/chat/completions
[meow:debug] response: HTTP/1.0 200 OK
     --- First: 940ms | Stream: 503ms | Size: 0.00KB | TPS: 1.9
```

`Size: 0.00KB` with a fast turnaround is the tell — the model emitted a tool
call (whose message content is empty by definition) and meow discarded it.

The same prompt, same VM, same meow binary against ollama works end to end.

## Root cause

`userspace/meow/src/api/client.rs:757`, in `accumulate_tool_call_delta`:

```rust
let is_finish = json.contains("\"finish_reason\":\"tool_calls\"");
```

That is a literal byte match with **no space after the colon**. It is the only
thing that makes the function return `true`, and its return value is what tells
the stream loop to stop and return `StreamResponse::CompleteWithTools`. If it
never fires, the accumulated tool calls in `pending` are never dispatched — meow
falls out of the loop with empty content and prints nothing.

The two servers serialize the same JSON differently:

| server | wire bytes |
|---|---|
| ollama (`/v1/chat/completions`) | `"finish_reason":"tool_calls"` |
| **mlx-server** | `"finish_reason": "tool_calls"` |

Both are valid JSON for the same document. ollama uses compact separators;
mlx-server uses Python's `json.dumps` defaults, which put a space after every
colon. meow's parser is hand-rolled (`no_std`, no serde), so it is matching
formatting rather than structure.

Confirmed by capturing raw SSE from both:

```
$ grep -o '"finish_reason":[^,]*' mlx_stream.txt | sort -u
"finish_reason": "tool_calls"
"finish_reason": null

$ grep -o '"finish_reason":[^,]*' ollama_stream.txt | sort -u
"finish_reason":"tool_calls"}]}
"finish_reason":null}]}
```

**It is only the finish check.** The tool-call payload itself parses fine —
`extract_json_string` is whitespace-tolerant, and mlx-server's delta carries
every field meow needs (`id`, `index`, `type`, `function.name`,
`function.arguments`). Key *order* differs between the two servers
(ollama: `id, index, type, function`; mlx: `function, type, id, index`) but that
does not matter, since each key is searched for independently.

Proof the model and endpoint are fine — non-streaming, mlx-server:

```json
finish_reason: tool_calls
tool_calls: [{"function": {"name": "shell",
              "arguments": "{\"command\": \"ls -la /tmp\"}"}, ...}]
```

Streaming against the same endpoint also emits `finish_reason: "tool_calls"`.
Only meow's recognition of it fails.

## One caveat about this model

`Qwen-AgentWorld-35B-A3B-oQ4` spends a long empty-content preamble before
emitting the call — measured 214 completion tokens for a trivial `ls /tmp`.
A request capped at `max_tokens: 200` returns `finish_reason: length` with
nothing at all. meow sends `DEFAULT_MAX_TOKENS = 16384`
(`client.rs:34`), so this is not a problem for meow, but it will bite anyone
probing the endpoint by hand with a small cap.

## Fix

Match structure, not formatting. Minimally, allow optional whitespace around the
colon in the finish check — and do the same wherever else meow byte-matches a
JSON key/value pair, since any Python-backed server will have the same spacing.
A small `json_field_is(json, "finish_reason", "tool_calls")` helper that skips
whitespace after the colon would cover it without pulling in a parser.

Worth auditing `client.rs` for other `contains("\"key\":\"value\"")` literals
while doing this; this one was found by accident and others would fail the same
way.

## Reproducing

```bash
# fails: empty output, no tool executed
meow -N -p mlx -m mlx-community/Qwen-AgentWorld-35B-A3B-oQ4 \
     -c "Use the shell tool to run: ls /bin"

# works
meow -N -p ollama -m qwen3:4b -c "Use the shell tool to run: ls /bin"
```

Provider config (`bootstrap/etc/meow/config`) — mlx-server is OpenAI-compatible,
so `type = openai` is correct and not part of the problem:

```
[provider:mlx]
base_url=http://10.0.2.2:8080
type = openai
```

## Background

- `acceptance/08_meow_clone_compile_run.md` — the playbook this blocked. It
  passes with ollama + `qwen3:4b` on a 4.5 MB extreme kernel.
- `userspace/meow/src/api/client.rs` — `accumulate_tool_call_delta` (751),
  `extract_openai_delta_content` (781), `DEFAULT_MAX_TOKENS` (34).
- `userspace/meow/src/tools/shell.rs` — the Shell tool. Note
  `USE_PRETEND_SHELL = true` is the default, so tool commands do **not** go
  through busybox.
