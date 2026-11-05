# Design and architecture

## Task contract

A **source** provides portable Pi values. A **target** keeps its device-specific and unrelated values. **Staging** selects configuration categories and destinations without writing files. A **review** is a captured, immutable plan. A **conflict** is a value needing an explicit source/target decision. **Apply** is a separate confirmed operation. **Offline** means no successful current connection, never proof that a previous write did not happen.

The public source fetches only JSON from AHGGG/my-pi-harness, resolving HEAD to one immutable SHA before reading files. No downloaded repository code is executed. Source revision and files are checked again before apply. Private-model definitions cannot come from the public source.

## Native workspace

- One light native window, familiar resizable panes, semantic GPUI Kit controls.
- First-run SSH setup saves connections before continuing. This computer is the default source.
- Two selection panes: source and selected configuration/files on the left, saved target-device cards on the right. Remote OS is not guessed.
- Review replaces both panes with a quiet file navigator and an aligned, wrapping native code diff. Source is always left and target right, identified by actual device names. Binary/private/truncated files keep safe metadata previews.
- Click either content pane, including blank space, to choose that version for the target. Text-selection, modifier-click, gutter-selection and scrolling gestures do not choose. Headers remain an optional Tab/Enter/Space alternative; Alt+Left/Right chooses source/target. Source-for-all stages available changes only; unavailable sources and MCP command-detail gates remain explicit.
- Each side reveals a small translucent arrow overlay on hover or keyboard focus. Overlays do not reserve diff width; no instruction subtitle is shown. After explicitly choosing a side, its outer edge navigates the previous (left) or next (right) hunk within the current item. Clicking the opposite edge chooses that side first. The arrow buttons and Alt+Up/Down navigate the same hunks without changing the version decision. A visible hunk count and active highlight show movement even when the entire diff fits onscreen. First/last boundaries do not wrap; single-hunk and safe opaque previews do not offer nonexistent navigation. Sidebar items are separate from in-file hunks.
- Pending filters unresolved version choices, not all unviewed or unsynced files. The bottom bar keeps Cancel next to Confirm. Cancel preserves selection and discards the plan, cached previews and decisions; confirmation is a separate action.
- Whole-file hashes remain in header tooltips and standalone policy changes retain a visible compact marker. Full-replacement/private-content consequences are disclosed at final confirmation rather than repeated beneath every diff.
- Drag-and-drop is an accelerator. Buttons and keyboard actions provide the same staging flow.
- Confirmation summarizes scope; conflict choices never silently default to source.
- Errors and per-device outcomes use text, not color alone. Blocked reviews never appear as a successful zero-difference result. A new selection invalidates the displayed review.
- Checkbox/path changes are saved immediately. Save acknowledgments do not replace newer UI state or clear an in-flight operation.
- Cmd/Ctrl+R refreshes profiles, Cmd/Ctrl+Enter prepares review, Cmd/Ctrl+Shift+R repeats the last selection. None applies automatically.

## Module interfaces

- `crates/desktop`: GPUI Kit presentation and an I/O worker. UI state never owns a network session or touches target files.
- `crates/sync-core`: strict connection parsing, system OpenSSH transport and native askpass bridge, nonsecret profile persistence.
- `backend`: dependency-free Node JSONL engine adapted from my-pi-harness at 1d8b174. Captured plans and private content remain in process memory. Field allowlists, merge semantics, stale-plan rejection and backup/partial-failure behavior stay behind this interface.

All network/filesystem work runs away from the UI thread. Authentication events pause the I/O worker for a response from a native masked-input or host-trust dialog. JSONL stdout is a private protocol, not a diagnostic stream. Each remote connection owns one system SSH process and one persistent Node worker. This preserves worker state between snapshot and apply without spawning a fresh session per operation. Requests/code are carried as bounded JSONL data through private pipes; the remote launcher is fixed. Failed or uncertain requests poison the connection and cannot trigger an automatic apply retry.

System OpenSSH provides normal aliases, identities, native-agent selection and cryptographic negotiation. New omitted user/port values defer to config; legacy explicit fields retain their meaning. The GUI askpass helper uses a capability-authenticated local bridge for password, passphrase and unknown-host prompts. Changed keys remain blocked. Standard user and native global host trust are retained alongside the app's private file; custom `UserKnownHostsFile` and `KnownHostsCommand` fail preflight instead of silently changing the trust policy.

The app disables config-driven forwarding, proxy/jump paths, local/remote commands, multiplexing, session suppression and keyboard-interactive/MFA. This is a constrained session, not a sandbox for trusted local SSH config: `Match exec` can still execute during configuration parsing. Agent environment is inherited at launch. Bitwarden may supply the native agent, but approval stays in Bitwarden and actual interoperability must be verified separately.

## Guidance used

Design and coding follow the requested [GPUI Kit skills](https://github.com/longbridge/gpui-kit/tree/328aec50f04c351950887e2c22369837cb8e4529/skills), including the Design Guides. Planning and test organization draw on [Matt Pocock's codebase-design, domain-modeling/grill-with-docs, and TDD guidance](https://github.com/mattpocock/skills/tree/4ae2317ddb22b046e26455e4f9c9b3aaaf62212d/skills/engineering). Sources were read, not installed as agent extensions.

## Deliberate limits

Built-in categories retain audited per-setting choices. Custom file/folder paths are relative to the configured Pi directory and reviewed as whole-file copies. Standalone permission policies carry an explicit warning; embedded permission fields and unknown MCP/settings fields remain outside the audited merger. There is no semantic policy-weakening analysis. Authentication/credential paths, hidden paths, symlinks, nested repositories and cross-platform filename collisions fail closed. Models are private files copied byte-for-byte after review, not interpreted as a fixed Pi schema. Review shows replacement, hidden contents, backup and unchanged-default notices. Runtime compatibility and provider connectivity belong to Pi. Unreadable or invalid source models can be skipped independently of other settings. Packaging, signing, live-device interoperability and assistive-technology validation need real platform evidence.
