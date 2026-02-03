## Plan: Adding Thinking Mode Support to Meow

**Goal:** Enable users of `meow` (both `meow-local` and `userspace meow`) to specify a "thinking mode" for Gemini models, allowing for improved reasoning and optionally displaying thought summaries.

**Current State Review:**
*   `MEOW.md` describes two versions: `meow-local` (host, robust tools) and `userspace meow` (guest, constrained, no HTTPS).
*   `meow-local` connects to a locally-hosted Ollama server. `userspace meow` connects to `10.0.2.2:11434` (Ollama on host).
*   `docs/GEMINI_OPENAI.md` shows how to use Gemini models via an OpenAI-compatible API, including parameters for `reasoning_effort` and `thinking_config` (`thinking_level`, `include_thoughts`).
*   Neither `meow` version currently supports specifying these Gemini-specific thinking parameters.
*   `userspace meow` lacks TLS, so direct Gemini API calls (which are HTTPS) are not possible. It must go through Ollama on the host. This implies Ollama on the host must be able to proxy or interact with Gemini's thinking parameters. This is a critical point.

**Assumptions:**
1.  The primary target for "thinking mode" will be Gemini models accessed *via the OpenAI compatibility layer*, likely running on the *host's Ollama instance* that `meow-local` connects to, or through the QEMU networking for `userspace meow`.
2.  Ollama itself would need to support forwarding these `reasoning_effort` or `thinking_config` parameters to the actual Gemini API. If Ollama doesn't natively support this, it becomes a much larger task (modifying Ollama or adding a proxy layer). Meow-chan will assume for now that *if* `meow` wants to leverage this with Ollama, Ollama would need to be updated or configured to pass these parameters. **This is a key question: Does Ollama support forwarding `reasoning_effort` or `thinking_config` to external Gemini APIs?** If not, `meow-local` might need a direct Gemini API integration (requiring a new client, potentially TLS).

**Phased Approach:**

### Phase 1: Investigate Ollama's Gemini Thinking Mode Support (Critical First Step)

1.  **Research Ollama:** Meow-chan needs to find out if Ollama's `openai` compatibility layer supports passing `reasoning_effort` or `extra_body` (for `thinking_config`) to the upstream Gemini API.
    *   **Action:** Search Ollama documentation, GitHub issues, or source code for keywords like "Gemini thinking", "reasoning_effort", "extra_body", "pass-through parameters".
    *   **If Ollama supports it:** Preem! We can proceed with modifying `meow` to send these parameters to Ollama.
    *   **If Ollama *does not* support it:**
        *   **Option A (Harder):** Explore if `meow-local` should have a *direct* Gemini API client (requiring a new Rust HTTP client with TLS support and API key management). This would *only* apply to `meow-local`, as `userspace meow` cannot do HTTPS.
        *   **Option B (Alternative for Userspace):** If direct integration isn't feasible for `userspace meow`, then its thinking mode capabilities would be limited to what the *host's Ollama* can provide, which might mean no thinking mode for `userspace meow` via Gemini, or only if Ollama gets updated.

### Phase 2: Design `meow` Interface for Thinking Mode

1.  **Command-Line Argument/Configuration:**
    *   Add a new command-line flag, e.g., `--thinking-mode <level>` or `--reasoning-effort <level>` (e.g., `minimal`, `low`, `medium`, `high`).
    *   Possibly a `--include-thoughts` flag to request thought summaries.
    *   Consider a configuration file entry for persistent settings.
2.  **In-Chat Command:**
    *   For interactive mode, introduce a new command like `/think <level>` (e.g., `/think high`) or `/think off`).
    *   `/showthoughts` to toggle thought summary display.

### Phase 3: Implement Backend Logic in `meow`

1.  **Parse Arguments/Commands:** Modify `meow`'s argument parser and command handler to recognize the new thinking mode parameters.
2.  **Modify API Request Payload:**
    *   When constructing the chat completion request to Ollama, conditionally add the `reasoning_effort` parameter to the `create` call based on user input.
    *   If `include_thoughts` is requested, add the `extra_body` structure as shown in `docs/GEMINI_OPENAI.md` to the API call.
3.  **Handle Responses (Thought Summaries):**
    *   If `include_thoughts` is enabled, the model might return additional content or structure for thoughts. `meow` will need to parse this and decide how to present it to the user (e.g., prefixing with "Thought Process:" or displaying it separately). This will require understanding the exact format of thought summaries from the Gemini API when routed through Ollama.

### Phase 4: Testing

1.  **Unit Tests:** Add tests for parsing new command-line arguments and in-chat commands.
2.  **Integration Tests:**
    *   Run `meow-local` with various thinking mode settings against an Ollama instance configured to use Gemini.
    *   If Ollama supports it, test `userspace meow` similarly.
    *   Verify that reasoning effort impacts response quality (qualitative).
    *   Verify that thought summaries are correctly requested and displayed.

**Potential Challenges & Questions:**

*   **Ollama Compatibility (Reiterated):** This is the biggest unknown. If Ollama doesn't support forwarding these parameters, the scope changes dramatically.
*   **API Key Management:** If `meow-local` goes direct to Gemini, how will the API key be securely provided? (Environment variable, config file?)
*   **Error Handling:** What happens if an unsupported `reasoning_effort` is used for a specific Gemini model (e.g., `medium` for Gemini 3 Pro)? `meow` should gracefully handle such errors from the API.
*   **Performance/Cost:** Thinking mode might increase latency and token usage. Informing the user about this might be useful.
