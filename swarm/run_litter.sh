#!/bin/sh
# Runs entirely inside one Alpine/arm64 container: starts litter-hub on
# 127.0.0.1 (no Docker-host IP needed since everything shares one network
# namespace), then runs one `meow litter chase` round per persona in
# swarm/personas/, each pointed at a distinct Ollama model on the host
# (192.168.65.254 -- the one thing that genuinely lives outside this
# container -- see docs/LINUX_NET_BUGS.md #5 for why not host.docker.internal).
#
# CWD is `/`, which makes meow's file-tool sandbox root `/` too
# (tools::context::is_within_sandbox: "/" matches everything) -- the
# simplest way to let every agent read /akuma-src (mounted read-only) and
# still write its own /MEOW.md and /etc/meow/config. Not a real sandbox;
# fine for a local demo against local models.
#
# Expects, all mounted by the caller (see the one-liner in
# docs/LITTER_EXPERIMENT.md "Running it"):
#   /bin/meow         -- the linux-net meow build
#   /bin/litter-hub   -- litter-hub cross-compiled for aarch64-unknown-linux-musl
#   /personas/        -- swarm/personas/*.md
#   /akuma-src/       -- the akuma repo, read-only

set -e
cd /

HUB_PORT=7700
HUB_ADDR="127.0.0.1:${HUB_PORT}"
OLLAMA_URL="http://192.168.65.254:11434"

/bin/litter-hub --port "$HUB_PORT" &
HUB_PID=$!
trap 'kill $HUB_PID 2>/dev/null' EXIT
sleep 0.5

mkdir -p /etc/meow

# name:model, one Ollama model per persona so the debate is model-vs-model
# (see docs/LITTER_EXPERIMENT.md "Running it") rather than weights-vs-themselves.
AGENTS="sherlock:qwen3:4b hercules:gemma4-yolo-4b:latest zenigata:gemma4:e4b ressler:qwen3.5:0.8b"

round() {
    name="$1"
    model="$2"
    prompt="$3"

    cp "/personas/${name}.md" /MEOW.md

    cat > /etc/meow/config <<EOF
litter_agent_name=${name}
litter_hub_addr=${HUB_ADDR}
current_provider=ollama
current_model=${model}

[provider:ollama]
base_url=${OLLAMA_URL}
EOF

    echo "=== ${name} (${model}) ==="
    /bin/meow litter chase -N --no-tui -c "$prompt" || echo "  [!] ${name}'s round failed, continuing"
}

TASK="You are joining a litter of agents debating the Akuma bare-metal OS. Read one file under /akuma-src relevant to your persona's concerns, form an opinion, and share it with the litter."

# Run sequentially, never in parallel -- OLLAMA_MAX_LOADED_MODELS=2 on the
# host means concurrent distinct models thrash into swap (see
# docs/LITTER_EXPERIMENT_PHASE_0.md "Environment as of this snapshot").
for pair in $AGENTS; do
    name="${pair%%:*}"
    model="${pair#*:}"
    round "$name" "$model" "$TASK"
done

echo
echo "=== Transcript ==="
/bin/meow litter observe
