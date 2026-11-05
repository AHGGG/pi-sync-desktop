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

1. On first launch, save one or more SSH connections, then choose **Next**. Saving a profile does not claim the connection works; host trust and authentication happen when reviewing.
2. This computer is the default source. The left pane selects what to sync; the right pane selects saved target devices. **Change** can still select another source or the public preset.
3. Select built-in configuration items or **Add file or folder** by its relative path inside the Pi agent directory, such as `extensions` or `skills/example/SKILL.md`. Selections and targets are saved immediately, including before the first review.
4. **Review changes** opens a full-window comparison with source/target contents or private-file hashes. File copies and conflicts need an explicit Source, Target or Skip choice. **Cancel review** returns to the remembered selection without writing.
5. **Confirm sync** summarizes the actual targets and asks for final confirmation. Source and target changes invalidate the review. Connection failures are shown separately from a successful zero-difference result.

**Review saved selection** reads fresh data with one click. It never reuses an old plan or silently applies remembered decisions. Local OS icons and generic SSH-device icons distinguish devices without guessing a remote platform.

Custom paths are relative to each configured Pi directory, not arbitrary absolute source/destination mappings. Folders are recursive; target-only files are never deleted and empty folders are not copied. Transfers allow 256 files, 4 MiB per file and 8 MiB total custom bytes. Symlinks, Git working trees, authentication/credential paths, hidden paths and nonportable names are blocked with an error rather than silently skipped.

## Safety and scope

- Device ID, shell settings, default provider/model, and unrelated target values are preserved.
- Settings and packages, reviewed MCP fields, and reviewed subagent fields are supported. Unsupported fields remain local.
- Private models move only through private process pipes and encrypted device connections. They never enter the public repository, profiles, or review text. The entire file is copied after review, with a backup of the previous bytes. Only source JSON readability is checked; Pi owns model compatibility. Invalid or unreadable source models can be skipped while other settings sync.
- Embedded permission settings stay local. Explicitly selected standalone permission files can be copied only through whole-file review, with a tool-access warning and an explicit choice. There is no semantic policy-weakening analysis.
- A successful operation keeps one recovery backup of the files involved in that operation. Partial failures retain recovery evidence instead of pruning it.
- Passwords/passphrases are not saved. Profiles contain connection information and Pi paths only; remembered selections contain identifiers and relative paths, never content. Changed SSH host keys fail closed.

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
