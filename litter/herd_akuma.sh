#!/bin/sh
# herd_akuma.sh -- stand a litter up on Akuma/amd64 as *herd services*.
#
# Supersedes the `nohup ... &` shape in litter/yard_akuma.sh, for one reason:
# **nothing on this target reaps an orphan.** An agent started from an ssh
# session outlives the session shell, its parent then dies, and no one ever
# calls wait4() on it -- so it stays a zombie for the life of the boot. `kill -9`
# on one returns 0 and changes nothing, because a zombie has no thread to signal;
# it reads exactly like an unkillable process and is not one
# (docs/archive/AKUMA_AMD64_NO_SLOT_RECYCLER.md, appendix 2026-09-19).
# Measured 2026-09-20: three agents and a duplicate llama-server that neither
# `kill` nor `kill -9` could remove, and a restart that could only be done by
# rebooting the box.
#
# herd is pid 1. It owns its children, reaps them, restarts them on the
# `restart_delay_ms` it is given, captures their stdout to
# /var/log/herd/<svc>.log, and rescans /etc/herd/enabled every 20 seconds -- so
# `herd enable <svc>` starts a service without restarting herd.
#
# Load discipline is the other half. This box has four cores and the kernel's
# netpoll thread shares them; over-subscribe them and the failure is not
# slowness, it is `Connection timed out during banner exchange` on a box whose
# only console is ssh. ONE llama-server with THREADS=1 and one slot per agent.
#
#   herd_akuma.sh start [name ...]   write configs + units, enable them
#   herd_akuma.sh stop  [name ...]   disable them (herd stops them on its next scan)
#   herd_akuma.sh status             herd status + the agents' own logs
#
# Joining another litter (docs/LITTER_RELAY_TOPOLOGY.md):
#   LITTER_STATIC_PEERS=ryzen@192.168.1.50:7700 herd_akuma.sh start sherlock
#
# HUB_ADDR stays 127.0.0.1:7700 even with peers relaying in: Akuma's listener is
# bound by PORT ONLY (`socket.listen(port)` -> smoltcp's unspecified-address
# ListenEndpoint), so a hub on 127.0.0.1:7700 already answers on this box's LAN
# address -- verified from another host on 2026-09-20. Do NOT "expose" it by
# putting the LAN address in HUB_ADDR: the agents connect to that same string,
# loopback diversion is 127.x only, and a connect to our own LAN address leaves
# on the NIC and is never answered.
set -e

HUB_ADDR="${HUB_ADDR:-127.0.0.1:7700}"
LITTER_NAME="${LITTER_NAME:-trashcan}"
LITTER_STATIC_PEERS="${LITTER_STATIC_PEERS:-}"
MODEL="${MODEL:-smollm2-135m}"
PORT="${PORT:-8081}"
THREADS="${THREADS:-1}"
MEOW_BIN="${MEOW_BIN:-/bin/meow-live}"
[ -x "$MEOW_BIN" ] || MEOW_BIN=/bin/meow

AVAIL=/etc/herd/available
ENABLED=/etc/herd/enabled

cmd="${1:-start}"
shift 2>/dev/null || true
AGENTS="${*:-${AGENTS:-sherlock}}"
NPAR=$(echo "$AGENTS" | wc -w)

case "$cmd" in
start)
    echo "[herd-litter] litter=$LITTER_NAME hub=$HUB_ADDR model=$MODEL threads=$THREADS"
    echo "[herd-litter] agents='$AGENTS' parallel=$NPAR bin=$MEOW_BIN"
    echo "[herd-litter] peers='${LITTER_STATIC_PEERS:-none}'"

    mkdir -p "$AVAIL" "$ENABLED"

    # One server, one slot per agent. --no-mmap because this kernel's file-page
    # cache and a model mapping are a bad pair under memory pressure.
    #
    # **`workdir = /tmp` is load-bearing.** A service inherits herd's own
    # working directory, which for an init is `/` -- and llama.cpp scans for its
    # ggml backend libraries relative to the working directory. From `/` that walk descends into `/src`, an entire
    # source tree: measured 2026-09-20, the process issued 4480 `stat` calls and
    # 140 `getdents64` while opening the model **zero** times, then went silent
    # with no listener and 1 s of CPU. It looks exactly like a model loading
    # slowly and is not. `yard_akuma.sh` did `cd /tmp` before `nohup`, which is
    # why the same binary and model came up in five seconds there; `workdir` is
    # herd's version of that `cd` (it chdirs around the spawn, since Akuma's
    # SPAWN syscall carries no working directory).
    cat > "$AVAIL/llama.conf" <<UNIT
command = /bin/llama-server
args = -m /models/${MODEL}.gguf --host 127.0.0.1 --port ${PORT} -c 2048 -t ${THREADS} --parallel ${NPAR} --no-mmap
workdir = /tmp
restart = true
restart_delay_ms = 5000
UNIT
    /bin/herd enable llama || true

    for name in $AGENTS; do
        home="/agents/${name}"
        mkdir -p "${home}/etc/meow"
        [ -f "/personas/${name}.md" ] && cp "/personas/${name}.md" "${home}/MEOW.md"
        # An agent's litter_key IS its identity on the relay: meow generates one
        # only when the config has none, so rewriting the config from scratch
        # would re-key the agent on every restart. Carry the seed across.
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
        # **No `start_delay_ms`.** It reads like the right way to let
        # llama-server load first, and on the Firecracker guest it made herd
        # start the agent TWICE: the service sits Stopped-with-a-delay, and
        # `reload_config`'s 20 s re-parse plus its own `start_stopped_services`
        # call can start it again next to the instance already running. Two live
        # processes sharing one MEOW_HOME, one name and one signing key, racing
        # the same bind. The agent needs no delay anyway -- it polls, and meow
        # retries its provider by itself.
        cat > "$AVAIL/${name}.conf" <<UNIT
command = ${MEOW_BIN}
args = litter live
env = MEOW_HOME=${home}
restart = true
restart_delay_ms = 10000
UNIT
        /bin/herd enable "$name" || true
    done

    mkdir -p /operator/etc/meow
    opkey=$(grep '^litter_key=' /operator/etc/meow/config 2>/dev/null || true)
    cat > /operator/etc/meow/config <<CFG
litter_agent_name=operator
litter_hub_addr=${HUB_ADDR}
litter_name=${LITTER_NAME}
litter_static_peers=${LITTER_STATIC_PEERS}
${opkey}
CFG
    echo "[herd-litter] enabled; herd rescans every 20s. Watch: herd log <svc>"
    ;;

stop)
    for name in $AGENTS llama; do
        /bin/herd disable "$name" || true
    done
    echo "[herd-litter] disabled: $AGENTS llama"
    ;;

status)
    /bin/herd status || true
    for name in $AGENTS; do
        echo "=== $name ==="
        /bin/herd log "$name" 2>/dev/null | tail -15
    done
    ;;

*)
    sed -n '2,30p' "$0"
    exit 1
    ;;
esac
