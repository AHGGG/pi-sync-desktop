import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
function bridge(t, onTransport) {
  const child = spawn(
    process.execPath,
    [new URL("../bridge.mjs", import.meta.url).pathname],
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
