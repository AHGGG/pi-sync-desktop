import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
function bridge(t, onTransport, nodeArgs = []) {
  const child = spawn(
    process.execPath,
    [...nodeArgs, new URL("../bridge.mjs", import.meta.url).pathname],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  const pending = new Map();
  let id = 0,
    stderr = "";
  child.stderr.on("data", (s) => (stderr += s));
  t.after(() => {
    child.kill();
    assert.equal(stderr, "");
  });
  createInterface({ input: child.stdout }).on("line", async (line) => {
    const response = JSON.parse(line);
    if (response.type === "transport") {
      const result = await onTransport(response);
      child.stdin.write(
        JSON.stringify({
          op: "transport-result",
          transportId: response.transportId,
          response: result,
        }) + "\n",
      );
      return;
    }
    const entry = pending.get(response.id);
    if (entry) {
      clearTimeout(entry.timer);
      pending.delete(response.id);
      entry.resolve(response);
    }
  });
  return (message) =>
    new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(() => reject(Error("bridge timeout")), 5000);
      pending.set(requestId, { resolve, timer });
      child.stdin.write(JSON.stringify({ ...message, id: requestId }) + "\n");
    });
}
test("JSONL stays live while servicing SSH transport responses", async (t) => {
  const request = bridge(t, async () => ({
    ok: true,
    settings: { exists: false, hash: null, value: null },
    state: { exists: false, hash: null, value: null },
  }));
  assert.equal((await request({ op: "ping" })).protocolVersion, 1);
  const response = await request({
    op: "preview",
    source: { id: "source", type: "ssh", agentDir: "~/.pi/agent" },
    targets: [{ id: "target", type: "ssh", agentDir: "~/.pi/agent" }],
    items: ["settings"],
  });
  assert.equal(response.ok, true);
  assert.equal(response.targets[0].status, "ready");
});
test("private tokens are masked in review without altering the captured plan", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "desktop-bridge-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = {
      id: "source",
      type: "local",
      agentDir: path.join(root, "source"),
    },
    target = {
      id: "target",
      type: "local",
      agentDir: path.join(root, "target"),
    };
  for (const d of [source, target]) await fs.mkdir(d.agentDir);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"normal"}',
  );
  await fs.writeFile(
    path.join(target.agentDir, "settings.json"),
    '{"theme":"sk-SECRET_TOKEN"}',
  );
  const request = bridge(t);
  const p = await request({
    op: "preview",
    source,
    targets: [target],
    items: ["settings"],
  });
  assert.doesNotMatch(JSON.stringify(p), /sk-SECRET_TOKEN/);
  const q = await request({
    op: "resolve",
    planId: p.planId,
    resolutions: { target: { settings: { "/theme": "target" } } },
  });
  const applied = await request({
    op: "apply",
    planId: q.planId,
    approved: true,
    targetIds: ["target"],
  });
  assert.equal(applied.ok, true);
  assert.equal(
    JSON.parse(await fs.readFile(path.join(target.agentDir, "settings.json")))
      .theme,
    "sk-SECRET_TOKEN",
  );
});

test("explicit reveal exposes captured MCP details only, never other masked fields", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "desktop-reveal-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = {
      id: "source",
      type: "local",
      agentDir: path.join(root, "source"),
    },
    target = {
      id: "target",
      type: "local",
      agentDir: path.join(root, "target"),
    };
  for (const d of [source, target]) await fs.mkdir(d.agentDir);
  await fs.writeFile(
    path.join(source.agentDir, "settings.json"),
    '{"theme":"normal"}',
  );
  await fs.writeFile(
    path.join(target.agentDir, "settings.json"),
    '{"theme":"sk-OTHER_SECRET"}',
  );
  await fs.writeFile(
    path.join(source.agentDir, "mcp.json"),
    '{"mcpServers":{"argent":{"command":"npx","args":["--token=sk-MCP_SECRET"]}}}',
  );
  const request = bridge(t);
  const p = await request({
    op: "preview",
    source,
    targets: [target],
    items: ["settings", "mcp"],
  });
  assert.doesNotMatch(JSON.stringify(p), /sk-MCP_SECRET|sk-OTHER_SECRET/);
  const r = await request({ op: "reveal", planId: p.planId, approved: true });
  assert.equal(r.ok, true);
  assert.equal(r.planId, p.planId);
  assert.match(JSON.stringify(r), /sk-MCP_SECRET/);
  assert.doesNotMatch(JSON.stringify(r), /sk-OTHER_SECRET/);
});

test("native authentication owns transport lifetime without an independent bridge timeout", async (t) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "desktop-bridge-clock-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const preload = path.join(root, "clock.cjs");
  // Accelerate the former 90s transport deadline, without slowing the suite.
  await fs.writeFile(preload, "const original = setTimeout; global.setTimeout = (f, ms, ...args) => original(f, ms === 90000 ? 1 : ms, ...args);\n");
  const request = bridge(t, async () => {
    await new Promise(resolve => setTimeout(resolve, 50));
    return { ok: true, settings: { exists: false, hash: null, value: null }, state: { exists: false, hash: null, value: null } };
  }, ["--require", preload]);
  const response = await request({ op: "preview", source: { id: "source", type: "ssh", agentDir: "~/.pi/agent" }, targets: [{ id: "target", type: "ssh", agentDir: "~/.pi/agent" }], items: ["settings"] });
  assert.equal(response.ok, true);
  assert.equal(response.targets[0].status, "ready");
});

test('only an approved per-file response bypasses private content redaction', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'bridge-file-review-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = { id: 'source', type: 'local', agentDir: path.join(root, 'source') };
  const target = { id: 'target', type: 'local', agentDir: path.join(root, 'target') };
  for (const dir of [source, target]) await fs.mkdir(dir.agentDir);
  const secret = 'sk-EXPLICIT_PRIVATE_REVIEW';
  await fs.writeFile(path.join(source.agentDir, 'models.json'), JSON.stringify({ apiKey: secret }));
  await fs.writeFile(path.join(target.agentDir, 'models.json'), '{}');
  await fs.writeFile(path.join(source.agentDir, 'notes.txt'), 'token: ' + secret);
  const request = bridge(t);
  const p = await request({ op: 'preview', source, targets: [target], items: ['models', 'path:notes.txt'] });
  assert.equal(p.ok, true);
  assert.doesNotMatch(JSON.stringify(p), /EXPLICIT_PRIVATE_REVIEW/);
  const base = { op: 'review-file', planId: p.planId, targetId: 'target', document: 'models', path: '/', page: 0 };
  const denied = await request(base);
  assert.equal(denied.error.code, 'APPROVAL_REQUIRED');
  assert.doesNotMatch(JSON.stringify(denied), /EXPLICIT_PRIVATE_REVIEW/);
  const full = await request({ ...base, approved: true });
  assert.equal(full.ok, true);
  assert.equal(JSON.parse(full.after).apiKey, secret);
  const file = await request({ ...base, document: 'files', path: 'notes.txt', approved: true });
  assert.equal(file.after, 'token: ' + secret);
  const reveal = await request({ op: 'reveal', planId: p.planId, approved: true });
  assert.doesNotMatch(JSON.stringify(reveal), /EXPLICIT_PRIVATE_REVIEW/);
  const resolved = await request({ op: 'resolve', planId: p.planId });
  assert.doesNotMatch(JSON.stringify(resolved), /EXPLICIT_PRIVATE_REVIEW/);
  await request({ op: 'discard', planId: resolved.planId });
  assert.equal((await request({ ...base, planId: resolved.planId, approved: true })).error.code, 'PLAN_NOT_FOUND');
});
