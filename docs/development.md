# Development

Use Rust 1.92 or newer, Node.js 20 or newer, OpenSSH 8.7 or newer, and the checked-in Cargo.lock. GPUI Kit is pinned to 0.9.0, which pins a coherent GPUI snapshot. The application does not depend on its webview or JavaScript-shell layers.

## Linux

GPUI needs a graphical X11/Wayland session and Vulkan-capable graphics to display a real window. On Ubuntu 24.04 the build dependencies are:

```sh
sudo apt-get install gcc g++ clang cmake libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libvulkan1
cargo run --locked -p pi-sync-desktop
```

A headless machine may compile without being able to render. If fontconfig's runtime library is already installed but development metadata is unavailable, its supported `RUST_FONTCONFIG_DLOPEN=1` build option uses dynamic loading. This is not a substitute for a graphical session.

## macOS and Windows

macOS: macOS 15+, Xcode command-line tools, standard Rust toolchain.

Windows: Windows 10+, MSVC Rust toolchain, Visual Studio C++ build tools, Windows SDK and CMake. Node must be on PATH for the app and for remote SSH worker sessions. Remote Windows SSH requires Node executable lookup from the service's normal command environment; actual interoperability remains a validation item.

## System OpenSSH

The client must support the transport's required options, including `SessionType`, `StdinNull`, `ForkAfterAuthentication` and forced askpass (`SSH_ASKPASS_REQUIRE=force`). An old client may fail configuration preflight; update the client rather than relaxing transport guards or server key-exchange settings. Install `openssh-client` on Debian/Ubuntu, the OpenSSH Client optional feature on Windows, or check the built-in `/usr/bin/ssh` and PATH on macOS. Windows first checks the system OpenSSH installation, then falls back to PATH.

New `ssh alias` profiles inherit omitted user/port values from OpenSSH config. Existing saved explicit values remain explicit, including the old default port 22. Remove and re-add a profile as `ssh alias` if it should inherit both user and port; there is no edit-profile flow yet. Agent environment comes from the process launching the GUI, which can differ from an interactive terminal. Relaunch after changing it. Bitwarden unlock and signing approval remain in Bitwarden; neither a dedicated Bitwarden integration nor actual user setup validation is claimed. Custom `UserKnownHostsFile` and `KnownHostsCommand` are intentionally unsupported and blocked before connection. Config is trusted: `Match exec` can run during parsing. See [security](security.md) for the complete transport restrictions.

## Verification

`node --test backend/tests/*.test.mjs` runs temporary-directory and simulated-transport tests without real devices. `cargo test --locked -p sync-core` exercises parsing, transport guard arguments, askpass validation, trust decisions and profile persistence. Desktop tests cover production GPUI controls, presentation state and worker integration without requiring a production account. Run `scripts/test-ssh.sh` explicitly for an isolated localhost OpenSSH fixture using only disposable keys/configuration; it never changes real SSH credentials or system configuration. The fixture requires `ssh`, `sshd`, `ssh-keygen`, `ssh-agent`, `ssh-add`, Node and Cargo. Its dedicated-agent case is required by default and uses only a disposable key/socket, never the user's agent. Restricted environments that cannot create AF_UNIX sockets may explicitly run `PI_SYNC_TEST_SKIP_AGENT=1 scripts/test-ssh.sh`; record this as incomplete agent coverage, not a full integration pass. The helper executable is built by the script before tests run.

CI defines Linux, macOS and Windows native build/test jobs, with mandatory real SSH and isolated-agent integration on Linux. A matrix definition is not evidence of a passing platform build. Check the exact commit's job results. Native window/input, scaling, keyboard, accessibility, live SSH and installer behavior require separate smoke tests.

## Distribution

The supported development entry point is Cargo. No npm package or installer is published. Prefer signed native releases first. A future `npx` wrapper can map OS/architecture to a pinned signed release and verified checksum, then spawn the binary without a browser engine. Avoid an installer that downloads and runs an unversioned `latest` executable.
