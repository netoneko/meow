#!/bin/sh
# Host-side control for a persistent litter yard: ONE long-lived container
# (not --rm) whose residents are `meow litter live` agents that never exit —
# they poll their inboxes and wake on new messages, and whoever wins the hub
# bind race coordinates (see litter/yard_init.sh and tools::litter::live).
#
# Usage:
#   litter/yard.sh start [akuma-src-dir]   stand the yard up (detached)
#   litter/yard.sh stop                    tear it down (roster+state die with it)
#   litter/yard.sh talk <agent|litter> "message"
#                                          operator -> one agent, or the whole
#                                          litter (`--to litter` fans out)
#   litter/yard.sh task "do this"          drop a task into the coordinator's
#                                          task memory (/litter/tasks); it gets
#                                          broadcast and marked .dispatched
#   litter/yard.sh watch                   transcript (meow litter observe)
#   litter/yard.sh logs                    tail every agent's stdout
#
# Nothing here is meow-specific magic: talk/task/watch are just `docker exec`
# with MEOW_HOME pointed at the operator's (or an agent's) scope.

set -e
cd "$(dirname "$0")/.."

NAME=litter-yard
BIN="$PWD/target/aarch64-unknown-linux-musl/release/meow"
AKUMA_SRC="${2:-$(cd "$(dirname "$0")/../.." && pwd)}"   # the akuma repo root by default

case "${1:-}" in
start)
    docker rm -f "$NAME" 2>/dev/null || true
    # -d: detached and persistent (no --rm — the yard is meant to outlive any
    # one task). AGENTS mirrors run_litter.sh's persona:model list; the
    # OLLAMA_MAX_LOADED_MODELS=2 caveat applies here too (yard_init staggers
    # starts by 1s, and agents stagger their own turns by name hash).
    docker run -d --platform linux/arm64 --name "$NAME" \
      -e AGENTS="${LITTER_AGENTS:-sherlock:qwen3:4b hercules:gemma4-yolo-4b:latest zenigata:gemma4:e4b ressler:qwen3.5:0.8b}" \
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
    # MEOW_HOME=/operator: the operator config yard_init wrote — agent name
    # `operator`, hub 127.0.0.1:7700. Not itself a resident.
    docker exec -e MEOW_HOME=/operator "$NAME" \
      /bin/meow litter send --to "$agent" --body "$msg" --from operator
    ;;

task)
    msg="${2:?usage: yard.sh task \"do this\"}"
    docker exec "$NAME" sh -c \
      "cat > /litter/tasks/task-\$(date +%s).md <<'EOF'
$msg
EOF"
    echo "task dropped in /litter/tasks — the coordinator broadcasts it within ~30s"
    ;;

watch)
    docker exec -e MEOW_HOME=/operator "$NAME" /bin/meow litter observe
    ;;

logs)
    for agent in $(docker exec "$NAME" ls /agents); do
        echo "=== $agent ==="
        docker exec "$NAME" tail -40 "/agents/$agent/log"
    done
    ;;

*)
    sed -n '2,25p' "$0"
    exit 1
    ;;
esac
