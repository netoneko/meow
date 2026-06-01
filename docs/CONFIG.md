# meow Configuration

Config file: `/etc/meow/config`

Uses a simple INI-style format — global key=value settings at the top, then `[provider:name]` sections.

## Example

```ini
# Global settings
current_provider=groq
current_model=llama-3.3-70b-versatile
current_personality=Meow
render_markdown=true
exit_on_escape=false

# Local Ollama (QEMU host gateway)
[provider:ollama]
base_url=http://10.0.2.2:11434

# Groq
[provider:groq]
base_url=https://api.groq.com/openai/v1
api_key=gsk_your-groq-key-here

# OpenAI
[provider:openai]
base_url=https://api.openai.com
api_key=sk-your-api-key-here

# Gemini (OpenAI-compatible)
[provider:gemini]
base_url=https://generativelanguage.googleapis.com/v1beta/openai/
api_key=your-gemini-key-here
```

## Global Settings

| Key | Description | Default |
|-----|-------------|---------|
| `current_provider` | Active provider name | `ollama` |
| `current_model` | Model to use | `gemma3:27b` |
| `current_personality` | Active personality (`Meow`, `Jaffar`, `Rosie`) | `Meow` |
| `render_markdown` | Render Markdown in TUI | `true` |
| `exit_on_escape` | Exit app on Escape key | `false` |

## Provider Settings

Each `[provider:name]` section accepts:

| Key | Description | Required |
|-----|-------------|----------|
| `base_url` | HTTP or HTTPS endpoint | Yes |
| `api_key` | Bearer token for authentication | No |

All providers use the OpenAI-compatible `/v1/chat/completions` endpoint. The path is inferred from `base_url`:

- `http://host:11434` → `/v1/chat/completions`
- `https://api.groq.com/openai/v1` → `/openai/v1/chat/completions`
- Any URL ending in `/v1` → appends `/chat/completions`

## HTTPS

TLS 1.3 is supported via libakuma-tls. Certificate verification is not implemented (equivalent to `curl -k`).

## QEMU Networking

The host machine is reachable at `10.0.2.2` from inside the VM:

```ini
[provider:ollama]
base_url=http://10.0.2.2:11434
```

## Custom System Prompt

Place a `MEOW.md` file in the current working directory to override the personality system prompt entirely. Meow loads it at startup if present.

## Runtime Commands

Switch provider/model without restarting:

```
/provider              # Show current provider
/provider list         # List all configured providers
/provider groq         # Switch to groq

/model                 # Show current model
/model list            # List models from current provider
/model gpt-4o          # Switch model

/personality list      # List personalities
/personality Rosie     # Switch personality
```

Changes made with runtime commands are saved back to `/etc/meow/config`.

## Quick Setup

```bash
# From inside the Akuma VM
mkdir -p /etc/meow
cat > /etc/meow/config << 'EOF'
current_provider=groq
current_model=llama-3.3-70b-versatile

[provider:ollama]
base_url=http://10.0.2.2:11434

[provider:groq]
base_url=https://api.groq.com/openai/v1
api_key=gsk_your-groq-key-here
EOF

meow init   # verify
```
