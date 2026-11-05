# Design and architecture

## Task contract

A **source** provides portable Pi values. A **target** keeps its device-specific and unrelated values. **Staging** selects configuration categories and destinations without writing files. A **review** is a captured, immutable plan. A **conflict** is a value needing an explicit source/target decision. **Apply** is a separate confirmed operation. **Offline** means no successful current connection, never proof that a previous write did not happen.

The public source fetches only JSON from AHGGG/my-pi-harness, resolving HEAD to one immutable SHA before reading files. No downloaded repository code is executed. Source revision and files are checked again before apply. Private-model definitions cannot come from the public source.

## Native workspace

- One light native window, familiar resizable panes, semantic GPUI Kit controls.
- First-run SSH setup saves connections before continuing. This computer is the default source.
- Two selection panes: source and selected configuration/files on the left, saved target-device cards on the right. Remote OS is not guessed.
- Review replaces both panes. Cancel preserves the selection and discards the plan; confirmation is a separate action.
- Drag-and-drop is an accelerator. Buttons and keyboard actions provide the same staging flow.
- Confirmation summarizes scope; conflict choices never silently default to source.
- Errors and per-device outcomes use text, not color alone. Blocked reviews never appear as a successful zero-difference result. A new selection invalidates the displayed review.
- Checkbox/path changes are saved immediately. Save acknowledgments do not replace newer UI state or clear an in-flight operation.
- Cmd/Ctrl+R refreshes profiles, Cmd/Ctrl+Enter prepares review, Cmd/Ctrl+Shift+R repeats the last selection. None applies automatically.

## Module interfaces

- `crates/desktop`: GPUI Kit presentation and an I/O worker. UI state never owns a network session or touches target files.
- `crates/sync-core`: strict connection parsing, host identity/authentication, fixed SSH worker transport, nonsecret profile persistence.
- `backend`: dependency-free Node JSONL engine adapted from my-pi-harness at 1d8b174. Captured plans and private content remain in process memory. Field allowlists, merge semantics, stale-plan rejection and backup/partial-failure behavior stay behind this interface.

All network/filesystem work runs away from the UI thread. Authentication events pause the I/O worker for a response from a native masked-input or host-trust dialog. JSONL stdout is a private protocol, not a diagnostic stream.

## Guidance used

Design and coding follow the requested [GPUI Kit skills](https://github.com/longbridge/gpui-kit/tree/328aec50f04c351950887e2c22369837cb8e4529/skills), including the Design Guides. Planning and test organization draw on [Matt Pocock's codebase-design, domain-modeling/grill-with-docs, and TDD guidance](https://github.com/mattpocock/skills/tree/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/skills/engineering). Sources were read, not installed as agent extensions.

## Deliberate limits

Built-in categories retain audited per-setting choices. Custom file/folder paths are relative to the configured Pi directory and reviewed as whole-file copies. Standalone permission policies carry an explicit warning; embedded permission fields and unknown MCP/settings fields remain outside the audited merger. There is no semantic policy-weakening analysis. Authentication/credential paths, hidden paths, symlinks, nested repositories and cross-platform filename collisions fail closed. Models are private files copied byte-for-byte after review, not interpreted as a fixed Pi schema. Review shows replacement, hidden contents, backup and unchanged-default notices. Runtime compatibility and provider connectivity belong to Pi. Unreadable or invalid source models can be skipped independently of other settings. Packaging, signing, live-device interoperability and assistive-technology validation need real platform evidence.
