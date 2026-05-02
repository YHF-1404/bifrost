#!/usr/bin/env bash
# Deploy the cross-built bifrost-server binary to the production host.
#
# Assumes:
#   * `cross build --release --target x86_64-unknown-linux-gnu -p bifrost-server`
#     has already produced the binary, OR pass `--build` to do it here.
#   * `ssh root@$SERVER_HOST` works without a password.

set -euo pipefail

cd "$(dirname "$0")/.."

SERVER_HOST=${SERVER_HOST:-root@64.176.40.25}
TARGET=x86_64-unknown-linux-gnu
BIN_PATH=target/${TARGET}/release/bifrost-server

if [[ "${1:-}" == "--build" ]]; then
    cross build --release --target "$TARGET" -p bifrost-server
fi

if [[ ! -f "$BIN_PATH" ]]; then
    echo "no binary at $BIN_PATH — run 'scripts/build-cross.sh' or pass '--build'" >&2
    exit 1
fi

echo "==> creating directories on $SERVER_HOST"
ssh "$SERVER_HOST" 'mkdir -p /etc/bifrost /var/lib/bifrost/received /run/bifrost'

# Linux refuses scp over an executable mmap'd as text — stop first.
echo "==> stopping running daemon (if any)"
ssh "$SERVER_HOST" 'systemctl stop bifrost-server 2>/dev/null || true'

echo "==> copying binary"
scp "$BIN_PATH" "$SERVER_HOST:/usr/local/bin/bifrost-server"

echo "==> copying config + systemd unit (won't overwrite existing config)"
scp deploy/server.toml.example "$SERVER_HOST:/etc/bifrost/server.toml.example"
ssh "$SERVER_HOST" 'test -f /etc/bifrost/server.toml || cp /etc/bifrost/server.toml.example /etc/bifrost/server.toml'
scp deploy/systemd/bifrost-server.service "$SERVER_HOST:/etc/systemd/system/bifrost-server.service"

echo "==> reloading + (re)starting"
ssh "$SERVER_HOST" 'systemctl daemon-reload && systemctl enable bifrost-server && systemctl restart bifrost-server'

sleep 1
echo
echo "==> status"
ssh "$SERVER_HOST" 'systemctl --no-pager --lines=20 status bifrost-server' || true
