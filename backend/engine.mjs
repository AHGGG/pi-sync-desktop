import fs from "node:fs/promises";
import { latestPublic } from "./public-source.mjs";
import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { handle, decodeModels } from "./worker.mjs";
import { SCHEMAS, validateDocument, mergeDocuments } from "./documents.mjs";
import { cell, object, same } from "./merge.mjs";
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
    const documents = input.items.filter((k) => k !== "models"),
      values = {},
      fingerprints = {};
    let models, modelError;
    if (input.source.type === "public") {
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
      const snapshot = documents.length
        ? await this.call(source, {
            op: "snapshot",
            documents,
            includeModels: false,
          })
        : {};
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
    return { values, fingerprints, models, modelError };
  }
  async preview(input) {
    if (
      !object(input) ||
      !object(input.source) ||
      !Array.isArray(input.items) ||
      !input.items.length ||
      new Set(input.items).size !== input.items.length ||
      input.items.some(
        (k) => !["settings", "mcp", "subagents", "models"].includes(k),
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
        const status =
          Object.values(documents).some((d) => d.conflicts.length) ||
          (models?.changed && models.error)
            ? "conflicts"
            : "ready";
        reviews.push({ id: t.id, status, documents, models });
        prepared.push({ target: t, snapshot, merged, status, models });
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
  resolve({ planId, resolutions = {}, modelChoices = {} }) {
    const plan = this.#plans.get(planId);
    if (!plan) fail("PLAN_NOT_FOUND");
    if (
      !object(resolutions) ||
      !object(modelChoices) ||
      [...Object.keys(resolutions), ...Object.keys(modelChoices)].some(
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
    const prepared = plan.prepared.map((item) => {
      const merged = mergeDocuments(
        plan.source.values,
        { state: { exists: false, value: null }, ...item.snapshot },
        resolutions[item.target.id] ?? plan.input.resolutions?.[item.target.id],
      );
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
        status:
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
                ["settings", "mcp", "subagents", "models", "state"].includes(
                  a?.file,
                ),
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
