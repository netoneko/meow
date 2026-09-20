#!/bin/sh
# Host-side control for a persistent litter yard: ONE long-lived container
# whose residents are `meow litter live` agents that never exit — they poll
# their inboxes and wake on new messages; whoever wins the hub bind race
# runs the raft thread (see litter/yard_init.sh, docs/LITTER_STATE_MACHINE.md).
#
# Usage:
#   litter/yard.sh start [akuma-src-dir]   stand the yard up (detached)
#   litter/yard.sh stop                    tear it down (all state is in-memory:
#                                          history below the last compaction
#                                          marker is gone by design)
#   litter/yard.sh talk <agent|litter> "message"
#                                          operator -> one agent, or the whole
#                                          litter (`to: litter` fans out)
#   litter/yard.sh task "do this"          open a TRACKED task: the leader's
#                                          table assigns it to the least
#                                          loaded agent with a lease
#   litter/yard.sh respawn <agent>         (re)start one resident after a kill
#   litter/yard.sh watch                   transcript (meow litter observe)
#   litter/yard.sh logs                    tail every agent's log
#
# talk/task/watch are just `docker exec` with MEOW_HOME pointed at the
# operator's scope.

set -e
cd "$(dirname "$0")/.."

NAME=litter-yard
BIN="$PWD/target/aarch64-unknown-linux-musl/release/meow"
AKUMA_SRC="${2:-$(cd "$(dirname "$0")/../.." && pwd)}"   # akuma repo root by default

op() {  # run meow as the operator (root) inside the yard
    docker exec -e MEOW_HOME=/operator "$NAME" /bin/meow "$@"
}

case "${1:-}" in
start)
    docker rm -f "$NAME" 2>/dev/null || true
    docker run -d --platform linux/arm64 --name "$NAME" \
      -e AGENTS="${LITTER_AGENTS:-sherlock:qwen3:4b hercules:gemma4-yolo-4b:latest zenigata:gemma4:e4b ressler:qwen3.5:0.8b}" \
      -e OLLAMA_URL="${OLLAMA_URL:-http://192.168.65.254:11434}" \
      -e LITTER_NAME="${LITTER_NAME:-yard}" \
      -e LITTER_STATIC_PEERS="${LITTER_STATIC_PEERS:-ryzen@192.168.1.126:7700}" \
      -v "$BIN:/bin/meow:ro" \
      -v "$PWD/litter/yard_init.sh:/yard_init.sh:ro" \
      -v "$PWD/litter/personas:/personas:ro" \
      -v "$AKUMA_SRC:/akuma-src:ro" \
      alpine:3.20 sh /yard_init.sh
    ;;

stop)
    docker rm -f "$NAME"
    ;;

talk)
    agent="${2:?usage: yard.sh talk <agent|litter> \"message\"}"
    msg="${3:?usage: yard.sh talk <agent|litter> \"message\"}"
    op litter send --to "$agent" --body "$msg" --from root
    ;;

task)
    msg="${2:?usage: yard.sh task \"do this\"}"
    # [task] is the wire-level task-table hook: the leader's raft thread
    # opens a tracked task, leases it to the least-loaded agent, requeues
    # it if the lease expires, and folds the [done] into the event log.
    op litter send --to litter --body "[task] $msg" --from root
    ;;

respawn)
    agent="${2:?usage: yard.sh respawn <agent>}"
    model="$(docker exec "$NAME" sh -c "grep current_model= /agents/$agent/etc/meow/config | cut -d= -f2")"
    docker exec -d "$NAME" sh -c "cd / && MEOW_HOME=/agents/$agent /bin/meow litter live >> /agents/$agent/log 2>&1"
    echo "[yard] $agent respawning (model $model)"
    ;;

watch)
    op litter observe
    ;;

logs)
    for agent in $(docker exec "$NAME" ls /agents); do
        echo "=== $agent ==="
        docker exec "$NAME" tail -30 "/agents/$agent/log"
    done
    ;;

*)
    sed -n '2,24p' "$0"
    exit 1
    ;;
esac
