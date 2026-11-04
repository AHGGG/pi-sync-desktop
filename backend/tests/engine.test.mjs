import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { SyncEngine } from "../engine.mjs";
async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "pi-desktop-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = {
    id: "source",
    type: "local",
    agentDir: path.join(root, "source"),
  };
  const target = {
    id: "target",
    type: "local",
    agentDir: path.join(root, "target"),
  };
  for (const d of [source, target]) await fs.mkdir(d.agentDir);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    JSON.stringify({ theme: "light", deviceId: "source" }),
  );
  await fs.writeFile(
    path.join(target.agentDir, "settings.json"),
    JSON.stringify({
      theme: "dark",
      deviceId: "target",
      shellPath: "/bin/zsh",
      defaultProvider: "builtin",
      defaultModel: "existing",
      unrelated: 42,
    }),
  );
  return { source, target, engine: new SyncEngine(), root };
}
test("review requires explicit field decisions, preserves device fields and applies only approved plan", async (t) => {
  const { source, target, engine } = await fixture(t);
  const input = { source, targets: [target], items: ["settings"] };
  const first = await engine.preview(input);
  assert.equal(first.targets[0].status, "conflicts");
  assert.equal(first.targets[0].documents.settings.conflicts[0].path, "/theme");
  const plan = await engine.preview({
    ...input,
    resolutions: { target: { settings: { "/theme": "source" } } },
  });
  assert.equal(plan.targets[0].status, "ready");
  await assert.rejects(
    engine.apply({ planId: plan.planId }),
    /APPROVAL_REQUIRED/,
  );
  const result = await engine.apply({
    planId: plan.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(result.targets[0].status, "applied");
  const settings = JSON.parse(
    await fs.readFile(path.join(target.agentDir, "settings.json")),
  );
  assert.deepEqual(settings, {
    theme: "light",
    deviceId: "target",
    shellPath: "/bin/zsh",
    defaultProvider: "builtin",
    defaultModel: "existing",
    unrelated: 42,
  });
  await assert.rejects(
    engine.apply({
      planId: plan.planId,
      approved: true,
      targetIds: ["target"],
    }),
    /PLAN_NOT_FOUND/,
  );
});
test("source or target edits invalidate reviewed plans, offline peers do not prevent independent review", async (t) => {
  const { source, target, engine } = await fixture(t);
  const request = {
    source,
    targets: [target, { id: "offline", type: "ssh", agentDir: "~/.pi/agent" }],
    items: ["settings"],
    resolutions: { target: { settings: { "/theme": "source" } } },
  };
  const p = await engine.preview(request);
  assert.equal(p.targets[1].status, "blocked");
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"new"}',
  );
  await assert.rejects(
    engine.apply({ planId: p.planId, approved: true, targetIds: ["target"] }),
    /SOURCE_CHANGED/,
  );
  const q = await engine.preview(request);
  await fs.writeFile(
    path.join(target.agentDir, "settings.json"),
    '{"theme":"external"}',
  );
  const result = await engine.apply({
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(result.targets[0].error, "STALE_SNAPSHOT");
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .theme,
    "external",
  );
});
test("successful applies retain exactly one prior configuration backup", async (t) => {
  const { source, target, engine } = await fixture(t);
  const input = { source, targets: [target], items: ["settings"] };
  const p = await engine.preview({
    ...input,
    resolutions: { target: { settings: { "/theme": "source" } } },
  });
  await engine.apply({
    planId: p.planId,
    approved: true,
    targetIds: ["target"],
  });
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"third"}',
  );
  const q = await engine.preview(input);
  await engine.apply({
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  const backups = await fs.readdir(
    path.join(target.agentDir, ".harness-sync/backups"),
  );
  assert.equal(backups.length, 1);
  const old = JSON.parse(
    await fs.readFile(
      path.join(
        target.agentDir,
        ".harness-sync/backups",
        backups[0],
        "settings.json",
      ),
    ),
  );
  assert.equal(old.theme, "light");
});
test("resolve uses captured review, accepts skip, and never silently replaces changed inputs", async (t) => {
  const { source, target, engine } = await fixture(t);
  const input = { source, targets: [target], items: ["settings"] };
  const p = await engine.preview(input);
  const q = engine.resolve({
    planId: p.planId,
    resolutions: { target: { settings: { "/theme": "skip" } } },
  });
  assert.equal(q.targets[0].status, "ready");
  assert.equal(q.targets[0].documents.settings.changes.length, 0);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"unreviewed"}',
  );
  await assert.rejects(
    engine.apply({ planId: q.planId, approved: true, targetIds: ["target"] }),
    /SOURCE_CHANGED/,
  );
});
test("private remote models stay off review output and preserve unaffected built-in default", async (t) => {
  const { source, target, root } = await fixture(t);
  const privateModels = {
    providers: {
      custom: {
        apiKey: "SECRET_NEVER_IN_REVIEW",
        models: [
          {
            id: "private-model",
            name: "Private",
            api: "openai-completions",
            reasoning: false,
            input: ["text"],
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
            contextWindow: 32000,
            maxTokens: 4096,
          },
        ],
      },
    },
  };
  await fs.writeFile(
    path.join(source.agentDir, "models.json"),
    JSON.stringify(privateModels),
  );
  const { handle } = await import("../worker.mjs");
  let exported = false;
  const engine = new SyncEngine(async (t, request) => {
    if (request.op === "export-models") exported = true;
    return handle({ ...request, agentDir: source.agentDir });
  });
  const remote = { ...source, id: "remote", type: "ssh" };
  const p = await engine.preview({
    source: remote,
    targets: [target],
    items: ["models"],
  });
  assert.equal(exported, true);
  assert.equal(p.targets[0].status, "ready");
  assert.doesNotMatch(JSON.stringify(p), /SECRET_NEVER_IN_REVIEW/);
  const r = await engine.apply({
    planId: p.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(r.targets[0].status, "applied");
  assert.deepEqual(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "models.json"))),
    privateModels,
  );
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .defaultModel,
    "existing",
  );
});
test("incompatible private models can be skipped without blocking selected settings", async (t) => {
  const { source, target, engine } = await fixture(t);
  await fs.writeFile(
    path.join(source.agentDir, "models.json"),
    '{"providers":{}}',
  );
  await fs.writeFile(
    path.join(target.agentDir, "models.json"),
    '{"providers":{"builtin":{"models":[{"id":"existing"}]}}}',
  );
  const p = await engine.preview({
    source,
    targets: [target],
    items: ["settings", "models"],
  });
  assert.equal(p.targets[0].status, "conflicts");
  assert.equal(p.targets[0].models.error, "DEFAULT_MODEL_UNVERIFIED");
  const q = engine.resolve({
    planId: p.planId,
    resolutions: { target: { settings: { "/theme": "source" } } },
    modelChoices: { target: "skip" },
  });
  assert.equal(q.targets[0].status, "ready");
  const r = await engine.apply({
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(r.targets[0].status, "applied");
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .theme,
    "light",
  );
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "models.json")))
      .providers.builtin.models[0].id,
    "existing",
  );
});
test("partial worker failures report verified files and backup location without private contents", async (t) => {
  const { source } = await fixture(t);
  const engine = new SyncEngine(async (_target, request) =>
    request.op === "apply"
      ? {
          ok: false,
          error: { code: "VERIFY_FAILED" },
          partial: true,
          applied: [{ file: "settings", verified: true }],
          backupPath: "/private/backup",
        }
      : {
          ok: true,
          settings: { exists: false, hash: null, value: {} },
          state: { exists: false, hash: null, value: null },
        },
  );
  const p = await engine.preview({
    source,
    targets: [{ id: "remote", type: "ssh", agentDir: "~/.pi/agent" }],
    items: ["settings"],
    resolutions: { remote: { settings: { "/theme": "source" } } },
  });
  const r = await engine.apply({
    planId: p.planId,
    approved: true,
    targetIds: ["remote"],
  });
  assert.equal(r.partial, true);
  assert.equal(r.targets[0].partial, true);
  assert.deepEqual(r.targets[0].applied, [
    { file: "settings", verified: true },
  ]);
  assert.equal(r.targets[0].backupPath, "/private/backup");
});
test("selected fields do not copy unselected source settings", async (t) => {
  const { source, target, engine } = await fixture(t);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"light","defaultThinkingLevel":"max"}',
  );
  const p = await engine.preview({
    source,
    targets: [target],
    items: ["settings"],
    fields: { settings: ["/theme"] },
    resolutions: { target: { settings: { "/theme": "source" } } },
  });
  assert.equal(p.targets[0].documents.settings.changes.length, 1);
  await engine.apply({
    planId: p.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .defaultThinkingLevel,
    undefined,
  );
});
test("unselected private package paths on a source do not prevent a selected theme sync", async (t) => {
  const { source, target, engine } = await fixture(t);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"light","packages":["/private/local/package"]}',
  );
  const p = await engine.preview({
    source,
    targets: [target],
    items: ["settings"],
    fields: { settings: ["/theme"] },
  });
  assert.equal(p.targets[0].status, "conflicts");
  assert.equal(p.targets[0].documents.settings.conflicts.length, 1);
});
test("MCP command application requires captured details reveal plus explicit review approval", async (t) => {
  const { source, target, engine } = await fixture(t);
  await fs.writeFile(
    path.join(source.agentDir, "mcp.json"),
    '{"mcpServers":{"argent":{"command":"npx","args":["tool"]}}}',
  );
  const p = await engine.preview({ source, targets: [target], items: ["mcp"] });
  const choices = {
    target: {
      mcp: {
        "/mcpServers/argent/command": "source",
        "/mcpServers/argent/args": "source",
      },
    },
  };
  const q = engine.resolve({ planId: p.planId, resolutions: choices });
  await assert.rejects(
    engine.apply({
      planId: q.planId,
      approved: true,
      targetIds: ["target"],
      commandReviewApproved: true,
    }),
    /COMMAND_REVIEW_REQUIRED/,
  );
  const full = engine.reveal({ planId: q.planId, approved: true });
  assert.equal(full.targets[0].documents.mcp.changes[0].after.value, "npx");
  const r = engine.resolve({ planId: q.planId, resolutions: choices });
  await assert.rejects(
    engine.apply({ planId: r.planId, approved: true, targetIds: ["target"] }),
    /COMMAND_REVIEW_REQUIRED/,
  );
  const applied = await engine.apply({
    planId: r.planId,
    approved: true,
    targetIds: ["target"],
    commandReviewApproved: true,
  });
  assert.equal(applied.targets[0].status, "applied");
});

