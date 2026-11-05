import fs from "node:fs/promises";
import { latestPublic } from "./public-source.mjs";
import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { handle, decodeModels, validateRelativePath, validatePathCollisions } from "./worker.mjs";
import { SCHEMAS, validateDocument, mergeDocuments } from "./documents.mjs";
import { cell, object, same, mergeDocument } from "./merge.mjs";
const fail = (code) => {
  throw new Error(code);
};
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const checked = (r) => {
  if (!r?.ok) {
    const error = new Error(r?.error?.code || "TARGET_FAILED");
    error.workerResult = r;
    throw error;
  }
  return r;
};
export const safeError = (e) =>
  /^[A-Z_]{3,80}$/.test(e?.message) ? e.message : "VALIDATION_FAILED";
const hashes = (s) =>
  Object.fromEntries(
    Object.keys(s)
      .filter((k) => object(s[k]) && "hash" in s[k])
      .map((k) => [k, s[k].hash]),
  );
function target(t) {
  if (
    !object(t) ||
    !["local", "ssh"].includes(t.type) ||
    typeof t.id !== "string" ||
    !/^[a-zA-Z0-9_-]{1,60}$/.test(t.id) ||
    typeof t.agentDir !== "string" ||
    !t.agentDir ||
    /[\0\r\n]/.test(t.agentDir)
  )
    fail("INVALID_TARGET");
  return structuredClone(t);
}
function project(name, value, fields) {
  const result = {};
  for (const p of fields ?? Object.keys(SCHEMAS[name])) {
    const c = cell(value ?? {}, p);
    if (!c.present) continue;
    if (p === "/extensions/builtin:mcp") {
      result.extensions = c.value;
      continue;
    }
    const parts = p.slice(1).split("/");
    let node = result;
    for (const k of parts.slice(0, -1)) node = node[k] ??= {};
    node[parts.at(-1)] = c.value;
  }
  validateDocument(name, result);
  return result;
}
function fileDisplay(record) {
  if (!record) return null;
  const bytes = Buffer.from(record.base64, 'base64');
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    if (/[\x00-\x08\x0e-\x1f]/.test(text)) return '[Binary file: content hidden]';
    // Private protocol only. Credentials and command strings are never exposed implicitly.
    if (/(?:bearer\s|sk-[A-Za-z0-9]|(?:api[_-]?key|token|password|secret|authorization|credential)\s*["']?\s*[:=]|https?:\/\/[^/\s]*@|["']?(?:command|args)["']?\s*[:=])/i.test(text)) return '[Sensitive content hidden]';
    return text.slice(0, 12000) + (text.length > 12000 ? '\n[Truncated]' : '');
  } catch { return '[Binary file: content hidden]'; }
}
// Explicit private review is lossless. Decode strictly, preserve BOMs, and use hex
// for both sides if either side contains binary data. Page boundaries never split
// a surrogate pair; prefer complete lines where possible.
function reviewPages(before, after) {
  let texts;
  try {
    texts = [before, after].map(bytes => {
      const text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
      if (/[\x00-\x08\x0e-\x1f\x7f]/.test(text)) throw new Error('BINARY');
      return text;
    });
  } catch { texts = null; }
  const split = text => {
    const pages = [];
    for (let start = 0; start < text.length;) {
      let end = Math.min(start + 12000, text.length);
      if (end < text.length) {
        if (text.charCodeAt(end - 1) >= 0xd800 && text.charCodeAt(end - 1) <= 0xdbff) end--;
        const newline = text.lastIndexOf('\n', end - 1);
        if (newline >= start) end = newline + 1;
      }
      pages.push(text.slice(start, end));
      start = end;
    }
    return pages;
  };
  const hex = bytes => {
    const pages = [];
    for (let start = 0; start < bytes.length; start += 4096)
      pages.push(Array.from({ length: Math.ceil(Math.min(4096, bytes.length - start) / 16) }, (_, line) => {
        const offset = start + line * 16;
        const content = bytes.subarray(offset, Math.min(offset + 16, bytes.length)).toString('hex').match(/.{1,2}/g).join(' ');
        return offset.toString(16).padStart(8, '0') + ': ' + content;
      }).join('\n') + '\n');
    return pages;
  };
  const pages = texts ? texts.map(split) : [before, after].map(hex);
  return { format: texts ? 'text' : 'hex', before: pages[0], after: pages[1],
    pageCount: Math.max(1, ...pages.map(p => p.length)) };
}
// Pretty printing deeply nested target-only data can amplify a small file into
// gigabytes of indentation. First serialize compactly, then conservatively bound
// indentation expansion before attempting pretty JSON. Excessive recursion fails
// with a content-free error rather than exposing private values in diagnostics.
function reviewJson(value) {
  let compact;
  try { compact = JSON.stringify(value); }
  catch { fail('JSON_REVIEW_TOO_DEEP'); }
  let depth = 0, maximum = 0, quoted = false, escaped = false;
  for (const char of compact) {
    if (quoted) {
      if (escaped) escaped = false;
      else if (char === '\\') escaped = true;
      else if (char === '"') quoted = false;
    } else if (char === '"') quoted = true;
    else if (char === '{' || char === '[') maximum = Math.max(maximum, ++depth);
    else if (char === '}' || char === ']') depth--;
  }
  // Every original character contributes at most one newline and maximum-depth
  // indentation. Fall back to complete compact JSON, never truncate.
  return (compact.length * (maximum * 2 + 3) <= 4 * 1024 * 1024
    ? JSON.stringify(value, null, 2) : compact) + '\n';
}
function reviewFiles(source, target, choices = {}) {
  const names = [...new Set([...Object.keys(source), ...Object.keys(target)])].sort();
  validatePathCollisions(names, true);
  if (!object(choices) || Object.entries(choices).some(([rel, choice]) => !names.includes(rel) || !['source', 'target', 'skip'].includes(choice))) fail('INVALID_FILE_CHOICE');
  return names.map(rel => {
    const src = Object.hasOwn(source, rel) ? source[rel] : undefined;
    const dst = Object.hasOwn(target, rel) ? target[rel] : undefined;
    const choice = Object.hasOwn(choices, rel) ? choices[rel] : undefined;
    const differs = !!src && src.hash !== (dst?.hash ?? null);
    const sensitive = /permission|policy|policies/i.test(rel);
    return { path: rel, sourceHash: src?.hash ?? null, targetHash: dst?.hash ?? null,
      changed: differs && choice !== 'target' && choice !== 'skip',
      conflict: differs && choice === undefined, sourceAvailable: !!src,
      before: fileDisplay(dst), after: fileDisplay(src), sensitive,
      ...(sensitive ? { warning: 'PERMISSION_POLICY_CHANGE' } : {}), ...(choice ? { choice } : {}) };
  });
}
export class SyncEngine {
  #plans = new Map();
  constructor(remote) {
    this.remote = remote;
  }
  async call(t, request) {
    return checked(
      await (t.type === "local"
        ? handle({ ...request, agentDir: t.agentDir })
        : this.remote
          ? this.remote(t, { ...request, agentDir: t.agentDir })
          : Promise.reject(new Error("SSH_UNAVAILABLE"))),
    );
  }
  async source(input) {
    const documents = input.items.filter((k) => k !== "models" && !k.startsWith("path:")),
      values = {},
      fingerprints = {};
    let models, modelError;
    let customFiles = {}, customDirectories = [];
    const paths = input.items.filter(k => k.startsWith("path:")).map(k => validateRelativePath(k.slice(5)));
    if (input.source.type === "public") {
      if (paths.length) fail("PUBLIC_CUSTOM_PATHS_FORBIDDEN");
      if (input.items.includes("models")) fail("PUBLIC_MODELS_FORBIDDEN");
      if (input.source.latest === true) {
        const latest = await latestPublic(documents);
        fingerprints.revision = latest.revision;
        for (const name of documents) {
          validateDocument(name, latest.values[name]);
          values[name] = latest.values[name];
          fingerprints[name] = sha(JSON.stringify(values[name]));
        }
      } else {
        if (typeof input.source.root !== "string") fail("INVALID_SOURCE");
        for (const name of documents) {
          const file = path.join(input.source.root, "shared", `${name}.json`);
          const stat = await fs.lstat(file);
          if (!stat.isFile() || stat.size > 4 * 1024 * 1024)
            fail("INVALID_PUBLIC_FILE");
          const bytes = await fs.readFile(file);
          values[name] = JSON.parse(bytes);
          validateDocument(name, values[name]);
          fingerprints[name] = sha(bytes);
        }
      }
    } else {
      const source = target(input.source);
      const snapshot = documents.length || paths.length
        ? await this.call(source, {
            op: "snapshot",
            documents,
            includeModels: false,
            paths, requirePaths: true,
          })
        : {};
      customFiles = snapshot.customFiles ?? {};
      customDirectories = snapshot.customDirectories ?? [];
      if (paths.length) fingerprints.customDirectories = customDirectories;
      if (paths.length) fingerprints.customFiles = Object.fromEntries(Object.entries(customFiles).map(([rel, r]) => [rel, r.hash]));
      for (const name of documents) {
        values[name] = project(
          name,
          snapshot[name].value,
          input.fields?.[name],
        );
        fingerprints[name] = snapshot[name].hash;
      }
      if (input.items.includes("models")) {
        try {
          const exported = await this.call(source, { op: "export-models" });
          const bytes = decodeModels(exported.modelsBase64);
          if (sha(bytes) !== exported.hash) fail("SOURCE_CHANGED");
          models = exported.modelsBase64;
          fingerprints.models = exported.hash;
        } catch (e) {
          const code = safeError(e);
          modelError = ({
            IO_ERROR: "SOURCE_MODELS_UNREADABLE",
            PERMISSION_DENIED: "SOURCE_MODELS_UNREADABLE",
            INVALID_JSON: "SOURCE_MODELS_INVALID_JSON",
            FILE_TOO_LARGE: "SOURCE_MODELS_TOO_LARGE",
            SYMLINK_REJECTED: "SOURCE_MODELS_SYMLINK_REJECTED",
            NOT_REGULAR_FILE: "SOURCE_MODELS_NOT_REGULAR_FILE",
          })[code] ?? code;
        }
      }
    }
    // Match the public harness --with-mcp behavior, but stage activation as a
    // normal per-field review decision. MCP-only never broadens settings scope.
    if (
      input.source.type === "public" &&
      values.settings &&
      input.items.includes("mcp")
    )
      values.settings.extensions = ["+builtin:mcp"];
    for (const name of documents)
      if (input.fields?.[name])
        values[name] = project(name, values[name], input.fields[name]);
    // MCP activation is opt-in; settings-only must not enable the extension.
    if (values.settings && !input.items.includes("mcp"))
      delete values.settings.extensions;
    return { values, fingerprints, models, modelError, customFiles, customDirectories };
  }
  async preview(input) {
    if (
      !object(input) ||
      !object(input.source) ||
      !Array.isArray(input.items) ||
      !input.items.length ||
      new Set(input.items).size !== input.items.length ||
      input.items.some(
        (k) => typeof k !== "string" || (!["settings", "mcp", "subagents", "models"].includes(k) && !k.startsWith("path:")),
      )
    )
      fail("INVALID_SELECTION");
    if (!Array.isArray(input.targets) || !input.targets.length)
      fail("TARGET_REQUIRED");
    input = structuredClone(input);
    input.targets = input.targets.map(target);
    if (new Set(input.targets.map((t) => t.id)).size !== input.targets.length)
      fail("DUPLICATE_TARGET");
    const locations = input.targets.map(
      (t) =>
        `${t.type}:${t.type === "local" ? "" : (t.ssh ?? t.id)}:${t.type === "local" ? path.resolve(t.agentDir) : t.agentDir}`,
    );
    if (new Set(locations).size !== locations.length) fail("DUPLICATE_TARGET");
    if (
      input.source.type !== "public" &&
      input.targets.some(
        (t) =>
          t.type === input.source.type &&
          t.agentDir === input.source.agentDir &&
          (t.type === "local" || t.id === input.source.id),
      )
    )
      fail("SOURCE_IS_TARGET");
    if (
      input.resolutions &&
      (!object(input.resolutions) ||
        Object.keys(input.resolutions).some(
          (id) => !input.targets.some((t) => t.id === id),
        ))
    )
      fail("UNKNOWN_RESOLUTION_TARGET");
    if (
      input.fields &&
      (!object(input.fields) ||
        Object.entries(input.fields).some(
          ([name, fields]) =>
            !input.items.includes(name) ||
            !Object.hasOwn(SCHEMAS, name) ||
            !Array.isArray(fields) ||
            fields.some((p) => !Object.hasOwn(SCHEMAS[name], p)),
        ))
    )
      fail("INVALID_FIELDS");
    const source = await this.source(input),
      prepared = [],
      reviews = [];
    for (const t of input.targets) {
      try {
        const snapshot = await this.call(t, {
          op: "snapshot",
          documents: Object.keys(source.values),
          includeModels: !!source.models,
          paths: input.items.filter(k => k.startsWith("path:")).map(k => k.slice(5)),
        });
        const merged = mergeDocuments(
          source.values,
          { state: { exists: false, value: null }, ...snapshot },
          input.resolutions?.[t.id],
        );
        const documents = Object.fromEntries(
          Object.entries(merged.documents).map(([n, d]) => [
            n,
            { changes: d.changes, conflicts: d.conflicts, kept: d.kept },
          ]),
        );
        const models = input.items.includes("models")
          ? {
              changed:
                !!source.modelError ||
                source.fingerprints.models !== snapshot.models?.hash,
              sourceHash: source.fingerprints.models ?? null,
              targetHash: snapshot.models?.hash ?? null,
              targetInspected: !!source.models,
              ...(source.modelError ? { error: source.modelError } : {}),
              warnings: [
                "PRIVATE_FILE_REPLACEMENT",
                "DEFAULT_SELECTION_PRESERVED",
                "RUNTIME_PROVIDER_NOT_TESTED",
              ],
            }
          : null;
        validatePathCollisions([...Object.keys(source.customFiles), ...source.customDirectories, ...Object.keys(snapshot.customFiles ?? {}), ...(snapshot.customDirectories ?? [])]);
        if (Object.keys(source.customFiles).some(rel => (snapshot.customDirectories ?? []).includes(rel)) || Object.keys(snapshot.customFiles ?? {}).some(rel => source.customDirectories.includes(rel))) fail("CUSTOM_PATH_COLLISION");
        const files = reviewFiles(source.customFiles, snapshot.customFiles ?? {});
        const status =
          files.some(f => f.conflict) ||
          Object.values(documents).some((d) => d.conflicts.length) ||
          (models?.changed && models.error)
            ? "conflicts"
            : "ready";
        reviews.push({ id: t.id, status, documents, models, files });
        prepared.push({ target: t, snapshot, merged, status, models, files });
      } catch (e) {
        reviews.push({ id: t.id, status: "blocked", error: safeError(e) });
      }
    }
    const planId = randomUUID();
    // A bounded process-local cache avoids storing private configuration in the repository or on disk.
    if (this.#plans.size >= 20)
      this.#plans.delete(this.#plans.keys().next().value);
    this.#plans.set(planId, {
      input,
      source,
      prepared,
      reviews,
      created: Date.now(),
    });
    return {
      planId,
      targets: structuredClone(reviews),
      sourceRevision:
        source.fingerprints.revision ?? input.source.revision ?? null,
    };
  }
  resolve({ planId, resolutions = {}, modelChoices = {}, fileChoices = {} }) {
    const plan = this.#plans.get(planId);
    if (!plan) fail("PLAN_NOT_FOUND");
    if (
      !object(resolutions) ||
      !object(modelChoices) ||
      !object(fileChoices) ||
      [...Object.keys(resolutions), ...Object.keys(modelChoices), ...Object.keys(fileChoices)].some(
        (id) => !plan.prepared.some((p) => p.target.id === id),
      )
    )
      fail("UNKNOWN_RESOLUTION_TARGET");
    resolutions = {
      ...plan.input.resolutions,
      ...plan.resolutions,
      ...resolutions,
    };
    modelChoices = { ...plan.modelChoices, ...modelChoices };
    if (Object.values(fileChoices).some(choices => !object(choices))) fail("INVALID_FILE_CHOICE");
    fileChoices = Object.fromEntries(plan.prepared.map(p => [p.target.id, { ...plan.fileChoices?.[p.target.id], ...fileChoices[p.target.id] }]));
    const prepared = plan.prepared.map((item) => {
      const merged = mergeDocuments(
        plan.source.values,
        { state: { exists: false, value: null }, ...item.snapshot },
        resolutions[item.target.id] ?? plan.input.resolutions?.[item.target.id],
      );
      const files = reviewFiles(plan.source.customFiles, item.snapshot.customFiles ?? {}, fileChoices[item.target.id]);
      const choice = modelChoices[item.target.id];
      if (
        choice !== undefined &&
        !["source", "target", "skip"].includes(choice)
      )
        fail("INVALID_MODEL_CHOICE");
      const models = item.models
        ? {
            ...item.models,
            changed:
              (!!item.models.error ||
                item.models.sourceHash !== item.models.targetHash) &&
              choice !== "target" &&
              choice !== "skip",
          }
        : null;
      return {
        ...item,
        merged,
        models,
        files,
        status:
          files.some(f => f.conflict) ||
          Object.values(merged.documents).some((d) => d.conflicts.length) ||
          (models?.changed && models.error)
            ? "conflicts"
            : "ready",
      };
    });
    const reviews = plan.reviews.map((review) => {
      const item = prepared.find((p) => p.target.id === review.id);
      if (!item) return review;
      return {
        id: review.id,
        status: item.status,
        models: item.models,
        files: item.files,
        documents: Object.fromEntries(
          Object.entries(item.merged.documents).map(([n, d]) => [
            n,
            { changes: d.changes, conflicts: d.conflicts, kept: d.kept },
          ]),
        ),
      };
    });
    const next = randomUUID();
    this.#plans.delete(planId);
    this.#plans.set(next, {
      ...plan,
      prepared,
      reviews,
      resolutions,
      modelChoices,
      fileChoices,
    });
    return {
      planId: next,
      targets: structuredClone(reviews),
      sourceRevision:
        plan.source.fingerprints.revision ?? plan.input.source.revision ?? null,
    };
  }
  reveal({ planId, approved }) {
    if (approved !== true) fail("APPROVAL_REQUIRED");
    const plan = this.#plans.get(planId);
    if (!plan) fail("PLAN_NOT_FOUND");
    plan.commandsRevealed = true;
    return {
      planId,
      targets: structuredClone(plan.reviews),
      sourceRevision:
        plan.source.fingerprints.revision ?? plan.input.source.revision ?? null,
      commandDetailsRevealed: true,
    };
  }
  async reviewFile({ planId, targetId, document, path: relativePath, page, approved, resolutions = {} }) {
    if (approved !== true) fail('APPROVAL_REQUIRED');
    const plan = this.#plans.get(planId);
    if (!plan) fail('PLAN_NOT_FOUND');
    const checkCurrent = () => {
      if (this.#plans.get(planId) !== plan) fail('PLAN_NOT_FOUND');
      if (Date.now() - plan.created > 30 * 60 * 1000) {
        this.#plans.delete(planId);
        fail('PLAN_EXPIRED');
      }
    };
    checkCurrent();
    if (!Number.isSafeInteger(page) || page < 0) fail('INVALID_REVIEW_PAGE');
    const item = plan.prepared.find(p => p.target.id === targetId);
    if (!item) fail('INVALID_REVIEW_TARGET');
    if (!Object.hasOwn(SCHEMAS, document) && (!object(resolutions) || Object.keys(resolutions).length))
      fail('INVALID_REVIEW_SELECTION');
    let before, after, unresolved;
    if (document === 'models') {
      if (relativePath !== '/' || !plan.input.items.includes('models') || !item.models || item.models.sourceHash === item.models.targetHash)
        fail('INVALID_REVIEW_SELECTION');
      if (item.models.error || !plan.source.models) fail('SOURCE_MODELS_UNAVAILABLE');
      if (!item.modelReviewPages) {
        const exported = await this.call(item.target, {
          op: 'review-models', approved: true, expectedHash: item.models.targetHash,
        });
        checkCurrent();
        if (typeof exported.modelsBase64 !== 'string') fail('INVALID_MODELS_ENCODING');
        before = Buffer.from(exported.modelsBase64, 'base64');
        if (exported.hash !== item.models.targetHash ||
            (exported.hash === null ? before.length !== 0 : sha(before) !== exported.hash))
          fail('TARGET_CHANGED');
        after = Buffer.from(plan.source.models, 'base64');
        item.modelReviewPages = reviewPages(before, after);
      }
    } else if (document === 'files') {
      validateRelativePath(relativePath);
      const row = item.files.find(f => f.path === relativePath && f.sourceAvailable && f.sourceHash !== f.targetHash);
      if (!row || !Object.hasOwn(plan.source.customFiles, relativePath)) fail('INVALID_REVIEW_SELECTION');
      before = Buffer.from(item.snapshot.customFiles?.[relativePath]?.base64 ?? '', 'base64');
      after = Buffer.from(plan.source.customFiles[relativePath].base64, 'base64');
    } else if (Object.hasOwn(SCHEMAS, document)) {
      if (relativePath !== '/' || !plan.input.items.includes(document) || !item.merged.documents[document])
        fail('INVALID_REVIEW_SELECTION');
      const source = plan.source.values[document];
      const target = item.snapshot[document].value ?? {};
      const baseline = item.snapshot.state?.value?.documents?.[document] ?? null;
      const merge = choices => mergeDocument(source, target, baseline, choices,
        SCHEMAS[document], value => validateDocument(document, value));
      const initial = merge({});
      const selectable = new Set([...initial.changes, ...initial.conflicts].map(row => row.path));
      if (!selectable.size || !object(resolutions) || Object.entries(resolutions).some(([key, choice]) =>
        !selectable.has(key) || !['source', 'target', 'skip'].includes(choice))) fail('INVALID_REVIEW_SELECTION');
      const captured = plan.resolutions?.[targetId]?.[document] ?? plan.input.resolutions?.[targetId]?.[document] ?? {};
      const proposed = merge({ ...captured, ...resolutions });
      unresolved = proposed.conflicts.length;
      before = Buffer.from(item.snapshot[document].exists ? reviewJson(target) : '');
      after = Buffer.from(reviewJson(proposed.result));
    } else fail('INVALID_REVIEW_SELECTION');
    const pages = document === 'models' ? item.modelReviewPages : reviewPages(before, after);
    if (page >= pages.pageCount) fail('INVALID_REVIEW_PAGE');
    return { planId, targetId, document, path: relativePath, page,
      pageCount: pages.pageCount, format: unresolved === undefined ? pages.format : 'json',
      ...(unresolved === undefined ? {} : { unresolved }),
      before: pages.before[page] ?? '', after: pages.after[page] ?? '' };
  }
  async apply({ planId, approved, targetIds, commandReviewApproved }) {
    if (approved !== true) fail("APPROVAL_REQUIRED");
    const plan = this.#plans.get(planId);
    if (!plan) fail("PLAN_NOT_FOUND");
    if (
      !Array.isArray(targetIds) ||
      !targetIds.length ||
      new Set(targetIds).size !== targetIds.length ||
      targetIds.some(
        (id) =>
          !plan.prepared.some(
            (p) => p.target.id === id && p.status === "ready",
          ),
      )
    )
      fail("TARGET_NOT_READY");
    const changesCommands = plan.prepared.some(
      (item) =>
        targetIds.includes(item.target.id) &&
        item.merged.documents.mcp?.changes.some((change) =>
          /\/(command|args)$/.test(change.path),
        ),
    );
    if (
      changesCommands &&
      (plan.commandsRevealed !== true || commandReviewApproved !== true)
    )
      fail("COMMAND_REVIEW_REQUIRED");
    this.#plans.delete(planId); // Never replay uncertain or partially applied operations.
    if (Date.now() - plan.created > 30 * 60 * 1000) fail("PLAN_EXPIRED");
    // Skipped models are outside this apply's scope, including source revalidation
    // and destination stale guards. They cannot block unrelated settings.
    const includeModels = plan.prepared.some(
      (p) => targetIds.includes(p.target.id) && p.models?.changed,
    );
    const source = await this.source({
      ...plan.input,
      items: plan.input.items.filter((name) => name !== "models" || includeModels),
    });
    const expectedSource = { ...plan.source.fingerprints };
    if (!includeModels) delete expectedSource.models;
    if (source.modelError || !same(source.fingerprints, expectedSource))
      fail("SOURCE_CHANGED");
    const results = [];
    for (const id of targetIds) {
      const item = plan.prepared.find((p) => p.target.id === id);
      try {
        const request = {
          op: "apply",
          expected: hashes(item.snapshot),
          retainOneBackup: true,
          paths: plan.input.items.filter(k => k.startsWith("path:")).map(k => k.slice(5)),
          expectedDirectories: item.snapshot.customDirectories,
          expectedFiles: Object.fromEntries(item.files.map(f => [f.path, f.targetHash])),
          fileWrites: Object.fromEntries(item.files.filter(f => f.changed).map(f => [f.path, source.customFiles[f.path].base64])),
        };
        if (!item.models?.changed) delete request.expected.models;
        if (Object.keys(item.merged.documents).length)
          request.state = item.merged.state;
        for (const [name, doc] of Object.entries(item.merged.documents))
          if (doc.changes.length) request[name] = doc.result;
        if (item.models?.changed) request.modelsBase64 = source.models;
        const r = await this.call(item.target, request);
        results.push({
          id,
          status: r.applied.length ? "applied" : "unchanged",
          files: r.applied.map((a) => a.file),
          backupPath: r.backupPath,
        });
      } catch (e) {
        const worker = e.workerResult;
        const applied = Array.isArray(worker?.applied)
          ? worker.applied
              .filter((a) =>
                ["settings", "mcp", "subagents", "models", "state"].includes(a?.file) || (typeof a?.file === "string" && a.file.startsWith("path:")),
              )
              .map((a) => ({ file: a.file, verified: a.verified === true }))
          : [];
        results.push({
          id,
          status: "failed",
          error: safeError(e),
          outcome: "inspect-and-preview-again",
          applied,
          partial: worker?.partial === true || applied.length > 0,
          backupPath:
            typeof worker?.backupPath === "string" ? worker.backupPath : null,
          uncertain: !worker || applied.some((a) => !a.verified),
        });
      }
    }
    return {
      targets: results,
      partial: results.some((r) => r.status === "failed"),
    };
  }
  discard(planId) {
    return { discarded: this.#plans.delete(planId) };
  }
}
