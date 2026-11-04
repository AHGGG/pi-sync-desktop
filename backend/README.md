# Sync backend

Dependency-free Node.js 20+ backend. Rust owns SSH authentication, host trust, profile storage, and UI. This directory contains no credentials and does not connect to live devices during tests.

Run `npm test --prefix backend` and `npm run check --prefix backend` from the repository root.

## Protocol

Keep `node backend/bridge.mjs` running with private stdin/stdout pipes. Each request and response occupies one JSON line. Never log protocol traffic: internal SSH events may carry source models, settings, or credentials embedded in user configuration. stderr is unused. The app should discard unneeded response buffers.

Ordinary request IDs are string or number. Success is `{id, ok:true, ...result}`, failure is `{id, ok:false, error:{code}}`. Error codes never contain file contents. `ping` returns protocolVersion 1.

### Preview

```json
{"id":1,"op":"preview","source":{"type":"public","latest":true},"targets":[{"id":"laptop","type":"local","agentDir":"/home/example/.pi/agent"}],"items":["settings","subagents"],"fields":{"settings":["/theme","/packages"]}}
```

Sources:

- Latest public: `{type:"public",latest:true}` downloads only allowlisted JSON files from `AHGGG/my-pi-harness`, pinned to a single resolved commit. No downloaded code executes. Network failure is explicit, never a fallback to stale bundled configuration. The response includes `sourceRevision`.
- Explicit bundled checkout: `{type:"public",root:"/runtime/public-harness",revision:"1d8b174"}` reads `shared/*.json`. The UI must label this as the bundled revision rather than latest.
- Local/SSH device: `{id:"source-id",type:"local"|"ssh",agentDir:"..."}`. Source and destination local directories must not be in Git working trees or traverse symlinks. SSH ids select Rust-owned connection profiles.

Items are `settings`, `mcp`, `subagents`, `models`. Public models are forbidden. Optional `fields` restricts each selected document to allowlisted JSON-pointer leaves; omitted fields means all supported source leaves. Device-local settings never become source declarations. For a public source, selecting both `settings` and `mcp` stages `+builtin:mcp` as an ordinary reviewed `/extensions/builtin:mcp` settings change, matching the upstream `--with-mcp` behavior. If `fields.settings` is provided, it must include that pointer to select activation. Settings-only never stages activation. MCP-only copies MCP configuration without changing settings or enabling the extension; select settings too to review activation. Device-source settings retain the source activation directive only when MCP is also selected. Unknown/private MCP server definitions and unreviewed settings are not copied.

Response: `{planId,sourceRevision,targets:[{id,status,documents,models}]}`. A target status is `ready`, `conflicts`, or `blocked` with a safe `error`. Documents contain `changes` (`path,before,after`), `conflicts` (`path,reason,source,target,base`), and `kept` paths. Value cells are `{present:false}` or `{present:true,value:...}`. The bridge redacts recognized secrets and all MCP command/argument values. Models contain source/target hashes, `sourceSummary`/`targetSummary` with provider names and model IDs only, changed flag, and warnings; whole-file replacement requires review. Raw private models are never in ordinary UI responses. A compatibility blocker appears as `models.error`; choosing Target/Skip for models allows other selected documents to proceed.

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

Rust must service this while an ordinary request is pending, using its authenticated `SshTransport::run_worker` with this directory's **bundled** `worker.mjs` source and the request. Then send `{op:"transport-result",transportId:"1",response:<worker JSON>}` or `{op:"transport-result",transportId:"1",error:"SSH_FAILED"}`. The bridge handles transport replies outside its serial ordinary-request queue. Credentials stay in Rust. Target Node.js 20+ is required. Remote operations are snapshot, export-models, check-models, apply. Destination preview `check-models` accepts only `modelSummary:{providers:{name:{models:[{id}]}}}` and snapshot hashes, never full `modelsBase64`. Its strict schema rejects credentials, endpoints, headers, labels, and write payloads. `snapshot` can request `includeModelSummary:true` to receive this metadata alongside model hashes. Full model bytes may be read from the source into the controller's private memory for schema validation, but are sent to a destination only during explicitly confirmed `apply`. Resolve and reveal perform no target transfer. Exported models move only through these private pipes; they are never uploaded to the public repository, written to intermediate plan files, or interpreted as commands.

## Safety and backup policy

- Settings merge uses independently tracked allowlisted leaves and preserves device ID, tracking ID, shellPath, shellCommandPrefix, defaultProvider/defaultModel, and unrelated target data.
- Selected models replace the entire file, after schema and configured-default compatibility checks. Explicit custom defaults must remain defined. A built-in/extension default is provably unaffected when neither old nor new custom file touches its provider. Missing/ambiguous defaults or removing a custom default remain blocked. Runtime availability, project overrides, sessions, and provider credentials are **not** tested.
- Each apply creates a private backup of selected previous files plus a manifest. Only after every write verifies are older recognized backups removed. A failed/partial apply preserves recovery evidence, which may temporarily mean multiple backups. No automatic deletion of unknown backup content.
- Multi-file writes are not atomic. A cooperative lock, safe-path checks, expected hashes, per-file atomic rename and verification narrow but cannot eliminate races with non-cooperating writers.
- Permission-policy syncing is intentionally unsupported and rejects selection. A separate reviewed schema and explicit permission approval are needed; ordinary settings sync does not copy permission rules. The permission-system package may be part of reviewed packages, but that does not copy a policy.

## Provenance

`worker.mjs`, `merge.mjs`, `documents.mjs` and their regression tests derive from `AHGGG/my-pi-harness` commit `1d8b174`. The MIT notice is retained in `UPSTREAM-LICENSE`. Changes add private source export, read-only model compatibility checks, conservative built-in non-interference handling, post-success single-backup retention, and per-field skip/automatic-change decisions. Tests run against temporary local fixtures only.
