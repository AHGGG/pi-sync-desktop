# Security and recovery

## Trust and authentication

Pasted SSH text is parsed as a small grammar: `ssh [user@]host-or-alias`, `-p`, and `-i`. It is never passed to a local shell. Extra options, remote commands, substitutions and control characters are rejected. For new profiles, omitted user/port values are resolved by OpenSSH configuration; legacy profiles retain their explicit values.

Connections use the installed system OpenSSH client, not libssh2. OpenSSH reads the user's trusted local configuration, including aliases, identity files and agent settings. The app overrides forwarding (including agent/X11/tunnels and GSSAPI credential delegation), proxy/jump hosts, local commands, remote commands, multiplexing, TTY allocation, session suppression, detached/background startup and keyboard-interactive authentication. These restrictions do **not** sandbox configuration parsing: `Match exec` may execute local commands even during the `ssh -G` preflight. Only use configuration you trust.

The preflight blocks custom `UserKnownHostsFile` and non-disabled `KnownHostsCommand` before network/authentication, because the app cannot safely combine those policies with its own trust file. They are not silently ignored. With the standard configuration, OpenSSH checks `~/.ssh/known_hosts`, `~/.ssh/known_hosts2`, the app's private trust file and native global trust. New accepted hosts are recorded in the app file. Unknown identities require comparing and explicitly accepting a SHA-256 fingerprint in the native GUI. Changed/revoked keys and unverifiable identities fail closed; there is no in-app override. Investigate and repair trust outside the app. DNS host-key verification and automatic host-key updates are disabled.

Configured agents and identity files are handled by OpenSSH. The GUI process inherits the agent environment at launch; restart it after changing that environment. Bitwarden may work as the configured native agent, with unlock/approval in Bitwarden itself, but actual Bitwarden interoperability remains unverified. No agent secrets are imported or stored by Pi Sync. Adding keys to an agent and Apple Keychain integration are disabled, so entering a passphrase does not persist it through those SSH settings; users normally relying on Keychain may see a passphrase prompt instead. Keyboard-interactive/MFA is deliberately disabled rather than translating arbitrary server challenges into password dialogs.

Password, key-passphrase and unknown-host requests use a native GUI askpass bridge. A random per-process capability authenticates bounded loopback requests; credentials and capability are not written to disk. Secrets travel through private process pipes and the local authenticated bridge, not profiles, command arguments or logs. The UI clears masked input and undo state. Authentication dialogs have a two-minute limit and are dismissed on cancellation; busy operations have a Cancel operation control. This does not guarantee erasure of every allocator copy or protect against a compromised process running as the same user.

Each SSH connection runs one persistent remote Node worker through a fixed launcher, with requests sent over private stdin/stdout rather than interpolated into command text. Protocol frames are bounded at 48 MiB and diagnostic capture at 64 KiB; excess stderr is drained without being retained. Only selected application-authored errors reach the UI, not raw server banners. Timeouts, cancellation and incomplete/invalid responses make the session unusable. An uncertain apply is never automatically replayed: inspect the target and prepare a fresh review, because some writes may already have happened.

## Stored data

The OS application-config directory for `pi-sync-desktop` contains:

- `profiles.json`: names, host/user/port/key path, and Pi directory. No credential values.
- `known_hosts`: accepted host identities, not authentication credentials.
- `last-selection.json`: source/destination identifiers and selected configuration categories and custom relative paths. No values, secrets, plans or remembered conflict decisions.

Plans, source configuration and model content remain in memory. A private temporary directory contains only the application's bundled JavaScript worker source, never device files. The directory is removed on normal shutdown.

## File writes

The worker rejects symlinked paths, paths inside Git repositories, nonregular files and oversized files. Selected shared documents must be valid JSON objects. Source models must be readable UTF-8 JSON, but no Pi model schema is imposed. Destination model bytes are hashed and backed up without parsing, so a malformed old file can be safely replaced. It checks source fingerprints again before apply and target fingerprints before writes. A plan is consumed before applying and is not replayable after a connection failure.

Writes are individually staged and renamed, but a multi-file operation is not a distributed transaction. A process/device failure can leave some files applied. Follow the per-device outcome, inspect affected files, and prepare a new review rather than blindly retrying. Static symlink rejection and stale hashes do not claim immunity against a hostile local process racing directory replacement.

Recovery backups live in each target's `.harness-sync/backups` under its Pi agent directory. A successful operation retains one previous operation's backup, covering the files involved in that operation, not a complete historical snapshot of every Pi file. A partial failure preserves recovery sets and reports its backup path. Review backup contents before manual restoration; the app does not silently restore over subsequent edits.

## Configuration scope

The audited allowlist excludes device IDs, shell commands/paths, defaultProvider/defaultModel and permission policies. Target-only values are kept. Model-file replacement is explicit and byte-for-byte. Default provider/model settings are preserved, not compatibility-checked; replacing custom definitions may require selecting a model in Pi afterward. Runtime availability and project/session overrides remain unverified. A source model read/JSON failure is a skippable review row, not a blocker for unrelated settings. Models skipped at apply are excluded from source revalidation and destination stale checks. Models-only sync does not parse unselected settings or baseline files.

Private models can include authentication data. They travel through private process pipes and SSH, not GitHub or the review UI. Review exposes hashes and warnings rather than their content. The public source downloads only allowlisted JSON files from one fixed repository at a verified commit identifier.

Standalone permission files can be explicitly selected as custom paths. Review labels policy changes as potentially granting/removing tool access, shows content (or hidden-content notice and SHA-256), and requires an explicit Source/Target/Skip decision followed by final confirmation. Policies embedded in built-in settings remain unchanged. This is whole-file replacement, not semantic policy validation or weakening analysis.

Custom files are limited to the configured Pi directory and preserve target-only files. Both recursive source and target inventories are captured and revalidated, including directory entries. Symlinks, Git repositories (including nested target repositories), authentication/credential names, dot-paths, nonportable Windows characters, and case/Unicode-normalized aliases are rejected. Protected descendants cause a visible blocker, not silent exclusion. Whole-file copies can contain executable extension code or private data; binary/sensitive content is hidden and text previews are truncated. Hashes identify the complete bytes being approved. No detection heuristic can guarantee that arbitrary selected files are free of secrets.

Custom recovery bytes use hashed flat backup filenames; the backup manifest maps each `path:<relative path>` to its original fingerprint. Files are private to the local process/SSH transfer, never checked into this repository. The private JSON protocol is bounded at 48 MiB; custom content separately permits 256 files, 4 MiB per file, and 8 MiB aggregate. Empty folders and file metadata/executable permissions are not copied.
