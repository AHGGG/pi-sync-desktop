import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import fsDefault from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { SyncEngine } from '../engine.mjs';
import { handle, validateContentRules, CONTENT_LIMITS } from '../worker.mjs';
async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'pi-contents-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = { id: 'source', type: 'local', agentDir: path.join(root, 'source') };
  const target = { id: 'target', type: 'local', agentDir: path.join(root, 'target') };
  await fs.mkdir(source.agentDir); await fs.mkdir(target.agentDir);
  const put = async (device, rel, value) => { const file = path.join(device.agentDir, rel); await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, value); };
  const engine = new SyncEngine();
  return { source, target, put, engine, input: { source, targets: [target], items: ['path:extensions'] } };
}
const choose = (engine, plan) => engine.resolve({ planId: plan.planId, fileChoices: { target: Object.fromEntries(plan.targets[0].files.filter(f => f.changed).map(f => [f.path, 'source'])) } });
const apply = (engine, plan) => engine.apply({ planId: plan.planId, approved: true, targetIds: ['target'] });

test('metadata inventory lists oversize, both devices, blocked names and suggestions without opening content', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/logs/big.log', Buffer.alloc(CONTENT_LIMITS.fileBytes + 1));
  await put(source, 'extensions/auth.json', 'PRIVATE-SENTINEL');
  await put(source, 'extensions/invalid:name', 'PRIVATE-SENTINEL');
  await put(target, 'extensions/cache/a', 'PRIVATE-SENTINEL');
  await fs.symlink('/tmp', path.join(source.agentDir, 'extensions/link'));
  const original = fsDefault.promises.open;
  fsDefault.promises.open = async () => { throw new Error('CONTENTS_MUST_NOT_OPEN_FILES'); };
  syncBuiltinESMExports();
  try {
    const result = await engine.contents({ ...input, paths: ['extensions'] });
    assert.equal(result.devices.length, 2);
    assert.equal(result.devices[0].entries.find(e => e.path.endsWith('big.log')).size, CONTENT_LIMITS.fileBytes + 1);
    assert.equal(result.devices[0].entries.find(e => e.path.endsWith('logs')).suggestedExclude, 'logs');
    assert.equal(result.devices[0].entries.find(e => e.path.endsWith('auth.json')).blocked, 'PROTECTED_CUSTOM_PATH');
    assert.equal(result.devices[0].entries.find(e => e.path.endsWith('invalid:name')).blocked, 'INVALID_CUSTOM_PATH');
    assert.equal(result.devices[0].entries.find(e => e.path.endsWith('link')).blocked, 'SYMLINK_REJECTED');
    assert.equal(result.devices[1].entries.find(e => e.path.endsWith('cache')).suggestedExclude, 'cache');
    assert.doesNotMatch(JSON.stringify(result), /PRIVATE-SENTINEL|base64|hash/);
  } finally { fsDefault.promises.open = original; syncBuiltinESMExports(); }
});

test('nested longest-prefix rules exclude future children while explicit descendants copy and excluded churn is ignored', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/logs/keep.txt', 'new');
  await put(target, 'extensions/logs/keep.txt', 'old');
  await put(source, 'extensions/logs/huge.log', Buffer.alloc(CONTENT_LIMITS.fileBytes + 1));
  await put(target, 'extensions/logs/local.log', 'retain me');
  input.contentRules = { 'extensions/logs': false, 'extensions/logs/keep.txt': true };
  const plan = choose(engine, await engine.preview(input));
  assert.deepEqual(plan.targets[0].files.map(f => f.path), ['extensions/logs/keep.txt']);
  await put(source, 'extensions/logs/new.log', 'churn');
  await put(target, 'extensions/logs/local.log', 'changed excluded');
  await put(target, 'extensions/logs/new.log', 'also excluded');
  const result = await apply(engine, plan);
  assert.equal(result.targets[0].status, 'applied');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/logs/keep.txt'), 'utf8'), 'new');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/logs/local.log'), 'utf8'), 'changed excluded');
  const manifest = JSON.parse(await fs.readFile(path.join(result.targets[0].backupPath, 'manifest.json')));
  assert.deepEqual(Object.keys(manifest), ['path:extensions/logs/keep.txt']);
});

