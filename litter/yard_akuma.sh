#!/bin/sh
# yard_akuma.sh -- a litter yard running natively on Akuma/amd64 bare metal.
#
# The Docker-based litter/yard.sh cannot run here (no Docker, no Alpine) and
# does not need to: a yard is only N `meow litter live` processes with separate
# MEOW_HOMEs racing to bind one hub port. Akuma has processes and a filesystem.
#
# The provider is this box's OWN llama-server rather than Ollama elsewhere.
# meow talks /v1/chat/completions (src/api/client.rs:418) and so does
# llama-server, so the litter needs no network beyond loopback.
#
# ONE server, several slots -- not one server per agent. The first attempt
# (2026-09-20) gave each agent its own llama-server: three model loads at once
# on a four-core box, on top of one already running, starved the netpoll thread
# until ICMP stopped answering and sshd could not finish a banner exchange. The
# box had to be restarted by hand. llama-server's own `--parallel` does this
# properly: the queueing happens inside one process instead of as CPU
# contention between four.
#
# Joining another litter (docs/LITTER_RELAY_TOPOLOGY.md):
#
#   LITTER_STATIC_PEERS=ryzen@192.168.1.126:7700 ./yard_akuma.sh
#
# HUB_ADDR stays 127.0.0.1:7700 even when peers relay in, and that is not an
# oversight: Akuma's listener is bound by PORT ONLY (`socket.listen(port)` in
# crates/akuma-net/src/socket.rs -> smoltcp's unspecified-address
# ListenEndpoint), so a hub on 127.0.0.1:7700 already answers on the box's LAN
# address. There is nothing to publish and no 0.0.0.0 to bind -- unlike the
# Docker yard, where HUB_ADDR=0.0.0.0 plus -p 7700:7700 is required. Do NOT set
# HUB_ADDR to the LAN address to "expose" it: the agents connect to that same
# string, loopback diversion is 127.x only (crates/akuma-net-nic/src/loopback.rs
# `is_loopback_frame`), and a connect to our own LAN address goes out the NIC
# and is never answered.
set -e

HUB_ADDR="${HUB_ADDR:-127.0.0.1:7700}"
LITTER_NAME="${LITTER_NAME:-trashcan}"
LITTER_STATIC_PEERS="${LITTER_STATIC_PEERS:-}"
MODEL="${MODEL:-smollm2-135m}"          # the 144 MB one: this is a swarm test, not a quality test
# Which meow to run. Deliberately NOT /bin/meow by default: a new build is
# installed beside the old one, so the binary the box booted with stays intact
# and nothing that is already running has its text rewritten under it
# (docs/LITTER_RELAY_TOPOLOGY.md "Known traps" -- that is a SIGBUS, not a panic).
MEOW_BIN="${MEOW_BIN:-/bin/meow-live}"
[ -x "$MEOW_BIN" ] || MEOW_BIN=/bin/meow
PORT="${PORT:-8081}"
# llama-server threads. ONE by default: this box has four cores and shares them
# with the kernel's netpoll thread, and an over-threaded server starves it --
# the failure mode is not slowness but ssh dying in the banner exchange, which
# on a keyboard-less box costs a walk to the power button.
THREADS="${THREADS:-1}"
AGENTS="${AGENTS:-sherlock zenigata hercules}"
NPAR=$(echo "$AGENTS" | wc -w)

echo "[yard] model=$MODEL port=$PORT threads=$THREADS agents='$AGENTS' parallel=$NPAR"
echo "[yard] litter=$LITTER_NAME hub=$HUB_ADDR peers='${LITTER_STATIC_PEERS:-none}' bin=$MEOW_BIN"

if pidof llama-server >/dev/null 2>&1 || pidof meow >/dev/null 2>&1 || pidof meow-live >/dev/null 2>&1; then
    echo "[yard] stopping what is already running"
    killall llama-server 2>/dev/null || true
    killall meow 2>/dev/null || true
    killall meow-live 2>/dev/null || true
    sleep 3
fi

echo "[yard] starting one llama-server"
(cd /tmp && nohup /bin/llama-server -m "/models/${MODEL}.gguf" \
    --host 127.0.0.1 --port "$PORT" -c 2048 -t "$THREADS" --parallel "$NPAR" --no-mmap \
    > /tmp/llama-yard.log 2>&1 &)

echo "[yard] waiting for it to load"
i=0
while [ $i -lt 150 ]; do
    if wget -q -O- "http://127.0.0.1:${PORT}/health" 2>/dev/null | grep -q ok; then
        echo "[yard] llama-server ready after ${i}s"; break
    fi
    i=$((i+1)); sleep 1
done
if [ $i -ge 150 ]; then
    echo "[yard] llama-server DID NOT COME UP -- not starting agents"
    tail -20 /tmp/llama-yard.log
    exit 1
fi

# An agent's litter_key IS its identity on the relay (docs/LITTER_RELAY_TOPOLOGY.md:
# "pinning this agent means pinning this seed"), and meow only generates one when
# the config has none. Rewriting the config from scratch would hand every agent a
# fresh identity on every restart, so carry the existing seed across.
for name in $AGENTS; do
    home="/agents/${name}"
    mkdir -p "${home}/etc/meow"
    [ -f "/personas/${name}.md" ] && cp "/personas/${name}.md" "${home}/MEOW.md"
    key=$(grep '^litter_key=' "${home}/etc/meow/config" 2>/dev/null || true)
    cat > "${home}/etc/meow/config" <<CFG
litter_agent_name=${name}
litter_hub_addr=${HUB_ADDR}
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}
${key}
current_provider=local
current_model=${MODEL}
render_markdown=false
exit_on_escape=false

[provider:local]
base_url=http://127.0.0.1:${PORT}/v1
type=openai
CFG
    echo "[yard] starting ${name}"
    (cd / && MEOW_HOME="${home}" nohup "$MEOW_BIN" litter live >> "${home}/log" 2>&1 &)
    sleep 3
done

# The operator scope: `meow litter send` as root, without being an agent.
mkdir -p /operator/etc/meow
opkey=$(grep '^litter_key=' /operator/etc/meow/config 2>/dev/null || true)
cat > /operator/etc/meow/config <<CFG
litter_agent_name=operator
litter_hub_addr=${HUB_ADDR}
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}
${opkey}
CFG

sleep 2
echo "[yard] up. agents: $(ls /agents 2>/dev/null | tr '\n' ' ')"
echo "[yard] processes:"; ps 2>/dev/null | grep -c meow
