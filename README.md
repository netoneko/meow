# meow — LLM chat client

`meow` is an LLM chat client for the Akuma OS guest. It connects to OpenAI-compatible providers (Ollama, Groq, OpenAI, etc.) and supports tool calling for filesystem, shell, git, and network operations.

---

## Usage

```bash
meow                          # Interactive TUI (default)
meow -c "What is 2+2?"       # Non-interactive: print response and exit
meow -m llama3.2              # Override model
meow -p groq                  # Override provider
meow -P Meow                  # Override personality (default: Meow)
meow -N                       # Disable persona; use a neutral assistant prompt
meow init                     # Show/initialize configuration
meow test                     # Run built-in tests
meow -h                       # Show help
```

### Non-interactive mode (`-c`)

Non-interactive mode prints streaming output directly to stdout with ANSI color codes but without cursor repositioning or the 3-pane TUI layout. Suitable for scripting, pipes, and low-memory environments.

```bash
meow -c "list files in the current directory"
meow --no-tui -m gemma3:4b -c "summarize this file"
```

The **first line** of non-interactive output (both `-c` and CGI mode) is
`session: <id>`, identifying the on-disk session log for later debugging (see
[Sessions](#sessions)).

---

## Features

- **Streaming responses**: token-by-token output as they arrive
- **Tool calling**: LLM can read/write files, run shell commands, query git, fetch URLs
- **Interactive TUI**: 3-pane layout with scrolling output, status bar, and multi-line input
- **Context compaction**: AI can summarize and reset its own context window when memory is low
- **HTTPS support**: TLS 1.3 via libakuma-tls (no certificate verification)
- **Multiple providers**: Ollama, Groq, OpenAI, and any OpenAI-compatible endpoint
- **Personality**: Meow (default); `--no-personality` / `-N` switches to a neutral assistant prompt
- **Compact tool schema**: the `compact-tools` build feature (on by default) ships the tools schema without per-tool descriptions, saving ~930 tokens on every request

---

## TUI Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/clear` | Clear chat history |
| `/session` | Describe the current session (id, log path, message/token counts) |
| `/new` | Start a fresh session |
| `/model [NAME]` | Show or switch model |
| `/model list` | List models from current provider |
| `/provider [NAME]` | Show or switch provider |
| `/provider list` | List configured providers |
| `/personality [NAME]` | Show or switch personality |
| `/tokens` | Show token usage vs limit |
| `/markdown` | Toggle Markdown rendering |
| `/hotkeys` | Show keyboard shortcuts |
| `/test` | Run built-in tests |
| `/quit` | Exit |

---

## Sessions

Each conversation is a **session**, backed by its own directory under
`/tmp/meow/<id>/` (sandbox-prefixed when running sandboxed). The conversation
log lives at `/tmp/meow/<id>/conversation.jsonl` — one JSON message object per
line, which is also the source streamed straight into each API request.

The session id is derived from the wall-clock time and pid (`<hex-seconds>-<hex-pid>`),
so it's unique across concurrent invocations and easy to correlate with logs.
When the RTC is unavailable the timestamp falls back to monotonic uptime.

- **Interactive (TUI):** the active session id is shown in the startup banner.
  Use `/session` to print its id, log path, and message/token counts, and
  `/new` to start a fresh session (allocates a new id + directory and reseeds).
- **Non-interactive (`-c` / CGI):** the session id is printed as the first line
  of output (`session: <id>`) so you can find the corresponding log afterward.

---

## Available Tools

The LLM can invoke these tools autonomously:

### Filesystem

| Tool | Key args | Description |
|------|----------|-------------|
| `FileRead` | `filename` | Read file (max 32KB) |
| `FileWrite` | `filename`, `content` | Write/create file |
| `FileAppend` | `filename`, `content` | Append to file |
| `FileEdit` | `filename`, `old_text`, `new_text` | Search-and-replace edit |
| `FileReadLines` | `filename`, `start`, `end` | Read line range |
| `FileExists` | `filename` | Check existence |
| `FileList` | `path` | List directory |
| `FileDelete` | `filename` | Delete file |
| `FileCopy` | `source`, `destination` | Copy file |
| `FileMove` | `source`, `destination` | Move or rename file |
| `FolderCreate` | `path` | Create directory |
| `CodeSearch` | `pattern`, `path` | Grep source files recursively |

### Shell & Navigation

| Tool | Key args | Description |
|------|----------|-------------|
| `Shell` | `cmd` | Execute shell command (also how git is run, e.g. `git status`, `git commit -m "msg"`) |
| `Cd` | `path` | Change working directory |
| `Pwd` | — | Print working directory |

> **Git** has no dedicated tool. The former 13 per-subcommand git tools — and the single `Git` tool that briefly replaced them — were all just `git <subcommand>` shell-outs, so they were dropped entirely. The model runs git through `Shell`, e.g. `Shell { "cmd": "git commit -m 'msg'" }`.

### Network

| Tool | Key args | Description |
|------|----------|-------------|
| `HttpFetch` | `url` | HTTP/HTTPS GET request |

### Context Management

| Tool | Key args | Description |
|------|----------|-------------|
| `CompactContext` | `summary` | Replace history with a summary to free memory |

---

## Configuration

Config file: `/etc/meow/config`

Run `meow init` to view current settings. See [docs/CONFIG.md](docs/CONFIG.md) for full details.

---

## Building

```bash
cd userspace
./build.sh --meow-only
```

`build.sh` builds meow size-optimized: it rebuilds `core`/`alloc` from source
(`-Z build-std`, nightly) with the `immediate-abort` panic strategy, shaving a
page off the binary. Panics therefore trap immediately rather than printing via
the panic handler.

Relevant cargo features (see `Cargo.toml`):

| Feature | Default | Effect |
|---------|---------|--------|
| `compact-tools` | **on** | Ship the tools schema without per-tool descriptions (~930 fewer tokens/request). `--no-default-features` restores the descriptive schema. |
| `tests` | off | Compile the in-binary self-test suite (`meow test`, `/test`). Omitted from shipped builds. |
| `size` | off | Cap in-memory tool output at 2KB (vs 32KB). |

---

## Memory Constraints

Designed to run with limited RAM:

- Chat history: 10 messages max (older trimmed automatically)
- Tool output: capped at 32KB (overflow saved to `/tmp`, summary returned)
- Context compaction: AI can invoke `CompactContext` to reset its history

---

## Network Architecture

```
┌─────────────────┐         ┌─────────────────┐
│  Akuma Guest    │         │  Host Machine   │
│  meow binary    │◄───────►│  Ollama / API   │
│  10.0.2.15      │  HTTP/S │  10.0.2.2       │
└─────────────────┘         └─────────────────┘
     QEMU User-Mode Networking
```

- QEMU host gateway is `10.0.2.2`
- Ollama default: `http://10.0.2.2:11434`
- HTTPS providers (Groq, OpenAI) connect directly over TLS
