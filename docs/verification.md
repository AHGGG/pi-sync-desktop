# Verification record

## Complete changed-file review · 2026-11-05

- Added explicit local full-file inspection for models, custom files, settings, MCP and subagents. The view is read-only; structured JSON includes the target-only values preserved by the actual merge, and labels undecided fields as provisional. Text and hexadecimal pages are complete, with no silent truncation. Opening, paging and closing the reader do not choose versions or apply changes.
- All 159 backend tests and 70 desktop tests passed, including approval/scope checks, target model hash binding, full custom-file text/hex reconstruction, malformed targets, unchanged default redaction, actual MCP merge output, pending decisions, cancellation, stale replies, failed-read recovery, and read-only native controls. The final workspace rerun passed all 25 core tests and the fixture diagnostic as well.
- The first workspace run hit the pre-existing fragmented askpass Broken pipe timing flake. Its isolated rerun and the unchanged full workspace rerun passed. No sync-core code was modified.
- Workspace/all-targets/all-features Clippy with warnings denied, formatting, the public-source scan, and real GPUI/WGPU full-file and entry-point renders passed. Captures use only synthetic data. The entry toolbar wraps at narrow widths; full-file paging and private-content reveal remain separate from sync confirmation.
- No real device connection or apply was performed. Native Windows/macOS behavior, explicit SSH/helper integration, accessibility tools, and remote CI for this change have not been run locally. Remote CI must be checked for the exact published commit.

## Independent version choices and navigation · 2026-11-05

- `cargo test --locked --workspace`: 63 desktop tests, 25 core tests and the fixture diagnostic passed locally. Updated real-pointer regressions cover Source → right arrow and Target → left arrow without changing choices, undecided/default choices across file boundaries, unresolved Pending items, unavailable-source backward navigation, disabled boundaries, and Enter/Space navigation. A new hover regression verifies content → arrow → content cues without advertising a choice over an arrow. Existing selection/scroll and held-pointer guards remain covered.
- All 143 backend tests, workspace/all-targets/all-features Clippy with warnings denied, formatting and the public-source scan passed. Native GPUI/WGPU navigation and next-file captures were inspected. The navigation fixture now selects Source through the pane, then activates the opposite arrow and asserts Source stays selected. No live connection or sync was performed.
- Opt-in helper-process and real-SSH checks are not part of this local UI run; the exact published commit is checked separately in the native CI matrix. Earlier records below describe the superseded select-first implementation.

## Hunk-to-item boundary navigation · 2026-11-05

- `cargo test --locked --workspace`: 62 desktop tests, 25 core tests and the fixture-diagnostic test passed locally on Linux. Six new production-view tests cover boundary-only item transitions, first/last entry for revisited items, select-first overlays on each item, Pending and duplicate paths across targets, opaque/equal/unavailable previews, busy/dialog/no-plan guards, visible offscreen reverse entry, and navigation during a held content/overlay/top-button press. No navigation test emits a backend command.
- Workspace/all-targets/all-features Clippy with warnings denied, formatting, public-source scan, and all 143 backend tests passed. The native GPUI/WGPU `--fixture-next-file` scenario verifies two hunk steps followed by an extra click to a new unchosen file. No live connection or sync was performed.
- The first local workspace run hit a Broken pipe in the existing 250 ms fragmented askpass test. An unchanged full-workspace rerun passed; transport code was not modified. Opt-in helper-process and real-SSH tests remain outside this local UI run. Remote CI is checked separately for the published commit.

## Content-pane selection and hunk navigation · 2026-11-05

- `cargo test --locked --workspace`: 56 desktop tests, 25 core tests and the standalone fixture-diagnostic test passed in Linux. The opt-in native-helper and real-SSH tests remained ignored in this run; transport code was not changed.
- Eight added production-view regressions cover visible hunk position and bounded navigation; select-first outer edges; blank-pane selection; keyboard, busy and unchanged-preview behavior; pure additions/deletions and unavailable/private previews; offscreen navigation and copy-selection preservation; native context expansion; and text/modifier/multiclick, gutter, outside-and-back drag, scrolling and replaced-preview input guards. The overlay regression also checks idle/side-hover visibility, keyboard-focus reveal, select-first activation, boundaries and unchanged content bounds.
- `node --test backend/tests/*.test.mjs`: all 143 tests passed. Formatting, `git diff --check`, warnings-denied workspace/all-targets/all-features Clippy, and the public-source credential-pattern scan passed.
- The production GPUI/WGPU renderer was exercised with the synthetic `--fixture-navigation` scenario: one edge click chooses target, the next advances to hunk 2 of 3. The generated native capture was visually inspected. No live device connection or sync was performed.
- These local checks do not establish Windows/macOS execution, assistive-technology behavior, or real-window manual input. No remote CI run or publication is claimed for the unpushed changes.

## System OpenSSH migration · 2026-11-05

