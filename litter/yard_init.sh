#!/bin/sh
# Container entrypoint for a persistent litter yard (see litter/yard.sh): one
# long-lived container, several *resident* agents (`meow litter live`), no
# dedicated hub process — whoever wins the bind race on 127.0.0.1:7700 runs
# the raft thread for as long as it lives, and if it dies the next agent's
# pulse re-races the bind.
#
# Per-agent isolation comes from MEOW_HOME (config::scope): each agent gets
# /agents/<name>/etc/meow/config and /agents/<name>/MEOW.md, so N agents
# share one filesystem without trampling a single global config. Every agent
# still runs with CWD=/ so the file-tool sandbox (rooted at CWD) can read
# /akuma-src.
#
# Expects, all mounted by yard.sh start:
#   /bin/meow        -- the linux-net meow build (with the litter feature)
#   /personas/       -- litter/personas/*.md
#   /akuma-src/      -- the akuma repo, read-only (what the agents talk about)
#   AGENTS env var   -- "name:model ..." whitespace-separated

AGENTS="${AGENTS:-sherlock:qwen3:4b hercules:gemma4-yolo-4b:latest zenigata:gemma4:e4b ressler:qwen3.5:0.8b}"
# One address serves both jobs: agents CONNECT to it, and the bind-race winner
# LISTENS on it. 127.0.0.1 keeps the litter inside this container, which is the
# right default. Set HUB_ADDR=0.0.0.0:7700 (with yard.sh publishing the port) to
# let a peer litter's relay reach in — on Linux, connect() to 0.0.0.0 lands on
# loopback, so the agents' own calls keep working unchanged.
HUB_ADDR="${HUB_ADDR:-127.0.0.1:7700}"
OLLAMA_URL="${OLLAMA_URL:-http://192.168.65.254:11434}"
# Relay plane (docs/LITTER_RELAY_TOPOLOGY.md): our litter's identity plus
# peer litters (`name@host:port,...`) — local hub always, peers via raft.
LITTER_NAME="${LITTER_NAME:-yard}"
LITTER_STATIC_PEERS="${LITTER_STATIC_PEERS:-}"

# The operator's identity: yard.sh talk/task run `meow litter send` as `root`
# through this scope, without being an agent themselves. root outranks
# debate (it's the root role on the wire).
mkdir -p /operator/etc/meow
cat > /operator/etc/meow/config <<EOF
litter_agent_name=operator
litter_hub_addr=${HUB_ADDR}
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}

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
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}
current_provider=ollama
current_model=${model}

[provider:ollama]
base_url=${OLLAMA_URL}
EOF

    echo "[yard] starting ${name} (${model})"
    (cd / && MEOW_HOME="${home}" /bin/meow litter live >> "${home}/log" 2>&1) &
    sleep 1
done

echo "[yard] yard is up: talk with yard.sh talk <agent|litter> \"...\", watch with yard.sh watch"

# Stay resident and reap: `wait` reaps any agent that dies, and as PID 1
# this shell also reaps orphans from `docker exec`. If an agent dies, the
# yard itself keeps running (restart it with yard.sh respawn <name>).
while :; do
    wait || true
    sleep 1
done
