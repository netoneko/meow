#!/usr/bin/env bash
# docker-linux-net-test.sh — verify meow linux-net mode in a container.
#
# What this proves:
#   meow's linux_net::resolve() and libakuma::net::TcpStream all use standard
#   Linux AArch64 socket syscalls. In this test they go through the Linux kernel
#   directly. Inside an Akuma stack=rump box the kernel's sysproxy intercepts
#   those same syscalls and routes them through the NetBSD rump TCP/IP stack.
#
# Requirements: Docker with linux/arm64 support (or native aarch64 host).
# Usage: bash userspace/meow/docker-linux-net-test.sh

set -euo pipefail
cd "$(dirname "$0")"

MEOW_BIN="target/aarch64-unknown-linux-musl/release/meow"

if [ ! -f "$MEOW_BIN" ]; then
    echo "==> Building meow (linux-net, aarch64-musl)..."
    MUSL_LIBC=/opt/homebrew/Cellar/musl-cross/0.9.11/libexec/aarch64-linux-musl/lib/libc.a
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc \
    RUSTFLAGS="-C link-self-contained=no -C link-arg=-nostartfiles -C link-arg=$MUSL_LIBC" \
    cargo +nightly build --release -Zbuild-std=core,alloc \
        --target aarch64-unknown-linux-musl \
        --features linux-net
fi

echo "==> $(file $MEOW_BIN | sed 's/.*: //')"

# Run meow inside an arm64 Alpine container with a mock ollama server.
# Both run inside the same container so there are no host-firewall issues.
# The mock server accepts POST /v1/chat/completions and returns "meow" as an
# SSE stream — the same wire format real ollama uses.
echo "==> Running meow inside docker (linux/arm64) against a mock ollama server..."

docker run --platform linux/arm64 --rm \
    -v "$(pwd)/$MEOW_BIN:/meow:ro" \
    alpine:3.20 sh -c '
        apk add -q python3 2>/dev/null

        # Start the mock ollama server
        python3 -c "
import http.server, json
PORT = 11435

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *a):
        print(\"[mock-ollama] \" + (fmt % a))
    def do_POST(self):
        cl = int(self.headers.get(\"Content-Length\", 0))
        req = json.loads(self.rfile.read(cl))
        prompt = req.get(\"messages\", [{}])[-1].get(\"content\", \"\")
        print(\"[mock-ollama] prompt: \" + prompt)
        # SSE stream: one token \"meow\" then stop
        body = (
            b\"data: {\\\"choices\\\":[{\\\"delta\\\":{\\\"role\\\":\\\"assistant\\\",\\\"content\\\":\\\"meow\\\"},\\\"index\\\":0}]}\\r\\n\\r\\n\"
            b\"data: {\\\"choices\\\":[{\\\"delta\\\":{},\\\"index\\\":0,\\\"finish_reason\\\":\\\"stop\\\"}]}\\r\\n\\r\\n\"
            b\"data: [DONE]\\r\\n\\r\\n\"
        )
        self.send_response(200)
        self.send_header(\"Content-Type\", \"text/event-stream\")
        self.send_header(\"Content-Length\", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

server = http.server.HTTPServer((\"127.0.0.1\", PORT), H)
print(\"[mock-ollama] listening on :\" + str(PORT))
server.serve_forever()
" &
        MOCK_PID=$!
        sleep 1

        # Write meow config pointing to the mock server at 127.0.0.1 (IP literal;
        # resolve() takes the fast-path and skips DNS entirely)
        mkdir -p /etc/meow
        printf "current_provider=ollama\ncurrent_model=test\n\n[provider:ollama]\nbase_url=http://127.0.0.1:11435\n" \
            > /etc/meow/config

        echo "--- meow -c \"say meow\" ---"
        /meow -c "say meow"
        EC=$?
        echo
        echo "--- exit $EC ---"
        kill $MOCK_PID 2>/dev/null || true
        exit $EC
    '

echo "==> Test passed: meow linux-net mode works in docker."
