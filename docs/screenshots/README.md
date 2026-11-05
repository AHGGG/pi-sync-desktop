# Native workflow captures

These are real GPUI/WGPU offscreen captures of the production workspace, not HTML mockups:

- `onboarding.png`: actual first-run backend state with an empty temporary app-config directory.
- `workspace.png`: actual backend startup with synthetic saved SSH profiles at `.example.invalid` hosts and a remembered selection. These profiles exist only in a temporary directory, never the product repository; no SSH connection is attempted.
- `review.png`: production review view with the explicit `--fixture-review` synthetic documentation rows. No user configuration or live device data is shown, and no sync is performed.

Run on a machine with a supported Metal or Vulkan renderer:

```sh
cargo run -p pi-sync-desktop --bin pi-sync-render-check \
  --features render-check --locked -- /tmp/pi-sync-workspace.png
cargo run -p pi-sync-desktop --bin pi-sync-render-check \
  --features render-check --locked -- /tmp/pi-sync-review.png --fixture-review
cargo run -p pi-sync-desktop --bin pi-sync-render-check \
  --features render-check --locked -- /tmp/pi-sync-navigation.png --fixture-navigation
```

For Linux with Chromium's existing Swiftshader ICD, set
`VK_ICD_FILENAMES=/usr/lib/chromium/vk_swiftshader_icd.json` and, when appropriate,
`RUST_FONTCONFIG_DLOPEN=1`. Normal platform linker dependencies still apply.
The tool drains SVG asset tasks, renders again, and rejects blank captures.
`--fixture-navigation` selects the synthetic target version through its outer edge, then activates that edge again to highlight hunk 2 of 3. It does not connect to a device or apply files.

Headless interaction tests separately exercise production controls, onboarding,
remembered selection, cancellation, keyboard review, authentication, and single-shot
confirmation. Real desktop smoke testing on macOS, Windows and a visible Linux session
is still required before calling those platforms runtime-verified.

`--fixture-next-file` extends the synthetic navigation fixture with a second text file. It verifies reaching the final hunk, then crossing to the next file on a separate click without choosing its version.
