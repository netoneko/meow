# API and Tools Alignment Proposal: Meow to OpenAPI/Gemini

This document proposes a strategy to align Akuma's `meow` userspace application with industry-standard tool calling conventions used by OpenAI and Google Gemini.

## 1. Current State Assessment

Currently, `meow` implements tool calling through a "manual" loop:
- **Definitions:** Tools are defined loosely in `src/toolset/definition.json` (incomplete) and hardcoded in `src/tools/mod.rs` for dispatch.
- **Protocol:** Tools are described to the model via the system prompt.
- **Execution:** The model outputs a JSON block in its text response. `meow` parses this text using primitive string matching in `helpers.rs`.
- **Feedback:** Tool results are fed back to the model as "user" messages with a `[Tool Result]` prefix.

### Limitations:
- **Brittle Parsing:** Simple string matching fails on complex JSON or nested structures.
- **Model Hallucinations:** The model often "fakes" tool results because it perceives the conversation as a simple text exchange.
- **Incompatibility:** Cannot easily switch to native provider features (like Gemini's "Google Search" tool or OpenAI's "File Search").

## 2. Standards Overview

### OpenAI Tool Format (Function Calling)
OpenAI uses a `tools` array in the request, where each tool is defined using JSON Schema.
```json
{
  "type": "function",
  "function": {
    "name": "file_read",
    "description": "Read contents of a file",
    "parameters": {
      "type": "object",
      "properties": {
        "filename": { "type": "string" }
      },
      "required": ["filename"]
    }
  }
}
```

### Gemini Tool Format
Gemini uses a nearly identical JSON Schema format, often referred to as "Function Declarations". It also supports built-in tools like `google_search`.

### OpenAPI Chat Protocol
While "OpenAPI Chat Protocol" often refers to the OpenAI API structure, it implies using OpenAPI/Swagger definitions to dynamically generate toolsets.

## 3. Tool Comparison & Mapping

| Meow Tool | Proposed OpenAPI Name | Parameters (JSON Schema) |
|-----------|-----------------------|--------------------------|
| `FileRead` | `file_read` | `filename: string` |
| `FileWrite` | `file_write` | `filename: string, content: string` |
| `FileEdit` | `file_edit` | `filename: string, old_text: string, new_text: string` |
| `CodeSearch` | `code_search` | `pattern: string, path: string, context: integer` |
| `GitStatus` | `git_status` | `(none)` |
| `HttpFetch` | `http_fetch` | `url: string` |
| `Shell` | `shell` | `cmd: string` |

### 3.1 Standard Tool Categories
In the OpenAPI/Gemini ecosystem, the following categories are considered standard for highly capable agents:
- **Filesystem (FS):** Operations like `read`, `write`, `list`, `search`.
- **Version Control (Git):** `status`, `commit`, `push`, `pull`.
- **Network (Net):** `http_fetch`, `dns_resolve`.
- **System (Shell):** `execute_command`.
- **Context Management:** `compact_history`, `search_memory`.

`meow` already implements most of these, but they need to be renamed to follow `snake_case` conventions standard in OpenAPI definitions (e.g., `FileRead` -> `file_read`).

### 3.2 Deprecated Tools
The following tools and their associated packages are to be removed to streamline the userspace environment:
- **Chainlink Tools:** `ChainlinkInit`, `ChainlinkCreate`, `ChainlinkList`, `ChainlinkShow`, `ChainlinkClose`, `ChainlinkReopen`, `ChainlinkComment`, `ChainlinkLabel`.
- **Package:** `userspace/chainlink` (to be deleted).

*(Detailed mapping for remaining tools should follow in the implementation phase)*

## 4. Gemini Context Caching (Future Enhancement)
To further optimize performance and reduce API costs, we plan to optionally support **Gemini Context Caching** later down the line.

- **Purpose:** Store large, static parts of the context (like the full toolset definition, system instructions, or project documentation) in Gemini's infrastructure.
- **Benefits:**
    - **Latency:** Faster "Time to First Token" (TTFT) as the model doesn't re-process the entire toolset on every turn.
    - **Cost:** Reduced input token charges for repeated context.
- **Implementation Note:** This will require adding cache management logic (Create, TTL Update, Delete) to `src/api/client.rs`.

## 5. Proposed Migration Plan

### Step 1: Cleanup and Standardized Definitions
- Delete `userspace/chainlink` and remove all `Chainlink` tool references from `src/tools/mod.rs`.
- Create `userspace/meow/src/toolset/openapi.json` containing the remaining toolset in OpenAPI/JSON Schema format.

### Step 2: Protocol Refactoring
Update `src/api/client.rs` and `src/api/types.rs` to support:
- `tools` field in the request.
- `tool_choice` configuration.
- Parsing `tool_calls` from the response (instead of just `content`).

### Step 3: Message Role Alignment
Introduce a `tool` role for messages in `src/app/history.rs`.
- OpenAI: `{"role": "tool", "tool_call_id": "...", "content": "..."}`
- Gemini: Part of `functionResponse` in the `parts` array.

### Step 4: Dispatcher Update
Update `src/tools/mod.rs` to use a more robust JSON parser and dispatch based on the standardized names.

## 6. Benefits
- **Reduced Hallucinations:** Models are trained to use the native `tool_calls` mechanism and are less likely to fake results.
- **Efficiency:** No need to include tool descriptions in the system prompt (saves tokens).
- **Extensibility:** Easily add new tools or integrate with 3rd party API definitions.

## 7. Implementation Schedule
1. **Week 1:** Cleanup (Chainlink removal) and finalize JSON schemas for core tools.
2. **Week 2:** Implement `tool` role and native request/response handling.
3. **Week 3:** Transition dispatcher and remove manual prompt instructions.
4. **Future:** Implement optional Gemini Context Caching.
