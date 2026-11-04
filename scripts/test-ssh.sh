#!/usr/bin/env bash
# Explicit opt-in: real SSH bound only to localhost, disposable files and keys.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'Rust cargo is required.' >&2; exit 1; }
command -v ssh-keygen >/dev/null || { echo 'OpenSSH ssh-keygen is required.' >&2; exit 1; }
node_path=$(command -v node) || { echo 'Node.js 20+ is required.' >&2; exit 1; }
export PI_SYNC_TEST_NODE_DIR="$(dirname "$node_path")"
export PI_SYNC_TEST_SSHD="${PI_SYNC_TEST_SSHD:-/usr/sbin/sshd}"
[[ -x "$PI_SYNC_TEST_SSHD" ]] || { echo 'An existing sshd executable is required; no system configuration is changed.' >&2; exit 1; }
cargo test --locked -p sync-core --test localhost_ssh -- --ignored --test-threads=1
