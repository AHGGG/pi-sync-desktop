# Native workspace capture

`workspace.png` is a real GPUI/WGPU offscreen capture of the production workspace,
using the backend's actual initial profile snapshot. It is not an HTML mockup or
a test-platform blank capture. No sync is applied by the capture tool.

Run on a machine with a supported Metal or Vulkan renderer:

```sh
cargo run -p pi-sync-desktop --bin pi-sync-render-check \
  --features render-check --locked -- docs/screenshots/workspace.png
```

For a Linux machine that already includes Chromium's Swiftshader ICD:

```sh
VK_ICD_FILENAMES=/usr/lib/chromium/vk_swiftshader_icd.json \
RUST_FONTCONFIG_DLOPEN=1 \
cargo run -p pi-sync-desktop --bin pi-sync-render-check \
  --features render-check --locked -- docs/screenshots/workspace.png
```

Normal platform linker dependencies still apply. The tool refuses to substitute
a mock renderer and fails if the captured image is empty or uniformly blank.
Headless interaction tests run the production view, native controls and input
handlers, separately from this pixel capture:

```sh
cargo test -p pi-sync-desktop --bin pi-sync-desktop workspace::tests --locked
```

Native desktop smoke testing on macOS, Windows and a visible Linux session is
still required before describing those platforms as runtime-verified.
