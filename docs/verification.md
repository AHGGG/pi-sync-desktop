# Verification record

Recorded on 2026-11-04 against this source tree. These results distinguish production code, headless integration, and actual user-platform testing.

## Passed in the Linux development environment

- Rust 1.99.1, Debian 13, Node 24.22.1.
- `cargo check --locked --workspace`: native GPUI application type-checks.
- `node --test backend/tests/*.test.mjs`: 65 tests, including field merges, first-run conflicts, source/target staleness, preserved defaults, private-model flow, model blockers, explicit MCP command review, partial failures and backup retention.
- `cargo test -p sync-core`: 9 tests for strict SSH parsing, nonsecret profile persistence, file modes/symlink rejection and host-key decisions.
- `cargo test -p pi-sync-desktop`: 13 tests. Includes Rust-to-embedded-Node preview/resolve/apply integration, six presentation rules, and four real GPUI component interactions: staging/keyboard review, drag-and-drop, conflict keyboard resolution with separate confirmation, and masked one-shot authentication.
- `scripts/test-ssh.sh`: isolated real OpenSSH integration. Ephemeral localhost-only server, temporary keys and files; unknown fingerprint acceptance/refusal, key authentication, encrypted-key passphrase success/cancel/wrong-answer handling, trusted reconnect, snapshot/apply, stale-hash refusal and changed-key blocking. The fixture is opt-in and ignored by the default test command. No real device or user credentials were changed.
- Real offscreen WGPU rendering through the environment's existing Swiftshader Vulkan implementation, using the production workspace and actual startup state. Screenshot visually inspected; see [capture details](screenshots/README.md).

The environment's runtime X11/fontconfig libraries were already present but their development link aliases were missing. Local-only linker aliases and the library's supported `RUST_FONTCONFIG_DLOPEN=1` option were used; no system files were changed. This does not establish hardware-GPU performance.

## Release/build gate

Formatting, warnings-denied Clippy, aggregate tests and public credential-pattern scanning passed. Linux release compilation also passed. The stripped x86_64 Linux binary measured approximately 35.2 MiB (without Node.js or system libraries). Binary size is not a runtime-memory or performance benchmark.

## Not established by these tests

- Real-window manual operation, OS accessibility integration, IME, high-DPI/multi-monitor behavior and keyboard layout coverage.
- Windows or macOS execution. CI defines native builds/tests for those platforms, but only completed jobs for the exact commit count as evidence.
- Live user-device connectivity, password-authenticated SSH servers, keyboard-interactive/MFA, agent interoperability across operating systems, remote Windows shells or recovery after physical network interruption.
- Installer signing/notarization, auto-update, npm distribution, or a released `npx` launcher.
- Permission-policy synchronization. It is intentionally unavailable; existing policies are preserved.

No test result is a claim that all model providers are reachable or that a multi-device operation is atomic.
