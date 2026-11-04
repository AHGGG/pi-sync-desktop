# Pi Sync Desktop

A native Rust + GPUI Kit workspace for reviewing Pi configuration changes before applying them to your computer or SSH devices. No Electron, webview, local web server, or browser UI.

**Early development build.** Core safety tests are automated. Native platform builds and actual-window validation are tracked separately in [verification](docs/verification.md). Do not mistake a source build for a signed, release-ready installer.

![Native Pi Sync workspace](docs/screenshots/workspace.png)

## Run from source

Requires Rust 1.92+, Node.js 20+ on the computer running the app, and Node.js 20+ on SSH destinations. Node is the dependency-free configuration worker, not the UI. Authentication uses libssh2, your SSH agent or keys, and in-app password/passphrase prompts.

```sh
cargo run --locked -p pi-sync-desktop
```

On Linux, install the native [GPUI Kit build dependencies](https://gpui-kit.com/docs/installation/). macOS requires macOS 15+ and Xcode command-line tools; Windows requires the MSVC Rust toolchain and Visual Studio C++ build tools. See [development](docs/development.md).

## Workflow

1. Choose the public harness, this computer, or a saved SSH device as the source.
2. Add devices with `ssh user@host -p 22`, optionally `-i '/path/to/key'`. Compare the host fingerprint before trusting it.
3. Drag Pi configuration categories into the queue, or use their keyboard-accessible Add buttons. Select destination devices.
4. Review each setting. Resolve conflicts with Source, Target, or Skip.
5. Confirm Apply. Each destination reports its own result. A changed source or target requires a fresh review.

**Repeat last selection** reads the latest data and prepares a new review. It never silently applies remembered choices.

## Safety and scope

- Device ID, shell settings, default provider/model, and unrelated target values are preserved.
- Settings and packages, reviewed MCP fields, and reviewed subagent fields are supported. Unsupported fields remain local.
- Private models move only through private process pipes and encrypted device connections. They never enter the public repository, profiles, or review text. Replacement is blocked when default-model safety cannot be established.
- Permission policies are preserved, not copied. This release does not yet provide the separate policy-review workflow required to change them safely.
- A successful operation keeps one recovery backup of the files involved in that operation. Partial failures retain recovery evidence instead of pruning it.
- Passwords/passphrases are not saved. Profiles contain connection information and Pi paths only. Changed SSH host keys fail closed.

See [security](docs/security.md) for limitations, data locations, and recovery. See [design](docs/design.md) for interaction and architecture decisions.

## Check

```sh
node --test backend/tests/*.test.mjs
node scripts/check-public.mjs
cargo test --locked --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Native binaries/installers and an `npx` launcher are not published yet. A small launcher could select a verified native release later; it would not change the native UI architecture.
