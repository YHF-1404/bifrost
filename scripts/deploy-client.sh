#!/usr/bin/env bash
# Deploy the cross-built bifrost-client binary to the router host.

set -euo pipefail

cd "$(dirname "$0")/.."

CLIENT_HOST=${CLIENT_HOST:-root@192.168.200.1}
TARGET=aarch64-unknown-linux-gnu
BIN_PATH=target/${TARGET}/release/bifrost-client

if [[ "${1:-}" == "--build" ]]; then
    cross build --release --target "$TARGET" -p bifrost-client
fi

if [[ ! -f "$BIN_PATH" ]]; then
    echo "no binary at $BIN_PATH — run 'scripts/build-cross.sh' or pass '--build'" >&2
    exit 1
fi

echo "==> creating directories on $CLIENT_HOST"
ssh "$CLIENT_HOST" 'mkdir -p /etc/bifrost /var/lib/bifrost/received /run/bifrost'

echo "==> stopping running daemon (if any)"
ssh "$CLIENT_HOST" 'systemctl stop bifrost-client 2>/dev/null || true'

echo "==> copying binary"
scp "$BIN_PATH" "$CLIENT_HOST:/usr/local/bin/bifrost-client"

echo "==> copying config + systemd unit (won't overwrite existing config)"
scp deploy/client.toml.example "$CLIENT_HOST:/etc/bifrost/client.toml.example"
ssh "$CLIENT_HOST" 'test -f /etc/bifrost/client.toml || cp /etc/bifrost/client.toml.example /etc/bifrost/client.toml'
scp deploy/systemd/bifrost-client.service "$CLIENT_HOST:/etc/systemd/system/bifrost-client.service"

echo "==> reloading + (re)starting"
ssh "$CLIENT_HOST" 'systemctl daemon-reload && systemctl enable bifrost-client && systemctl restart bifrost-client'

sleep 1
echo
echo "==> status"
ssh "$CLIENT_HOST" 'systemctl --no-pager --lines=20 status bifrost-client' || true
