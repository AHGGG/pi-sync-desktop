# Security and recovery

## Trust and authentication

Pasted SSH text is parsed as a small grammar: explicit `ssh user@host`, `-p`, and `-i`. It is never passed to a shell. ProxyCommand, jump hosts, forwarding, extra commands, substitutions and control characters are rejected. Aliases requiring arbitrary SSH config interpretation are not supported.

The connection uses libssh2. Existing OpenSSH known hosts and the app's trust file are checked. Unknown identities require comparing and explicitly accepting a SHA-256 fingerprint. Changed/unverifiable identities cannot be overridden inside the app. Verify and repair trust outside the app after investigating the change.

Agents and key files are preferred. Passwords/passphrases are held for the connection attempt, not written to profiles, command arguments or logs. The UI clears masked input and undo state. This does not claim that general-purpose GUI or SSH libraries can guarantee erasure of every allocator copy.

## Stored data

The OS application-config directory for `pi-sync-desktop` contains:

- `profiles.json`: names, host/user/port/key path, and Pi directory. No credential values.
- `known_hosts`: accepted host identities, not authentication credentials.
- `last-selection.json`: source/destination identifiers and selected configuration categories. No values, secrets, plans or remembered conflict decisions.

Plans, source configuration and model content remain in memory. A private temporary directory contains only the application's bundled JavaScript worker source, never device files. The directory is removed on normal shutdown.

## File writes

The worker rejects symlinked paths, paths inside Git repositories, oversized/malformed JSON and unsupported model schemas. It checks source fingerprints again before apply and target fingerprints before writes. A plan is consumed before applying and is not replayable after a connection failure.

Writes are individually staged and renamed, but a multi-file operation is not a distributed transaction. A process/device failure can leave some files applied. Follow the per-device outcome, inspect affected files, and prepare a new review rather than blindly retrying. Static symlink rejection and stale hashes do not claim immunity against a hostile local process racing directory replacement.

Recovery backups live in each target's `.harness-sync/backups` under its Pi agent directory. A successful operation retains one previous operation's backup, covering the files involved in that operation, not a complete historical snapshot of every Pi file. A partial failure preserves recovery sets and reports its backup path. Review backup contents before manual restoration; the app does not silently restore over subsequent edits.

## Configuration scope

The audited allowlist excludes device IDs, shell commands/paths, defaultProvider/defaultModel and permission policies. Target-only values are kept. Model-file replacement is explicit and validates the target default: an incoming custom default must still exist, or neither previous nor incoming custom configuration may touch the preserved built-in provider. Runtime availability and project/session overrides remain unverified.

Private models can include authentication data. They travel through private process pipes and SSH, not GitHub or the review UI. Review exposes hashes and warnings rather than their content. The public source downloads only allowlisted JSON files from one fixed repository at a verified commit identifier.

Permission-policy synchronization is unavailable until a separate schema, weakening analysis and explicit policy review are implemented. Existing policies remain unchanged.
