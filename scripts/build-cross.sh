#!/usr/bin/env bash
# Cross-compile both production binaries.
#
# Output:
#   target/x86_64-unknown-linux-gnu/release/bifrost-server
#   target/aarch64-unknown-linux-gnu/release/bifrost-client
#
# The server binary embeds web/dist/ at compile time via rust-embed.
# This script runs `npm run build` first so the embed has fresh
# assets. Pass --skip-web to skip the frontend build (useful when
# you're iterating on Rust only or your machine doesn't have Node).

set -euo pipefail

cd "$(dirname "$0")/.."

skip_web=
for arg in "$@"; do
  case "$arg" in
    --skip-web) skip_web=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [ -z "$skip_web" ]; then
  if [ ! -d web ]; then
    echo "==> no web/ directory; skipping frontend build"
  else
    echo "==> WebUI (web/)"
    (
      cd web
      if [ ! -d node_modules ]; then
        npm install
      fi
      npm run build
    )
  fi
fi

echo "==> bifrost-server (x86_64-unknown-linux-gnu)"
cross build --release --target x86_64-unknown-linux-gnu -p bifrost-server

echo "==> bifrost-client (aarch64-unknown-linux-gnu)"
cross build --release --target aarch64-unknown-linux-gnu -p bifrost-client

echo
echo "Built:"
ls -lh target/x86_64-unknown-linux-gnu/release/bifrost-server \
       target/aarch64-unknown-linux-gnu/release/bifrost-client
