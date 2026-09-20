#!/bin/sh
# One clean experiment run: fresh inference servers, fresh logs, fresh
# litter, one task.
#
# Exists because mixing runs makes every measurement untrustworthy. The
# yard's own logs reset when the container is recreated, but the
# llama-server logs do not — so a timing read after three restarts is
# three runs interleaved, and "the leader took 39 minutes" turned out to
# be one run's meow log against another run's server log.
#
# Usage:
#   litter/experiment.sh "<task>" ["<what the answer should look like>"]
#
# Env:
#   MODEL_BLOB   GGUF to serve (default: the qwen3:4b blob in ollama's store)
#   AGENTS       agent names, in port order (default: the four personas)
#   CTX          context per server (default 16384)
#   THREADS      -t for each server (default 1)
set -e
cd "$(dirname "$0")/.."

TASK="${1:?usage: experiment.sh \"<task>\" [\"<expected answer shape>\"]}"
WANT="${2:-}"
AGENTS="${AGENTS:-sherlock hercules zenigata ressler}"
CTX="${CTX:-16384}"
THREADS="${THREADS:-1}"
RUN_DIR="${RUN_DIR:-/tmp/litter-run}"
MODEL_BLOB="${MODEL_BLOB:-$HOME/.ollama/models/blobs/sha256-3e4cb14174460404e7a233e531675303b2fbf7749c02f91864fe311ab6344e4f}"

[ -f "$MODEL_BLOB" ] || { echo "no model at $MODEL_BLOB" >&2; exit 1; }

echo "[exp] stopping previous run"
pkill -f 'llama-server -m' 2>/dev/null || true
litter/yard.sh stop >/dev/null 2>&1 || true
sleep 3

# Fresh logs. Everything below this line belongs to exactly one run.
rm -rf "$RUN_DIR"
mkdir -p "$RUN_DIR"
echo "[exp] logs: $RUN_DIR"

i=0
for a in $AGENTS; do
    port=$((8081 + i))
    nohup llama-server -m "$MODEL_BLOB" --host 0.0.0.0 --port "$port" \
        -c "$CTX" -t "$THREADS" -np 1 --reasoning-budget 0 --alias "$a" \
        > "$RUN_DIR/llama-$a.log" 2>&1 &
    i=$((i + 1))
done

echo "[exp] waiting for $i inference server(s)"
i=0
for a in $AGENTS; do
    port=$((8081 + i))
    until curl -s --max-time 2 "http://127.0.0.1:$port/v1/models" >/dev/null 2>&1; do sleep 2; done
    echo "[exp]   $a ready on $port"
    i=$((i + 1))
done

spec=""
for a in $AGENTS; do spec="$spec $a:x"; done
LITTER_BASE_PORT=8081 LITTER_AGENTS="${spec# }" litter/yard.sh start >/dev/null
echo "[exp] litter up; waiting for the hub"
until docker exec -e MEOW_HOME=/operator litter-yard /bin/meow litter peers >/dev/null 2>&1; do sleep 2; done

litter/yard.sh task "$TASK" ${WANT:+"$WANT"}
echo "[exp] running. watch with: python3 $RUN_DIR/../timeline.py  (or litter/yard.sh watch)"
