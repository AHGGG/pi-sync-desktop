# Sync backend

Dependency-free Node.js 20+ backend. Rust owns SSH authentication, host trust, profile storage, and UI. This directory contains no credentials and does not connect to live devices during tests.

Run `npm test --prefix backend` and `npm run check --prefix backend` from the repository root.

## Protocol

Keep `node backend/bridge.mjs` running with private stdin/stdout pipes. Each request and response occupies one JSON line. Never log protocol traffic: internal SSH events may carry source models, settings, or credentials embedded in user configuration. stderr is unused. The app should discard unneeded response buffers.

The private protocol envelope is capped at 48 MiB consistently across Node input, desktop pipes, and SSH request/response transport. This accommodates the combined audited documents, private models, custom files, base64 expansion, and metadata. Individual files remain capped at 4 MiB; custom selections additionally allow at most 256 files and 8 MiB aggregate bytes. These are bounded in-memory transfers, not streaming file copies.

Ordinary request IDs are string or number. Success is `{id, ok:true, ...result}`, failure is `{id, ok:false, error:{code}}`. Error codes never contain file contents. `ping` returns protocolVersion 1.

### Preview

```json
{"id":1,"op":"preview","source":{"type":"public","latest":true},"targets":[{"id":"laptop","type":"local","agentDir":"/home/example/.pi/agent"}],"items":["settings","subagents"],"fields":{"settings":["/theme","/packages"]}}
```

Sources:

- Latest public: `{type:"public",latest:true}` downloads only allowlisted JSON files from `AHGGG/my-pi-harness`, pinned to a single resolved commit. No downloaded code executes. Network failure is explicit, never a fallback to stale bundled configuration. The response includes `sourceRevision`.
- Explicit bundled checkout: `{type:"public",root:"/runtime/public-harness",revision:"1d8b174"}` reads `shared/*.json`. The UI must label this as the bundled revision rather than latest.
- Local/SSH device: `{id:"source-id",type:"local"|"ssh",agentDir:"..."}`. Source and destination local directories must not be in Git working trees or traverse symlinks. SSH ids select Rust-owned connection profiles.

Items are `settings`, `mcp`, `subagents`, `models`, and `path:<relative path>`. Public sources cannot provide models or custom paths. Optional `fields` restricts each selected document to allowlisted JSON-pointer leaves; omitted fields means all supported source leaves. Device-local settings never become source declarations. For a public source, selecting both `settings` and `mcp` stages `+builtin:mcp` as an ordinary reviewed `/extensions/builtin:mcp` settings change, matching the upstream `--with-mcp` behavior. If `fields.settings` is provided, it must include that pointer to select activation. Settings-only never stages activation. MCP-only copies MCP configuration without changing settings or enabling the extension; select settings too to review activation. Device-source settings retain the source activation directive only when MCP is also selected. Unknown/private MCP server definitions and unreviewed settings are not copied.

Response: `{planId,sourceRevision,targets:[{id,status,documents,models,files}]}`. A target status is `ready`, `conflicts`, or `blocked` with a safe `error`. Documents contain `changes` (`path,before,after`), `conflicts` (`path,reason,source,target,base`), and `kept` paths. Value cells are `{present:false}` or `{present:true,value:...}`. The bridge redacts recognized secrets and all MCP command/argument values. Models contain source/target hashes, a `targetInspected` flag, changed flag, and warnings; whole-file replacement requires review. No provider names, model IDs, or private values appear in UI responses. An unreadable, missing, invalid-UTF-8 or invalid-JSON source appears as `models.error`; choosing Target/Skip allows other selected documents to proceed. If the source is unavailable, the target model file is not inspected, and a null target hash does not assert that it is missing.

### Custom files and folders

`path:extensions` recursively selects files under the Pi root; `path:permissions.json` explicitly selects a standalone policy. No absolute mapping, deletion of target-only files, empty-directory creation, symlinks, nested Git repositories, credentials or hidden paths. Cross-platform case/Unicode aliases and reserved filenames are rejected. An included protected descendant fails the selection rather than silently disappearing; users can explicitly exclude its branch in Choose contents.

Each `files` row includes `path`, `sourceHash`, `targetHash`, `changed`, `conflict`, `sourceAvailable`, `before`, `after`, and an optional permission-policy warning. Text previews are bounded; binary and recognized sensitive contents are hidden. All file changes require an explicit choice, including new files. Resolve uses `fileChoices:{targetId:{relativePath:"source"|"target"|"skip"}}`. Target-only rows are unchanged and cannot authorize deletion. Source and target file/directory inventories are revalidated at apply; the worker also checks again after locking and immediately before each replacement.

### Resolve captured review

```json
{"id":2,"op":"resolve","planId":"...","resolutions":{"laptop":{"settings":{"/theme":"source","/packages":"target"}}},"modelChoices":{"laptop":"source"}}
```

Resolution values: `source`, `target`, `skip` (skip has the effect of target). Applies to both conflicts and automatic changes. This recomputes from the **captured** source and destination snapshots, never newly changed files. It consumes the old plan and returns a new plan ID/review. Unknown or stale choices fail closed. The UI must not generate an unreviewed fresh preview in its Apply handler.

### Reveal MCP command details

