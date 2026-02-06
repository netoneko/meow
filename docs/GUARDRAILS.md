# Meow-chan Guardrails

To ensure reliable tool usage and prevent LLM hallucinations, Meow-chan implements several guardrail mechanisms.

## Intent Guardrail

The Intent Guardrail monitors the model's stated intentions and ensures they are followed by actual tool calls.

- **Detection:** It extracts phrases like "Let me...", "I'll...", "I will..." from the assistant's responses.
- **Enforcement:** If the model states an intention to perform an action but fails to output a tool call JSON in the same response or iteration, the guardrail triggers.
- **Action:** A `[System Notice]` is sent to the model, listing its stated intentions and asking it to complete the actions. The model is then given another iteration to perform the tool calls.

## Tools Guardrail (Fakes detection)

The Tools Guardrail prevents the model from hallucinating tool results instead of calling the actual tools.

- **Detection:** It scans the model's output for the phrase `[Tool Result]`. This phrase is reserved for actual system-provided tool outputs.
- **Enforcement:** If the model outputs `[Tool Result]` itself, it is considered a "fake" tool result (hallucination).
- **Action:** 
    - The hallucinated output is **not added to the conversation history**.
    - A `[System Notice]` is sent to the model informing it that hallucinating tool results is forbidden and that it must use the precise tools.
    - Stated intentions are extracted from the fake response and presented back to the model as a reminder of what it should actually do.
    - The model is reminded of the available tools and given another chance to respond correctly.
- **Statistics:** "Fakes" are tracked and displayed in the final response statistics.

## Statistics Display

At the end of each interaction, Meow-chan displays a summary of the guardrail activity:

- **Intent phrases:** Number of unique intentions detected.
- **Tools called:** Number of actual tool executions performed.
- **Fakes:** Number of times the model attempted to hallucinate a tool result.

If there is an intent/tool mismatch or if any fakes were detected, the status line is highlighted in **Red (Pearl)** to alert the user. Otherwise, it is shown in **Green** for success.

## Tool Failure Feedback

When a tool execution fails (returns `success: false` or a non-zero exit code), Meow-chan provides explicit feedback to the model to encourage self-correction.

- **Explicit Reporting:** The `[Tool Result]` block sent to the model starts with `Tool failed: ...` followed by the error message or command output.
- **Guidance:** The result block includes a system prompt: *"Please analyze the failure and try again with a corrected command or different approach."*
- **Shell Output:** For shell commands, the output now explicitly includes the exit code (e.g., `Exit code: 127`), even if there was no standard output (reported as `(No output)`).

This feedback loop allows the model to understand *why* an action failed and immediately attempt a fix in the next iteration, rather than assuming success or getting stuck.
