# Testing

## Built-in unit tests

Run from the command line:

```bash
meow test
```

Or from within the TUI:

```
/test
```

This runs test suites for:
- **history**: token estimation, trim logic, JSON serialization
- **config**: config file parsing (providers, keys, booleans)
- **chat**: JSON extraction helpers, escape handling, tool call serialization
- **stream**: streaming renderer state machine (requires `ENABLE_TESTS=true` in `config.rs`)

The stream renderer tests are disabled by default to save memory. Enable them by setting `ENABLE_TESTS = true` in `src/config.rs` before building.

## Non-interactive prompt test

```bash
meow -c "read the file prompts/000.txt and execute the instructions"
meow -m qwen3:8b -c "read prompts/001.txt and follow the instructions"
```

Non-interactive mode (`-c`) sends a single message, runs any tool calls autonomously, and exits. Output goes to stdout with color; no TUI repainting.

## Integration testing

```bash
# Clone the repo inside the VM
apk add git
git clone https://github.com/netoneko/meow.git
cd /meow
git checkout -b experimental

# Run a prompt against the cloned repo
meow -c "read document in prompts/000.txt and execute the instructions"
```

## Prompt files

`prompts/` contains test prompts. The AI reads these and executes the instructions, then you verify the output matches expectations.

## Memory stress

To test behavior under low memory, set `MEMORY=256` when launching QEMU:

```bash
MEMORY=256 cargo run --release
```

Meow is designed to stay functional at 256MB; context compaction keeps history small enough to fit.
