#!/bin/sh
# Container entrypoint for a persistent litter yard (see litter/yard.sh): one
# long-lived container, several *resident* agents (`meow litter live`), no
# dedicated hub process — whoever wins the bind race on 127.0.0.1:7700 IS the
# hub for as long as it lives, and if it dies the next agent to start (or a
# yard_init re-run) takes over.
#
# Per-agent isolation comes from MEOW_HOME (config::scope): each agent gets
# /agents/<name>/etc/meow/config and /agents/<name>/MEOW.md, so four agents
# share one filesystem without trampling a single global config. Every agent
# still runs with CWD=/ so the file-tool sandbox (rooted at CWD) can read
# /akuma-src.
#
# Expects, all mounted by yard.sh start:
#   /bin/meow        -- the linux-net meow build (with the litter feature)
#   /personas/       -- litter/personas/*.md
#   /akuma-src/      -- the akuma repo, read-only (what the agents talk about)
#   AGENTS env var   -- "name:model ..." whitespace-separated, like run_litter.sh

set -e

AGENTS="${AGENTS:-sherlock:qwen3:4b hercules:gemma4-yolo-4b:latest zenigata:gemma4:e4b ressler:qwen3.5:0.8b}"
HUB_ADDR="127.0.0.1:7700"
OLLAMA_URL="http://192.168.65.254:11434"

mkdir -p /litter/tasks

# The operator's identity: yard.sh talk runs `meow litter send` as `root`
# through this scope, without being an agent itself.
mkdir -p /operator/etc/meow
cat > /operator/etc/meow/config <<EOF
litter_agent_name=operator
litter_hub_addr=${HUB_ADDR}

[provider:ollama]
base_url=${OLLAMA_URL}
EOF

# One resident agent per persona, staggered a second apart so the bind race
# has a winner rather than a pileup.
for pair in $AGENTS; do
    name="${pair%%:*}"
    model="${pair#*:}"

    home="/agents/${name}"
    mkdir -p "${home}/etc/meow"
    cp "/personas/${name}.md" "${home}/MEOW.md"

    cat > "${home}/etc/meow/config" <<EOF
litter_agent_name=${name}
litter_hub_addr=${HUB_ADDR}
current_provider=ollama
current_model=${model}

[provider:ollama]
base_url=${OLLAMA_URL}
EOF

    echo "[yard] starting ${name} (${model})"
    (cd / && MEOW_HOME="${home}" /bin/meow litter live >> "${home}/log" 2>&1) &
    sleep 1
done

echo "[yard] yard is up; drop tasks with yard.sh task \"...\", watch with yard.sh watch"

# Stay resident and reap: the agents are our children, and `wait` reaps any
# that die. If an agent dies the yard keeps running — restart it (or let
# whoever re-runs yard_init take the hub) rather than taking the whole yard
# down. This shell is PID 1, so orphaned `meow litter live` processes from
# docker exec also end up here and get reaped instead of zombied.
while :; do
    wait || true
    sleep 1
done