for (const side of ['source', 'target']) test(`included ${side} churn still invalidates selected fingerprints`, async t => {
  const f = await fixture(t);
  await f.put(f.source, 'extensions/a', 'new'); await f.put(f.target, 'extensions/a', 'old');
  f.input.contentRules = { 'extensions/logs': false };
  const plan = choose(f.engine, await f.engine.preview(f.input));
  await f.put(f[side], 'extensions/a', 'changed');
  if (side === 'source') await assert.rejects(apply(f.engine, plan), /SOURCE_CHANGED/);
  else assert.equal((await apply(f.engine, plan)).targets[0].error, 'STALE_SNAPSHOT');
});

test('excluded protected paths, symlinks and invalid names never block or get read', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'new');
  for (const d of [source, target]) {
    await put(d, 'extensions/cache/auth.json', 'PRIVATE');
    await put(d, 'extensions/cache/invalid:name', 'PRIVATE');
    await fs.symlink('/tmp', path.join(d.agentDir, 'extensions/cache/link'));
    await put(d, 'extensions/auth.json', 'PRIVATE');
  }
  input.contentRules = { 'extensions/cache': false, 'extensions/auth.json': false };
  const plan = choose(engine, await engine.preview(input));
  assert.equal((await apply(engine, plan)).targets[0].status, 'applied');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/auth.json'), 'utf8'), 'PRIVATE');
});

test('excluded whole roots are pruned even when missing', async t => {
  const { engine, input } = await fixture(t);
  input.contentRules = { extensions: false };
  const plan = await engine.preview(input);
  assert.deepEqual(plan.targets[0].files, []);
  assert.equal((await apply(engine, plan)).targets[0].status, 'unchanged');
});

for (const rules of [{ '../bad': false }, { 'a//b': false }, { 'a\\b': false }, { 'extensions/auth.json': true }, { 'extensions/.env': true }, { 'extensions/A': true, 'extensions/a': false }, { 'extensions/a': 'false' }, JSON.parse('{"__proto__":false}'), { 'extensions/．．': false }, { 'extensions/／a': false }]) test(`invalid or protected rule rejected: ${JSON.stringify(rules)}`, () => assert.throws(() => validateContentRules(rules), /INVALID_|PROTECTED_/));

for (const names of [['logs', 'LOGS'], ['café', 'cafe\u0301']]) test(`portable exclusion protects aliases ${names}`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, `extensions/${names[0]}`, 'new'); await put(target, `extensions/${names[1]}`, 'old');
  input.contentRules = { [`extensions/${names[1]}`]: false };
  const plan = await engine.preview(input);
  assert.deepEqual(plan.targets[0].files, []);
  assert.equal((await apply(engine, plan)).targets[0].status, 'unchanged');
});

for (const side of ['source', 'target']) test(`included large logs return actionable ${side} limit details`, async t => {
  const f = await fixture(t);
  await f.put(f.source, 'extensions/a', 'new');
  await f.put(f[side], 'extensions/logs/app.log', Buffer.alloc(CONTENT_LIMITS.fileBytes + 1));
  f.input.contentRules = { 'extensions/logs': true };
  const expected = { deviceId: side, path: 'extensions/logs/app.log', size: CONTENT_LIMITS.fileBytes + 1, limit: CONTENT_LIMITS.fileBytes, kind: 'fileBytes' };
  if (side === 'source') await assert.rejects(f.engine.preview(f.input), error => { assert.deepEqual(error.errorDetail, expected); return true; });
  else assert.deepEqual((await f.engine.preview(f.input)).targets[0].errorDetail, expected);
});

test('forged apply cannot write, back up or fingerprint excluded file keys', async t => {
  const { target, put } = await fixture(t);
  await put(target, 'extensions/logs/a', 'keep');
  const request = { op: 'apply', agentDir: target.agentDir, paths: ['extensions'], contentRules: { 'extensions/logs': false }, expected: {}, expectedFiles: { 'extensions/logs/a': null }, fileWrites: { 'extensions/logs/a': 'eA==' } };
  assert.equal((await handle(request)).error.code, 'INVALID_CUSTOM_SELECTION');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/logs/a'), 'utf8'), 'keep');
  await assert.rejects(fs.stat(path.join(target.agentDir, '.harness-sync')), { code: 'ENOENT' });
});