test("public settings plus MCP stages an explicit activation decision and preserves other extensions", async (t) => {
  const { target, root, engine } = await fixture(t);
  const publicRoot = path.join(root, "public");
  await fs.mkdir(path.join(publicRoot, "shared"), { recursive: true });
  await fs.writeFile(path.join(publicRoot, "shared/settings.json"), "{}");
  await fs.writeFile(path.join(publicRoot, "shared/mcp.json"), "{}");
  await fs.writeFile(
    path.join(target.agentDir, "settings.json"),
    '{"extensions":["local.ts","-builtin:mcp"],"deviceId":"target"}',
  );
  const p = await engine.preview({
    source: { type: "public", root: publicRoot },
    targets: [target],
    items: ["settings", "mcp"],
  });
  assert.equal(p.targets[0].status, "conflicts");
  assert.equal(
    p.targets[0].documents.settings.conflicts[0].path,
    "/extensions/builtin:mcp",
  );
  assert.deepEqual(p.targets[0].documents.settings.conflicts[0].source.value, [
    "+builtin:mcp",
  ]);
  const q = engine.resolve({
    planId: p.planId,
    resolutions: {
      target: { settings: { "/extensions/builtin:mcp": "source" } },
    },
  });
  const r = await engine.apply({
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(r.targets[0].status, "applied");
  assert.deepEqual(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .extensions,
    ["local.ts", "+builtin:mcp"],
  );
});
test("MCP activation is excluded for settings-only, MCP-only, or an explicit field filter excluding activation", async (t) => {
  const { target, root, engine } = await fixture(t);
  const publicRoot = path.join(root, "public");
  await fs.mkdir(path.join(publicRoot, "shared"), { recursive: true });
  await fs.writeFile(
    path.join(publicRoot, "shared/settings.json"),
    '{"extensions":["+builtin:mcp"]}',
  );
  await fs.writeFile(path.join(publicRoot, "shared/mcp.json"), "{}");
  for (const selection of [
    { items: ["settings"] },
    { items: ["mcp"] },
    { items: ["settings", "mcp"], fields: { settings: ["/theme"] } },
  ]) {
    const p = await engine.preview({
      source: { type: "public", root: publicRoot },
      targets: [target],
      ...selection,
    });
    const settings = p.targets[0].documents.settings;
    assert.equal(
      (settings?.changes.length ?? 0) + (settings?.conflicts.length ?? 0),
      0,
    );
  }
});

test("destination preview receives only compatibility metadata, never source model credentials", async (t) => {
  const { source, target } = await fixture(t);
  const config = {
    providers: {
      custom: {
        apiKey: "DO_NOT_TRANSFER_API_KEY",
        baseUrl: "https://private-endpoint.invalid",
        headers: { Authorization: "DO_NOT_TRANSFER_HEADER" },
        models: [{ id: "chosen-model", name: "private-label" }],
      },
    },
  };
  await fs.writeFile(
    path.join(source.agentDir, "models.json"),
    JSON.stringify(config),
  );
  const { handle } = await import("../worker.mjs");
  const requests = [];
  const engine = new SyncEngine(async (_target, request) => {
    requests.push(structuredClone(request));
    return handle({ ...request, agentDir: target.agentDir });
  });
  const destination = { ...target, type: "ssh" };
  const p = await engine.preview({
    source,
    targets: [destination],
    items: ["models"],
  });
  assert.equal(p.targets[0].status, "ready");
  const check = requests.find((r) => r.op === "check-models");
  assert.equal(check.modelsBase64, undefined);
  assert.deepEqual(check.modelSummary, {
    providers: { custom: { models: [{ id: "chosen-model" }] } },
  });
  assert.deepEqual(p.targets[0].models.sourceSummary, check.modelSummary);
  assert.deepEqual(p.targets[0].models.targetSummary, { providers: {} });
  assert.doesNotMatch(
    JSON.stringify(p),
    /DO_NOT_TRANSFER|private-endpoint|apiKey|Authorization|private-label/,
  );
  for (const request of requests) {
    const payload =
      JSON.stringify(request) +
      (request.modelsBase64
        ? Buffer.from(request.modelsBase64, "base64").toString("utf8")
        : "");
    assert.doesNotMatch(
      payload,
      /DO_NOT_TRANSFER|private-endpoint|apiKey|Authorization|private-label/,
    );
  }
  const beforeResolve = requests.length;
  const q = engine.resolve({
    planId: p.planId,
    modelChoices: { target: "source" },
  });
  const revealed = engine.reveal({ planId: q.planId, approved: true });
  assert.equal(requests.length, beforeResolve);
  assert.doesNotMatch(
    JSON.stringify(revealed),
    /DO_NOT_TRANSFER|private-endpoint|apiKey|Authorization|private-label/,
  );
  await assert.rejects(
    engine.apply({ planId: q.planId, targetIds: ["target"] }),
    /APPROVAL_REQUIRED/,
  );
  assert.equal(
    requests.some((r) => r.op === "apply"),
    false,
  );
  const result = await engine.apply({
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(result.targets[0].status, "applied");
  const apply = requests.find((r) => r.op === "apply");
  assert.deepEqual(
    JSON.parse(Buffer.from(apply.modelsBase64, "base64")),
    config,
  );
});
