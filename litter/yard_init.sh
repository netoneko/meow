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
# One inference server PER AGENT, rather than one shared endpoint.
#
# With a single server the four agents queue behind each other: whichever
# model is loaded serves one request at a time, so a four-agent round costs
# four sequential turns and any model swap costs a reload. Measured
# 2026-09-21 against one Ollama, turns ran 9-12 minutes each. Give every
# agent its own `llama-server` and they think concurrently.
#
# Set LITTER_BASE_PORT and agent *i* (in AGENTS order) gets
# `http://$LLM_HOST:$((BASE+i))`, with its own name as the model alias —
# which is what `llama-server --alias <name>` answers to.
LITTER_BASE_PORT="${LITTER_BASE_PORT:-}"
LLM_HOST="${LLM_HOST:-192.168.65.254}"
# Relay plane (docs/LITTER_RELAY_TOPOLOGY.md): our litter's identity plus
# peer litters (`name@host:port,...`) — local hub always, peers via raft.
LITTER_NAME="${LITTER_NAME:-yard}"
LITTER_STATIC_PEERS="${LITTER_STATIC_PEERS:-}"

# Explicitly start from nothing.
#
# Today this is belt-and-braces — `yard.sh start` does `docker rm -f` first,
# so /agents lives in a writable layer that was just discarded. But that is
# an *incidental* guarantee: the moment anyone mounts a volume at /agents to
# keep logs across runs, stale sessions and logs would silently bleed from
# one experiment into the next, and a measurement taken across two runs is
# worse than no measurement (it is how "the leader took 39 minutes" got
# compared against another run's server log).
rm -rf /agents /operator
mkdir -p /agents

# The operator's identity: yard.sh talk/task run `meow litter send` as `root`
# through this scope, without being an agent themselves. root outranks
# debate (it's the root role on the wire).
# The operator joins as `root`, not as a separate "operator" name: root is
# the identity the hub grants operator authority to, and the task table
# refuses to assign work to it (no agent loop stands behind it). Under the
# old name it joined the roster as an ordinary member and a leader promptly
# handed it a sub-task nobody could ever claim.
mkdir -p /operator/etc/meow
cat > /operator/etc/meow/config <<EOF
litter_agent_name=root
litter_hub_addr=${HUB_ADDR}
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}

[provider:ollama]
base_url=${OLLAMA_URL}
EOF

# One resident agent per persona, staggered a second apart so the bind race
# has a winner rather than a pileup.
port_index=0
for pair in $AGENTS; do
    name="${pair%%:*}"
    model="${pair#*:}"

    # An entry may pin its own endpoint as `name:model@url`, which wins
    # over everything else. That is how you run a mixed litter — say the
    # leader on a big model via Ollama while the workers sit on their own
    # llama-server instances — so the same task can be compared across
    # models in one run.
    case "$pair" in
        *@*)
            agent_url="${pair##*@}"
            rest="${pair%@*}"
            name="${rest%%:*}"
            model="${rest#*:}"
            ;;
        *)
            agent_url=""
            ;;
    esac

    # Per-agent server, when one was asked for.
    if [ -n "$agent_url" ]; then
        :   # explicit; leave it alone
    elif [ -n "$LITTER_BASE_PORT" ]; then
        agent_url="http://${LLM_HOST}:$((LITTER_BASE_PORT + port_index))"
        model="$name"   # llama-server answers to its --alias
    else
        agent_url="$OLLAMA_URL"
    fi
    port_index=$((port_index + 1))

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
base_url=${agent_url}
EOF

    echo "[yard] starting ${name} (${model} @ ${agent_url})"
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
