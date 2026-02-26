# Meow - LLM Chat Client

Meow is an LLM chat client that runs inside the Akuma kernel guest. It connects to a locally-hosted Ollama server and supports tool calling for filesystem operations and streaming responses.

---

## Usage

```bash
meow                                    # Interactive mode with default model
meow -m gemma3:27b                      # Use specific model
meow -P Rosie                           # Use a different personality
meow -p ollama -m llama3.2              # Override provider and model
meow "What is 2+2?"                     # One-shot mode
meow -h                                 # Show help
```

## Features

- **Multiple Personalities**: Switch between Meow, Jaffar, Rosie and custom MEOW.md personalities
- **Streaming responses**: Displays LLM output token-by-token as it arrives
- **Tool calling**: LLM can execute filesystem and network operations
- **Progress indication**: Shows dots while waiting, elapsed time on first token
- **Network retry**: Automatic retry with exponential backoff on network errors
- **Memory limits**: Caps response size (16KB) and chat history (10 messages)

## Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/clear` | Clear chat history |
| `/model [NAME]` | Switch/check LLM model |
| `/model list` | List available models |
| `/provider [NAME]` | Switch/check provider |
| `/provider list` | List configured providers |
| `/personality [NAME]` | Switch/check personality |
| `/personality list` | List available personalities |
| `/tokens` | Show current token usage |
| `/markdown` | Toggle markdown rendering |
| `/exit` or `/quit` | Exit the chat |
| Ctrl+D | Exit on empty line |

## Personalities

Meow supports multiple AI personas. Available personalities:

- **Meow** (default) - Cyberpunk anime cat assistant
- **Jaffar** - Technical, analytical guide
- **Rosie** - Friendly, creative companion
- **Local MEOW.md** - Custom personality loaded from current working directory (takes precedence)

**Switch personality:**
```bash
meow -P Jaffar                    # Use Jaffar personality
/personality Rosie                # Switch in interactive mode
```

**Custom personality:**
Create a `MEOW.md` file in your working directory with a custom system prompt. It will automatically load and take precedence over built-in personalities.

## Available Tools

The LLM can invoke these tools via JSON commands:

### Filesystem Tools

| Tool | Args | Description |
|------|------|-------------|
| `FileRead` | `filename` | Read file contents (max 32KB) |
| `FileWrite` | `filename`, `content` | Write/create file |
| `FileAppend` | `filename`, `content` | Append to file |
| `FileExists` | `filename` | Check if file exists |
| `FileList` | `path` | List directory contents |
| `FileDelete` | `filename` | Delete a file |
| `FolderCreate` | `path` | Create directory |
| `FileCopy` | `source`, `destination` | Copy file |
| `FileMove` | `source`, `destination` | Move file |
| `FileRename` | `source_filename`, `destination_filename` | Rename file |

### Network Tools

| Tool | Args | Description |
|------|------|-------------|
| `HttpFetch` | `url` | HTTP GET request (max 16KB response) |

**Note**: `HttpFetch` only supports HTTP, not HTTPS. Userspace has no TLS stack.

## Network Architecture

```
┌─────────────────┐         ┌─────────────────┐
│  Akuma Guest    │         │  Host Machine   │
│                 │         │                 │
│  meow binary    │◄───────►│  Ollama API     │
│  (port 11434)   │  HTTP   │  localhost:11434│
│                 │         │                 │
│  10.0.2.15      │         │  10.0.2.2       │
└─────────────────┘         └─────────────────┘
     QEMU User-Mode Networking
```

- Guest connects to `10.0.2.2:11434` (QEMU's host gateway)
- Ollama must be running on the host machine
- Uses Ollama's `/api/chat` endpoint with `stream: true`

## SSL/TLS Status

| Component | TLS Support | Certificate Verification |
|-----------|-------------|-------------------------|
| Kernel `curl` | Yes (TLS 1.3) | Yes (use `-k` to skip) |
| Userspace `wget` | No | N/A |
| Userspace `meow` | No | N/A |

Userspace programs use libakuma's plain TCP sockets. TLS requires the kernel's async networking stack.

## Memory Constraints

To avoid OOM in the constrained environment:

- **Response buffer**: 16KB max per response
- **File operations**: 32KB max file size
- **Chat history**: 10 messages max (older trimmed)
- **Read buffer**: 1KB chunks

## Error Handling

- **Connection refused**: Ollama not running on host
- **Model not found (404)**: Requested model not available
- **Timeout**: Network latency, retry with backoff
- **Network errors**: Automatic retry (3 attempts, exponential backoff)

## Building

```bash
cd userspace
./build.sh  # Builds meow and copies to bootstrap/bin/
```

## Default Configuration

- **Model**: `deepseek-r1:32b`
- **Ollama endpoint**: `10.0.2.2:11434`
- **Persona**: Cyberpunk anime cat assistant
