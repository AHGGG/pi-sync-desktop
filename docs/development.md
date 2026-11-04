# Development

Use Rust 1.92 or newer, Node.js 20 or newer, and the checked-in Cargo.lock. GPUI Kit is pinned to 0.9.0, which pins a coherent GPUI snapshot. The application does not depend on its webview or JavaScript-shell layers.

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

## Verification

`node --test backend/tests/*.test.mjs` runs temporary-directory and simulated-transport tests without real devices. `cargo test -p sync-core` exercises parsing, trust decisions and profile persistence. Desktop tests cover production GPUI controls, presentation state and worker integration without requiring a production account. Run `scripts/test-ssh.sh` explicitly for an isolated localhost OpenSSH fixture using only disposable keys/configuration; it never changes real SSH credentials or system configuration.

CI defines Linux, macOS and Windows native build/test jobs. A matrix definition is not evidence of a passing platform build. Check the exact commit's job results. Native window/input, scaling, keyboard, accessibility, live SSH and installer behavior require separate smoke tests.

## Distribution

The supported development entry point is Cargo. No npm package or installer is published. Prefer signed native releases first. A future `npx` wrapper can map OS/architecture to a pinned signed release and verified checksum, then spawn the binary without a browser engine. Avoid an installer that downloads and runs an unversioned `latest` executable.
