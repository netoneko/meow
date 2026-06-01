# meow — LLM chat client

`meow` is an LLM chat client for the Akuma OS guest. It connects to OpenAI-compatible providers (Ollama, Groq, OpenAI, etc.) and supports tool calling for filesystem, shell, git, and network operations.

---

## Usage

```bash
meow                          # Interactive TUI (default)
meow -c "What is 2+2?"       # Non-interactive: print response and exit
meow -m llama3.2              # Override model
meow -p groq                  # Override provider
meow -P Rosie                 # Override personality
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

---

## Features

- **Streaming responses**: token-by-token output as they arrive
- **Tool calling**: LLM can read/write files, run shell commands, query git, fetch URLs
- **Interactive TUI**: 3-pane layout with scrolling output, status bar, and multi-line input
- **Context compaction**: AI can summarize and reset its own context window when memory is low
- **HTTPS support**: TLS 1.3 via libakuma-tls (no certificate verification)
- **Multiple providers**: Ollama, Groq, OpenAI, and any OpenAI-compatible endpoint
- **Personalities**: Meow (default), Jaffar, Rosie

---

## TUI Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/clear` | Clear chat history |
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
| `FileMove` | `source`, `destination` | Move file |
| `FileRename` | `source_filename`, `destination_filename` | Rename file |
| `FolderCreate` | `path` | Create directory |
| `CodeSearch` | `pattern`, `path` | Grep source files recursively |

### Shell & Navigation

| Tool | Key args | Description |
|------|----------|-------------|
| `Shell` | `cmd` | Execute shell command |
| `Cd` | `path` | Change working directory |
| `Pwd` | — | Print working directory |

### Git

| Tool | Key args | Description |
|------|----------|-------------|
| `GitStatus` | — | Show git status |
| `GitLog` | `count`, `oneline` | Show commit history |
| `GitAdd` | `path` | Stage files |
| `GitCommit` | `message` | Create commit |
| `GitCheckout` | `branch` | Switch branch |
| `GitBranch` | `name`, `delete` | List/create/delete branches |
| `GitPull` | — | Pull from remote |
| `GitPush` | — | Push to remote |
| `GitClone` | `url` | Clone repository |

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