`{op:"reveal",planId,approved:true}` returns the same captured review with MCP rows unredacted and `commandDetailsRevealed:true`. Use only after an explicit local UI action warning that executable arguments may contain private values. Other documents remain secret-masked. Do not log, persist, or include revealed values in diagnostics. The backend records reveal on the exact captured plan; `resolve` inherits that record. A changed MCP command/argument requires both this record and `commandReviewApproved:true` on Apply. Keep/Skip does not require reveal. A client-supplied approval flag cannot bypass reveal.

### Apply

```json
{"id":3,"op":"apply","planId":"...","approved":true,"targetIds":["laptop"]}
```

Only explicit ready target ids are eligible. The immutable, process-local plan expires after 30 minutes, is consumed before any write, and cannot be replayed after an uncertain result. Source hashes/revision are rechecked; workers verify all target snapshot hashes before writing and immediately before replacement. Restarting the backend invalidates all previews.

Response: `{targets:[{id,status,files,backupPath}],partial}`. Status is `applied`, `unchanged`, or `failed`. Failures carry a safe error, `applied:[{file,verified}]`, `partial`, `backupPath`, `uncertain`, and `outcome:"inspect-and-preview-again"`. Peers are independent: offline/blocked devices do not silently receive writes or prevent explicit application to ready peers. There is no cross-device transaction or automatic retry/rollback.

`discard` with `planId` frees a captured preview.

### SSH transport

During preview/apply, the bridge may emit:

```json
{"type":"transport","transportId":"1","target":{"id":"server","type":"ssh","agentDir":"~/.pi/agent"},"request":{"op":"snapshot","agentDir":"~/.pi/agent"}}
```

Rust must service this while an ordinary request is pending, using its authenticated `SshTransport::run_worker` with this directory's **bundled** `worker.mjs` source and the request. Then send `{op:"transport-result",transportId:"1",response:<worker JSON>}` or `{op:"transport-result",transportId:"1",error:"SSH_FAILED"}`. The bridge handles transport replies outside its serial ordinary-request queue. Credentials stay in Rust. Target Node.js 20+ is required. Remote operations are contents, snapshot, export-models, review-models, and apply. Destination preview requests only model existence and hashes, never private bytes or configuration metadata. Full model bytes may be read from the source into the controller's private memory for JSON readability checks, but are sent to a destination only during explicitly confirmed apply. Resolve and reveal perform no target transfer. Exported models move only through these private pipes; they are never uploaded to the public repository, written to intermediate plan files, or interpreted as commands.

## Safety and backup policy

- Settings merge uses independently tracked allowlisted leaves and preserves device ID, tracking ID, shellPath, shellCommandPrefix, defaultProvider/defaultModel, and unrelated target data.
- Selected models replace the entire file byte-for-byte after source UTF-8/JSON syntax checks. All valid JSON roots and unknown fields are accepted without interpreting Pi schemas or executing credential references. Old destination bytes need not be valid JSON: they are backed up exactly before replacement. Existing default selections remain unchanged and may need manual adjustment in Pi. Runtime availability, project overrides, sessions, and provider credentials are **not** tested. Skipped models are excluded from source revalidation and destination hash guards; models-only copies do not read unselected settings or baseline files.
- Each apply creates a private backup of selected previous files plus a manifest. Only after every write verifies are older recognized backups removed. A failed/partial apply preserves recovery evidence, which may temporarily mean multiple backups. No automatic deletion of unknown backup content.
- Multi-file writes are not atomic. A cooperative lock, safe-path checks, expected hashes, per-file atomic rename and verification narrow but cannot eliminate races with non-cooperating writers.
- Ordinary settings sync preserves embedded permission rules. Explicit standalone policy paths require whole-file choices and a tool-access warning; no semantic weakening analysis is claimed. Authentication and credential paths remain blocked.

## Provenance

`worker.mjs`, `merge.mjs`, `documents.mjs` and their regression tests derive from `AHGGG/my-pi-harness` commit `1d8b174`. The MIT notice is retained in `UPSTREAM-LICENSE`. Changes add private source export and opaque file copying, independent skipping of source-model failures, post-success single-backup retention, and per-field skip/automatic-change decisions. Tests run against temporary local fixtures only.

## Content selection

`contents` accepts `{source, targets, paths, contentRules}` and returns per-device metadata `{id, role, entries, error?}` plus `limits`. Entries contain a relative path, `kind`, nullable byte `size`, and optional `blocked`/`suggestedExclude`/`omitted` metadata. No file content or hashes are read or returned. Traversal is bounded; partial errors preserve collected entries so the user can exclude a branch and refresh. Excluded subtrees retain their branch row but are not enumerated.

`preview` accepts optional `contentRules`, an object mapping relative file/subtree paths to booleans. Longest-prefix rules win, with inclusion as the default; names compare after NFKC normalization and lowercasing. These are exact path/subtree rules, not globs. Invalid aliases and non-boolean values are rejected. Explicit exclusions can name protected descendants; explicit inclusions cannot weaken protected-path validation. Rules are captured in the plan and forwarded to source/target snapshots and worker apply checks. Excluded files are not read, hashed, accounted, backed up or written. Included source/target changes still invalidate review.

Limits are unchanged: 4 MiB per included file, 8 MiB aggregate included custom bytes, and 256 included files per device. Limit failures carry content-free `errorDetail` metadata naming device, path, measured size/count and limit; source bridge failures place this in `error.detail`. Selecting an oversized log does not enable a large-file mode.
