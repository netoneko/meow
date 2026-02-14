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
A new module (or addition to `config.rs`) will define the `Personality` struct and a static registry:

```rust
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,
}

pub const PERSONALITIES: &[Personality] = &[
    Personality {
        name: "Meow",
        description: MEOW_PERSONA,
    },
    Personality {
        name: "Jaffar",
        description: JAFAR_PERSONA,
    },
];
```

### C. Configuration Updates
The `Config` struct will be updated to include the current personality name:

```rust
pub struct Config {
    pub current_personality: String, // Defaults to "Meow"
    // ... other fields
}
```

### D. CWD Loading (`MEOW.md`)
At startup, `meow` will check for the existence of `MEOW.md` in the current working directory.
- If it exists, its content will be loaded as a special "Local" personality.
- This "Local" personality will take precedence if no other personality is explicitly requested.

## 4. Default Personalities

### Meow (Default)
The classic cybernetically-enhanced catgirl persona with cyberpunk slang and cat mannerisms.

### Jaffar (Extracted from meow-local)
A cunning and ambitious Grand Vizier persona, accompanied by the sarcastic sidekick Yager. 
*Note: Akuma-specific kernel context will be removed for the general version.*

## 5. Implementation Plan

1. **Refactor Constants**:
    - Extract character descriptions from `SYSTEM_PROMPT_BASE` in `userspace/meow/src/config.rs`.
    - Extract tool descriptions into a `COMMON_TOOLS` constant.
2. **Implement Registry**:
    - Add `Personality` struct and `PERSONALITIES` array to `config.rs`.
    - Port the Jaffar persona from `tools/meow-local/src/main.rs`.
3. **Update Config Logic**:
    - Add `current_personality` to `Config` struct.
    - Update `Config::parse` and `Config::serialize` to handle the new field.
4. **CLI Enhancements**:
    - Add support for `-P` / `--personality <name>` in `main.rs`.
5. **CWD Loading**:
    - Add logic in `main.rs` to detect and read `MEOW.md`.
6. **Prompt Assembly**:
    - Implement a helper to assemble the full system prompt at runtime.
7. **TUI Update (Optional)**:
    - Add a `/personality` command to list or switch personalities at runtime.

## 6. Verification
- Run `meow -P Jaffar` and verify the persona change.
- Create a `MEOW.md` with a custom prompt and verify it loads automatically.
- Verify that tool usage remains functional across different personalities.
