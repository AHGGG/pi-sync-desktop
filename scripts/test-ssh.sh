#!/usr/bin/env bash
# Explicit opt-in: real SSH bound only to localhost, disposable files and keys.
# Restricted runners without AF_UNIX may explicitly set PI_SYNC_TEST_SKIP_AGENT=1;
# this leaves agent integration unverified and is not equivalent to a full pass.
set -euo pipefail
if [[ "${PI_SYNC_TEST_SKIP_AGENT:-}" == 1 ]]; then
  echo 'WARNING: ssh-agent coverage explicitly skipped; this is not a full integration pass.' >&2
fi
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'Rust cargo is required.' >&2; exit 1; }
command -v ssh >/dev/null || { echo 'OpenSSH ssh is required.' >&2; exit 1; }
command -v ssh-agent >/dev/null || { echo 'OpenSSH ssh-agent is required.' >&2; exit 1; }
command -v ssh-add >/dev/null || { echo 'OpenSSH ssh-add is required.' >&2; exit 1; }
command -v ssh-keygen >/dev/null || { echo 'OpenSSH ssh-keygen is required.' >&2; exit 1; }
node_path=$(command -v node) || { echo 'Node.js 20+ is required.' >&2; exit 1; }
export PI_SYNC_TEST_NODE_DIR="$(dirname "$node_path")"
export PI_SYNC_TEST_SSHD="${PI_SYNC_TEST_SSHD:-/usr/sbin/sshd}"
[[ -x "$PI_SYNC_TEST_SSHD" ]] || { echo 'An existing sshd executable is required; no system configuration is changed.' >&2; exit 1; }
cargo build --locked -p sync-core --example askpass_fixture
# Respect Cargo's configured target directory without assuming a user shell setup.
target_dir=$(cargo metadata --format-version 1 --no-deps | node -p 'JSON.parse(require("fs").readFileSync(0, "utf8")).target_directory')
export PI_SYNC_TEST_ASKPASS="$target_dir/debug/examples/askpass_fixture"
cargo test --locked -p sync-core --test localhost_ssh -- --ignored --test-threads=1