test('post-lock scans ignore excluded churn and never restore excluded writes', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'new'); await put(target, 'extensions/a', 'old');
  input.contentRules = { 'extensions/logs': false };
  const plan = choose(engine, await engine.preview(input));
  const original = fsDefault.promises.mkdir;
  fsDefault.promises.mkdir = async (file, ...args) => {
    const result = await original(file, ...args);
    if (file.endsWith('/.harness-sync/lock')) await put(target, 'extensions/logs/app.log', Buffer.alloc(CONTENT_LIMITS.fileBytes + 1));
    return result;
  };
  syncBuiltinESMExports();
  try { assert.equal((await apply(engine, plan)).targets[0].status, 'applied'); }
  finally { fsDefault.promises.mkdir = original; syncBuiltinESMExports(); }
});

test('inventory bounds depth with partial safe entries and never silently claims completeness', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/' + 'd/'.repeat(34) + 'a', 'hidden');
  const result = await engine.contents({ ...input, paths: ['extensions'] });
  assert.equal(result.devices[0].error, 'CONTENT_INVENTORY_TOO_LARGE');
  assert.ok(result.devices[0].entries.length > 0);
  assert.ok(result.devices[0].entries.length <= CONTENT_LIMITS.entries);
  assert.equal(result.devices[1].entries[0].kind, 'missing');
});

for (const kind of ['files', 'totalBytes']) test(`included ${kind} limit remains enforced and excluded entries do not count`, async t => {
  const { source, put, engine, input } = await fixture(t);
  const number = kind === 'files' ? 257 : 3;
  for (let i = 0; i < number; i++) await put(source, `extensions/cache/${i}`, kind === 'files' ? 'a' : Buffer.alloc(3 * 1024 * 1024));
  await assert.rejects(engine.preview(input), error => {
    assert.equal(error.message, 'CUSTOM_SELECTION_TOO_LARGE');
    assert.equal(error.errorDetail.kind, kind);
    assert.equal(error.errorDetail.deviceId, 'source');
    assert.equal(error.errorDetail.limit, CONTENT_LIMITS[kind]);
    return true;
  });
  input.contentRules = { 'extensions/cache': false };
  assert.deepEqual((await engine.preview(input)).targets[0].files, []);
});

test('source fingerprints do not depend on directory enumeration order', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'a'); await put(source, 'extensions/b', 'b');
  // These valid distinct paths collate equally under localeCompare.
  await put(source, 'extensions/a\u200b', 'invisible suffix');
  const plan = choose(engine, await engine.preview(input));
  const original = fsDefault.promises.opendir;
  fsDefault.promises.opendir = async (...args) => {
    const directory = await original(...args);
    const entries = [];
    for await (const entry of directory) entries.push(entry);
    return { async *[Symbol.asyncIterator]() { yield* entries.reverse(); } };
  };
  syncBuiltinESMExports();
  try { assert.equal((await apply(engine, plan)).targets[0].status, 'applied'); }
  finally { fsDefault.promises.opendir = original; syncBuiltinESMExports(); }
});

test('saved excluded branches prune metadata traversal and can be re-expanded explicitly', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/cache/' + 'd/'.repeat(34) + 'a', 'hidden');
  const request = { ...input, paths: ['extensions'], contentRules: { 'extensions/cache': false } };
  const result = await engine.contents(request);
  assert.equal(result.devices[0].error, undefined);
  assert.deepEqual(result.devices[0].entries.map(e => e.path), ['extensions', 'extensions/cache']);
  assert.equal(result.devices[0].entries[1].omitted, true);
  request.contentRules['extensions/cache'] = true;
  assert.equal((await engine.contents(request)).devices[0].error, 'CONTENT_INVENTORY_TOO_LARGE');
});
