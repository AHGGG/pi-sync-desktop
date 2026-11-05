import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { MAX_PROTOCOL_BYTES } from '../worker.mjs';

const backend = new URL('../', import.meta.url);

test('all private protocol boundaries share the bounded combined-file envelope', async () => {
  assert.equal(MAX_PROTOCOL_BYTES, 48 * 1024 * 1024);
  const bridge = await fs.readFile(new URL('../bridge.mjs', import.meta.url), 'utf8');
  const worker = await fs.readFile(new URL('../worker.mjs', import.meta.url), 'utf8');
  const ssh = await fs.readFile(new URL('../../crates/sync-core/src/transport.rs', import.meta.url), 'utf8');
  const desktop = await fs.readFile(new URL('../../crates/desktop/src/backend.rs', import.meta.url), 'utf8');
  assert.match(bridge, /Buffer\.byteLength\(next\) > MAX_PROTOCOL_BYTES/);
  assert.match(bridge, /Buffer\.byteLength\(buffer\) > MAX_PROTOCOL_BYTES/);
  assert.match(worker, /length > MAX_PROTOCOL_BYTES/);
  assert.match(ssh, /const MAX_PROTOCOL_BYTES: usize = 48 \* 1024 \* 1024/);
  assert.match(ssh, /Buffer\.byteLength\(b\)>50331649/);
  assert.match(ssh, /payload\.len\(\) <= MAX/);
  assert.match(ssh, /n <= MAX_PROTOCOL_BYTES \+ 1/);
  assert.match(desktop, /48 \* 1024 \* 1024/);
});

test('maximum valid combined document and binary payload fits the envelope', () => {
  const document = { content: 'x'.repeat(4 * 1024 * 1024 - 64) };
  const custom = Buffer.alloc(4 * 1024 * 1024).toString('base64');
  const payload = { code: 'x'.repeat(64 * 1024), request: {
    settings: document, mcp: document, subagents: document, state: document,
    modelsBase64: custom, fileWrites: { 'extensions/a': custom, 'extensions/b': custom },
  } };
  const length = Buffer.byteLength(JSON.stringify(payload));
  assert.ok(length > 16 * 1024 * 1024, 'fixture must exceed old transport limit');
  assert.ok(length < MAX_PROTOCOL_BYTES);
});

test('bridge and worker accept valid envelopes larger than the old 16 MiB cap', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'pi-envelope-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const padding = 'x'.repeat(17 * 1024 * 1024);
  const bridge = spawnSync(process.execPath, ['bridge.mjs'], {
    cwd: backend, input: JSON.stringify({ id: 'large', op: 'ping', padding }) + '\n', encoding: 'utf8',
  });
  assert.equal(bridge.status, 0, bridge.stderr);
  assert.equal(JSON.parse(bridge.stdout).ok, true);
  const worker = spawnSync(process.execPath, ['--input-type=module', '-e', "import { workerMain } from './worker.mjs'; await workerMain();"], {
    cwd: backend, input: JSON.stringify({ op: 'snapshot', documents: [], agentDir: root, padding }), encoding: 'utf8',
  });
  assert.equal(worker.status, 0, worker.stderr);
  assert.equal(JSON.parse(worker.stdout).ok, true);
});