- `cargo test --locked --workspace`: 25 core and 43 desktop tests passed in the Linux development environment. Coverage includes alias/profile migration, native prompt validation, custom-trust preflight rejection, cancelled-dialog cleanup, and consumed apply approvals.
- `node --test backend/tests/*.test.mjs`: 143 tests passed, including transport lifetime while native authentication is pending.
- `node scripts/test-askpass.mjs`: actual helper-process integration passed locally for password/passphrase, explicit host trust, cancellation, unknown prompts and parent-bridge cleanup.
- `PI_SYNC_TEST_SKIP_AGENT=1 scripts/test-ssh.sh`: isolated localhost OpenSSH integration passed with agent coverage explicitly skipped. The container denied AF_UNIX socket creation for the disposable `ssh-agent`; this is not a full integration pass. The dedicated synthetic-agent test is implemented and required by default.
- Formatting, warnings-denied workspace/all-targets Clippy, the public-source scan and Linux release build passed. A fresh production GPUI/WGPU onboarding render was visually inspected, including alias guidance and the selected-client label.
- The first migration CI run exposed a macOS accepted-socket blocking-mode difference; accepted askpass sockets now explicitly use blocking mode with the existing absolute deadline, covered by a fragmented-request regression.
- The Linux CI fixture now places its disposable keys under the runner’s private temp directory. A synthetic world-writable temp-parent reproduction confirmed the original failure was sshd StrictModes correctly rejecting that ancestor; server authentication policy was not relaxed.
- The native CI matrix must pass for the exact pushed commit. It includes real helper-process tests on Linux, macOS and Windows, plus the mandatory isolated SSH-agent fixture on Linux. A matrix definition is not evidence that those jobs passed.

The transport now uses system OpenSSH and a persistent Node worker. The Linux fixture is synthetic and localhost-only; it does not establish Windows/macOS execution, actual Bitwarden integration, password-server/MFA support, or connectivity to the user's devices. Keyboard-interactive/MFA is disabled by design. No user SSH credentials, server crypto settings or real Pi configuration were changed by this fixture. Earlier entries below describe their historical implementations and are not evidence for unrun migration checks.

## Saved-device workflow and custom-file update · 2026-11-05

- `node --test backend/tests/*.test.mjs`: 142 tests passed. New coverage includes recursive text/binary copies, explicit standalone-policy choices, protected descendants, symlinks, nested repositories, case/Unicode collisions, stale directory inventories, partial recovery, prototype-named files, transport bounds and large base64 payloads.
- `cargo test --locked --workspace`: 36 desktop and 9 core tests passed. Twelve native production-control tests cover first-run setup, saved selection, full-window review, cancel, no implicit apply, bulk file choices, repeat-click suppression, failed-versus-empty review and masked authentication.
- `scripts/test-ssh.sh`: isolated localhost OpenSSH integration passed, including a 7 MiB custom directory (>8 MiB serialized snapshot), remote exact-byte apply, prior-file backup and target-only preservation. Only disposable keys and synthetic fixtures were used.
- `cargo fmt --all -- --check`, warnings-denied workspace/all-targets Clippy and `node scripts/check-public.mjs` and final Linux release compilation passed.
- Real GPUI/WGPU screenshots of onboarding, the two-pane selection view and synthetic review were inspected. Device-icon assets are explicitly bundled and tested. See [capture details](screenshots/README.md).

These checks run on Linux with Rust 1.99.1 and Node 24.22.1, using existing native runtime libraries through temporary linker aliases and Swiftshader for offscreen rendering. They do not establish manual real-window use, remote Windows shell interoperability, real user-device connectivity, semantic permission-policy analysis, or signed installers. Native CI must be checked for the exact pushed commit. No user Pi harness/configuration or secrets were added to the repository.

## Private-model copy update · 2026-11-05

- `node --test backend/tests/*.test.mjs`: 78 tests passed. Coverage includes unknown model schemas and valid JSON roots, exact-byte copies, malformed destination backups, missing/unreadable/invalid source skipping, excluded-file independence, private preview output, and source/target/concurrent-change guards.
- `cargo test --locked --workspace`: 18 desktop and 9 core tests passed, including the embedded Node invalid-source Skip flow and native review controls.
- `scripts/test-ssh.sh`: isolated localhost OpenSSH integration passed.
- `cargo fmt --all -- --check`, warnings-denied workspace/all-targets Clippy, and `node scripts/check-public.mjs` passed.

These checks ran on Linux with Rust 1.99.1 and Node 24.22.1. Existing runtime X11 libraries were used through temporary local linker aliases, with supported `RUST_FONTCONFIG_DLOPEN=1`; no system configuration was changed. This update does not establish real Windows-to-Mac connectivity, provider availability, or new manual native-window coverage. Native CI results must be checked for the exact pushed commit.

## Initial implementation · 2026-11-04

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
