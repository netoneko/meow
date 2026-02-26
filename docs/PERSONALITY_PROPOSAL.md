# Proposal: Multiple Model Personalities for Meow

## 1. Overview
This document outlines the plan to support multiple AI personalities in the `meow` assistant. The goal is to allow users to switch between different personas (e.g., Meow-chan, Jaffar) and load custom personalities from the current working directory.

## 2. Goals
- **Multiple Personalities**: Support a registry of built-in personas.
- **Dynamic Loading**: Support loading a custom persona from a `MEOW.md` file in the current working directory.
- **Configurable**: Allow setting the default personality in `/etc/meow/config`.
- **CLI Control**: Support selecting a personality via CLI arguments (`-P` / `--personality`).
- **Clean Architecture**: Separate character persona descriptions from common tool definitions and system context.

## 3. Proposed Architecture

### A. Persona vs. Tools
To keep the codebase dry, the system prompt will be constructed at runtime by joining three components:
1. **Persona**: The character description (e.g., Meow-chan's catgirl persona or Jaffar's vizier persona).
2. **Tools**: The description of available JSON commands (FileRead, Shell, etc.).
3. **Context**: Dynamic information like the current working directory and sandbox status.

### B. Personality Registry
The existing `config.rs` now defines a comprehensive `Personality` struct and a static registry containing all built‑in personas. Each entry also carries acknowledgement strings for the TUI/one‑shot modes and an error format string used when reporting failures.

```rust
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,

    pub ack_tui: &'static str,
    pub ack_one_shot: &'static str,
    pub error_format: &'static str, // use "{}" placeholder
}

pub const PERSONALITIES: &[Personality] = &[
    Personality {
        name: "Meow",
        description: MEOW_PERSONA,
        ack_tui: "Understood nya~! I'll use relative paths for file operations within the current directory. Ready to help! (=^・ω・^=)",
        ack_one_shot: "Understood nya~!",
        error_format: "～ Nyaa~! {} (=ＴェＴ=) ～\n",
    },
    Personality {
        name: "Jaffar",
        description: JAFFAR_PERSONA,
        ack_tui: "Understood. I shall utilize relative paths for my machinations within this directory. The throne awaits!",
        ack_one_shot: "Understood.",
        error_format: "Error: {}\n",
    },
    // additional personas such as Rosie are also defined
];
```

### C. Configuration Updates
The `Config` struct already includes a `current_personality` field and the parsing/serialization routines handle it. The default is "Meow" when the config file is absent. The `run_init` helper prints the current personality along with provider/model information.

```rust
pub struct Config {
    pub current_provider: String,
    pub current_model: String,
    pub current_personality: String,
    // ... other fields
}
```

### D. CWD Loading (`MEOW.md`)
The `main.rs` binary now contains a `load_local_prompt()` helper that attempts to open `MEOW.md` in the current directory. If the file exists and is a reasonable size (<64 KB) its contents are returned and prepended to the system prompt. This local prompt overrides whatever personality is configured or provided via CLI.

The helper is also used when assembling the system prompt for both TUI and one‑shot invocations.

## 4. Default Personalities

### Meow (Default)
The classic cybernetically-enhanced catgirl persona with cyberpunk slang and cat mannerisms.

### Jaffar (Extracted from meow-local)
A cunning and ambitious Grand Vizier persona, accompanied by the sarcastic sidekick Yager. 
*Note: Akuma-specific kernel context will be removed for the general version.*

### 5. Implementation Summary
All of the planned changes have been implemented and are shipping in the current codebase:

1. **Constants refactored**
    - Character descriptions live in `config.rs` as the various `*_PERSONA` constants.
    - `COMMON_TOOLS` holds the shared tool documentation appended to every system prompt.
2. **Registry complete**
    - `Personality` struct is defined with additional metadata (acknowledgements and error format).
    - Built‑in personalities include `Meow`, `Jaffar`, and `Rosie` (with room for more).
3. **Config logic updated**
    - `Config::parse`/`serialize` now read and write `current_personality`.
    - Defaults to `Meow` and `run_init` prints the current personality.
4. **CLI enhancements**
    - `main.rs` processes `-P`/`--personality` to override the configuration.
5. **CWD loading**
    - `load_local_prompt()` reads `MEOW.md` and the result takes precedence when assembling the prompt.
6. **Prompt assembly**
    - `main.rs` builds the final `system_prompt` by selecting the active personality or local prompt, then appending `COMMON_TOOLS` and any Chainlink tools.
    - Helper `get_active_personality()` returns the personality struct for use in TUI acknowledgement and error formatting.
7. **Runtime support**
    - The TUI already has `/personality` command to query or change the persona (unchanged by this proposal but tied into the new config field).

With these changes in place the assistant behaves according to the original goals.
## 6. Verification
- Run `meow -P Jaffar` and verify the persona change.
- Create a `MEOW.md` with a custom prompt and verify it loads automatically.
- Verify that tool usage remains functional across different personalities.