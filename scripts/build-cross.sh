#!/usr/bin/env bash
# Cross-compile both production binaries.
#
# Output:
#   target/x86_64-unknown-linux-gnu/release/bifrost-server
#   target/aarch64-unknown-linux-gnu/release/bifrost-client

set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> bifrost-server (x86_64-unknown-linux-gnu)"
cross build --release --target x86_64-unknown-linux-gnu -p bifrost-server

echo "==> bifrost-client (aarch64-unknown-linux-gnu)"
cross build --release --target aarch64-unknown-linux-gnu -p bifrost-client

echo
echo "Built:"
ls -lh target/x86_64-unknown-linux-gnu/release/bifrost-server \
       target/aarch64-unknown-linux-gnu/release/bifrost-client
