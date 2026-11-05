import test from 'node:test';
import { spawn } from 'node:child_process';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import fsDefault from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { SyncEngine } from '../engine.mjs';
import { handle, validateRelativePath, decodeModels } from '../worker.mjs';
async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'pi-custom-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = { id: 'source', type: 'local', agentDir: path.join(root, 'source') };
  const target = { id: 'target', type: 'local', agentDir: path.join(root, 'target') };
  const put = async (device, rel, content) => { const file = path.join(device.agentDir, rel); await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, content); };
  await fs.mkdir(source.agentDir); await fs.mkdir(target.agentDir);
  return { source, target, put, engine: new SyncEngine(), input: { source, targets: [target], items: ['path:extensions'] } };
}
function choose(engine, plan, choices) { return engine.resolve({ planId: plan.planId, fileChoices: { target: choices ?? Object.fromEntries(plan.targets[0].files.filter(f => f.changed).map(f => [f.path, 'source'])) } }); }
async function apply(engine, plan) { return engine.apply({ planId: plan.planId, approved: true, targetIds: ['target'] }); }

test('recursive custom files require choices, preserve target-only data, copy exact binary and retain recovery bytes', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a.ts', 'export const value = 2;');
  await put(source, 'extensions/nested/icon.bin', Buffer.from([0, 255, 4]));
  await put(target, 'extensions/a.ts', 'old');
  await put(target, 'extensions/local.ts', 'local only');
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].status, 'conflicts');
  assert.equal(preview.targets[0].files.find(f => f.path.endsWith('local.ts')).sourceAvailable, false);
  assert.equal(preview.targets[0].files.find(f => f.path.endsWith('icon.bin')).after, '[Binary file: content hidden]');
  await assert.rejects(apply(engine, preview), /TARGET_NOT_READY/);
  const result = await apply(engine, choose(engine, preview));
  assert.equal(result.targets[0].status, 'applied');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/local.ts'), 'utf8'), 'local only');
  assert.deepEqual(await fs.readFile(path.join(target.agentDir, 'extensions/nested/icon.bin')), Buffer.from([0, 255, 4]));
  const backup = result.targets[0].backupPath;
  const manifest = JSON.parse(await fs.readFile(path.join(backup, 'manifest.json')));
  assert.equal(manifest['path:extensions/a.ts'].exists, true);
  const contents = await Promise.all((await fs.readdir(backup)).filter(n => n.endsWith('.bin')).map(n => fs.readFile(path.join(backup, n), 'utf8')));
  assert.ok(contents.includes('old'));
  const unchanged = await engine.preview(input);
  assert.equal(unchanged.targets[0].status, 'ready');
  assert.ok(unchanged.targets[0].files.every(f => !f.changed && !f.conflict));
});

test('standalone permissions are flagged and keep/skip choices prevent replacement', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  input.items = ['path:permissions.json'];
  await put(source, 'permissions.json', '{"tools":["*"]}');
  await put(target, 'permissions.json', '{"tools":[]}');
  for (const choice of ['target', 'skip']) {
    const preview = await engine.preview(input);
    assert.equal(preview.targets[0].files[0].sensitive, true);
    assert.equal(preview.targets[0].files[0].warning, 'PERMISSION_POLICY_CHANGE');
    const result = await apply(engine, choose(engine, preview, { 'permissions.json': choice }));
    assert.equal(result.targets[0].status, 'unchanged');
    assert.equal(await fs.readFile(path.join(target.agentDir, 'permissions.json'), 'utf8'), '{"tools":[]}');
  }
});

for (const edit of ['source-content', 'source-addition', 'source-deletion', 'target-content', 'target-addition']) test(`${edit} invalidates captured recursive review`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a.txt', 'new'); await put(target, 'extensions/a.txt', 'old');
  const plan = choose(engine, await engine.preview(input));
  if (edit === 'source-deletion') await fs.unlink(path.join(source.agentDir, 'extensions/a.txt'));
  else await put(edit.startsWith('source') ? source : target, edit.endsWith('addition') ? 'extensions/b.txt' : 'extensions/a.txt', 'changed');
  if (edit.startsWith('source')) await assert.rejects(apply(engine, plan), /SOURCE_CHANGED/);
  else { const r = await apply(engine, plan); assert.equal(r.targets[0].error, 'STALE_SNAPSHOT'); }
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/a.txt'), 'utf8'), edit === 'target-content' ? 'changed' : 'old');
});

for (const rel of ['../a', '/tmp/a', 'extensions/../settings.json', 'a\\b', 'a:b', 'a//b', 'constructor', 'a/CON.txt', 'a/foo.', 'auth.json', 'credentials.json', 'extensions/api-token.txt', '.harness-sync', 'extensions/.env', 'settings.json', 'mcp.json', 'models.json', 'subagents.json']) test(`reject unsafe selection ${rel}`, () => assert.throws(() => validateRelativePath(rel), /INVALID_CUSTOM_PATH|PROTECTED_CUSTOM_PATH|AUDITED_DOCUMENT_REQUIRED/));

test('missing source, protected descendants and source symlinks block instead of empty diff', async t => {
  const { source, put, engine, input } = await fixture(t);
  await assert.rejects(engine.preview(input), /SOURCE_CUSTOM_PATH_MISSING/);
  await put(source, 'extensions/auth.json', 'do not show');
  await assert.rejects(engine.preview(input), /PROTECTED_CUSTOM_PATH/);
  await fs.unlink(path.join(source.agentDir, 'extensions/auth.json'));
  await fs.symlink('/tmp', path.join(source.agentDir, 'extensions/link'));
  await assert.rejects(engine.preview(input), /SYMLINK_REJECTED/);
});

test('target symlinks block before writes, and custom review masks secret/command contents', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/config.json', '{"command":"never display me"}');
  let plan = await engine.preview(input);
  assert.equal(plan.targets[0].files[0].after, '[Sensitive content hidden]');
  await fs.symlink(source.agentDir, path.join(target.agentDir, 'extensions'));
  plan = await engine.preview(input);
  assert.equal(plan.targets[0].status, 'blocked'); assert.equal(plan.targets[0].error, 'SYMLINK_REJECTED');
});

test('custom limits reject oversized files and too many files', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/large', Buffer.alloc(4 * 1024 * 1024 + 1));
  await assert.rejects(engine.preview(input), /FILE_TOO_LARGE/);
  await fs.unlink(path.join(source.agentDir, 'extensions/large'));
  for (let i = 0; i < 257; i++) await put(source, `extensions/file-${i}`, 'a');
  await assert.rejects(engine.preview(input), /CUSTOM_SELECTION_TOO_LARGE/);
});

test('worker refuses custom writes outside selection and malformed encodings', async t => {
  const { target } = await fixture(t);
  const request = { op: 'apply', agentDir: target.agentDir, expected: {}, paths: ['extensions'], expectedFiles: { 'other.txt': null }, fileWrites: { 'other.txt': 'YQ==' } };
  assert.equal((await handle(request)).error.code, 'INVALID_CUSTOM_SELECTION');
  request.expectedFiles = { 'extensions/a': null }; request.fileWrites = { 'extensions/a': '@@@' };
  assert.equal((await handle(request)).error.code, 'INVALID_CUSTOM_WRITES');
});

test('custom backup cleanup supports changing selections across successful runs', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'one'); await put(target, 'extensions/a', 'old');
  await apply(engine, choose(engine, await engine.preview(input)));
  input.items = ['path:notes.txt']; await put(source, 'notes.txt', 'note');
  const result = await apply(engine, choose(engine, await engine.preview(input)));
  assert.equal(result.targets[0].status, 'applied');
  assert.equal((await fs.readdir(path.join(target.agentDir, '.harness-sync/backups'))).length, 1);
});

test('partial custom failure preserves verified progress and recoverable backup without leaking OS errors', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  for (const name of ['a', 'b']) { await put(source, `extensions/${name}`, 'new'); await put(target, `extensions/${name}`, 'old'); }
  const plan = choose(engine, await engine.preview(input));
  const original = fsDefault.promises.rename;
  fsDefault.promises.rename = async (from, to) => { if (to.endsWith('/extensions/b')) throw new Error('SECRET_OS_ERROR'); return original(from, to); };
  syncBuiltinESMExports();
  try {
    const result = await apply(engine, plan);
    assert.equal(result.targets[0].partial, true);
    assert.equal(result.targets[0].applied[0].file, 'path:extensions/a');
    assert.equal(result.targets[0].applied[0].verified, true);
    assert.ok(result.targets[0].backupPath);
    assert.doesNotMatch(JSON.stringify(result), /SECRET_OS_ERROR/);
    assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/b'), 'utf8'), 'old');
    await assert.rejects(apply(engine, plan), /PLAN_NOT_FOUND/);
  } finally { fsDefault.promises.rename = original; syncBuiltinESMExports(); }
});

for (const moment of ['lock', 'replacement']) test(`custom files recheck freshness at ${moment}`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'new'); await put(target, 'extensions/a', 'old');
  const plan = choose(engine, await engine.preview(input));
  const method = moment === 'lock' ? 'mkdir' : 'open';
  const original = fsDefault.promises[method];
  fsDefault.promises[method] = async (file, ...args) => {
    const result = await original(file, ...args);
    if ((moment === 'lock' && file.endsWith('/.harness-sync/lock')) || (moment === 'replacement' && file.endsWith('.tmp'))) await fs.writeFile(path.join(target.agentDir, 'extensions/a'), 'external');
    return result;
  };
  syncBuiltinESMExports();
  try {
    const result = await apply(engine, plan);
    assert.equal(result.targets[0].error, moment === 'lock' ? 'STALE_SNAPSHOT' : 'CONCURRENT_UPDATE');
    assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/a'), 'utf8'), 'external');
  } finally { fsDefault.promises[method] = original; syncBuiltinESMExports(); }
});

test('custom selections reject excessive aggregate bytes and deep nesting', async t => {
  const { source, put, engine, input } = await fixture(t);
  for (let i = 0; i < 3; i++) await put(source, `extensions/${i}`, Buffer.alloc(3 * 1024 * 1024));
  await assert.rejects(engine.preview(input), /CUSTOM_SELECTION_TOO_LARGE/);
  await fs.rm(path.join(source.agentDir, 'extensions'), { recursive: true });
  await put(source, 'extensions/' + 'd/'.repeat(34) + 'a', 'x');
  await assert.rejects(engine.preview(input), /CUSTOM_SELECTION_TOO_LARGE/);
});

test('invalid custom choices cannot authorize unreviewed paths or implicit source selection', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/a', 'new');
  const preview = await engine.preview(input);
  for (const choices of [null, 'source', { 'extensions/a': 'auto' }, { 'unreviewed': 'source' }]) assert.throws(() => engine.resolve({ planId: preview.planId, fileChoices: { target: choices } }), /INVALID_FILE_CHOICE/);
  assert.throws(() => engine.resolve({ planId: preview.planId, fileChoices: { unknown: {} } }), /UNKNOWN_RESOLUTION_TARGET/);
});

for (const rel of ['a?', 'a*', 'a<', 'a>', 'a|', 'a"']) test(`cross-platform invalid filename ${rel} rejected`, () => assert.throws(() => validateRelativePath(rel), /INVALID_CUSTOM_PATH/));

for (const name of ['toString', 'hasOwnProperty', 'valueOf', 'isPrototypeOf']) test(`Object.prototype filename ${name} remains an ordinary reviewed file`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  input.items = [`path:${name}`];
  await put(source, name, 'new');
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].status, 'conflicts');
  assert.equal(preview.targets[0].files[0].before, null);
  assert.equal(preview.targets[0].files[0].conflict, true);
  const result = await apply(engine, choose(engine, preview));
  assert.equal(result.targets[0].status, 'applied');
  assert.equal(await fs.readFile(path.join(target.agentDir, name), 'utf8'), 'new');
});

for (const side of ['source', 'target']) test(`explicit file inside nested ${side} repository is rejected`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  input.items = ['path:extensions/a.js'];
  await put(source, 'extensions/a.js', 'new');
  await put(target, 'extensions/a.js', 'old');
  await put(side === 'source' ? source : target, 'extensions/.git/HEAD', 'ref: refs/heads/main');
  if (side === 'source') await assert.rejects(engine.preview(input), /REPOSITORY_TARGET/);
  else {
    const result = await engine.preview(input);
    assert.equal(result.targets[0].status, 'blocked');
    assert.equal(result.targets[0].error, 'REPOSITORY_TARGET');
  }
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/a.js'), 'utf8'), 'old');
});

test('repository introduced before custom replacement is not overwritten', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  input.items = ['path:extensions/a.js'];
  await put(source, 'extensions/a.js', 'new'); await put(target, 'extensions/a.js', 'old');
  const plan = choose(engine, await engine.preview(input));
  const original = fsDefault.promises.open;
  fsDefault.promises.open = async (file, ...args) => {
    const result = await original(file, ...args);
    if (file.endsWith('.tmp')) await put(target, 'extensions/.git/HEAD', 'ref: refs/heads/main');
    return result;
  };
  syncBuiltinESMExports();
  try {
    const result = await apply(engine, plan);
    assert.equal(result.targets[0].error, 'REPOSITORY_TARGET');
    assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/a.js'), 'utf8'), 'old');
  } finally { fsDefault.promises.open = original; syncBuiltinESMExports(); }
});

for (const pair of [['A', 'a'], ['A/x', 'a/y'], ['caf\u00e9.txt', 'cafe\u0301.txt']]) test(`portable path collision ${pair.join(' vs ')} fails before writes`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  for (const rel of pair) await put(source, 'extensions/' + rel, rel);
  await assert.rejects(engine.preview(input), /CUSTOM_PATH_COLLISION/);
  assert.deepEqual(await fs.readdir(target.agentDir), []);
});

test('explicit aliasing selections are rejected but exact parent-child overlap deduplicates', async t => {
  const { source, put, engine, input } = await fixture(t);
  await put(source, 'extensions/A/x', 'x'); await put(source, 'extensions/a/y', 'y');
  input.items = ['path:extensions/A/x', 'path:extensions/a/y'];
  await assert.rejects(engine.preview(input), /CUSTOM_PATH_COLLISION/);
  await fs.rm(path.join(source.agentDir, 'extensions/a'), { recursive: true });
  input.items = ['path:extensions', 'path:extensions/A/x'];
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].files.length, 1);
});

for (const pair of [['A', 'a'], ['A/x', 'a/y'], ['A', 'A/y']]) test(`cross-source/target aliases ${pair.join(' vs ')} block target review`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/' + pair[0], 'new');
  await put(target, 'extensions/' + pair[1], 'old');
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].status, 'blocked');
  assert.equal(preview.targets[0].error, 'CUSTOM_PATH_COLLISION');
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/' + pair[1]), 'utf8'), 'old');
});

for (const dir of ['a', 'A']) test(`target empty directory ${dir} cannot alias source file A`, async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/A', 'new');
  await fs.mkdir(path.join(target.agentDir, 'extensions', dir), { recursive: true });
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].error, 'CUSTOM_PATH_COLLISION');
});

test('new empty target directory invalidates inventory before any writes', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  await put(source, 'extensions/A', 'new');
  const plan = choose(engine, await engine.preview(input));
  await fs.mkdir(path.join(target.agentDir, 'extensions/a'), { recursive: true });
  const result = await apply(engine, plan);
  assert.equal(result.targets[0].error, 'STALE_SNAPSHOT');
});

test('custom recursive binary and policy files roundtrip through the self-contained SSH worker protocol', async t => {
  const { source, target, put, input } = await fixture(t);
  source.type = 'ssh'; target.type = 'ssh';
  await put(source, 'extensions/nested/data.bin', Buffer.from([255, 0, 123]));
  await put(source, 'extensions/permissions.json', '{"allow":["read"]}');
  await put(target, 'extensions/nested/data.bin', 'old binary');
  const worker = await fs.readFile(new URL('../worker.mjs', import.meta.url), 'utf8');
  let calls = 0;
  const engine = new SyncEngine((_device, request) => new Promise((resolve, reject) => {
    calls++;
    const child = spawn(process.execPath, ['--input-type=module', '-e', worker + '\nawait workerMain();'], { stdio: ['pipe', 'pipe', 'pipe'] });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.on('error', reject);
    child.on('close', () => { try { resolve(JSON.parse(output)); } catch (error) { reject(error); } });
    child.stdin.end(JSON.stringify(request));
  }));
  const preview = await engine.preview(input);
  assert.equal(preview.targets[0].files.find(f => f.path.endsWith('permissions.json')).sensitive, true);
  const result = await apply(engine, choose(engine, preview));
  assert.equal(result.targets[0].status, 'applied');
  assert.equal(calls, 4);
  assert.deepEqual(await fs.readFile(path.join(target.agentDir, 'extensions/nested/data.bin')), Buffer.from([255, 0, 123]));
  assert.equal(await fs.readFile(path.join(target.agentDir, 'extensions/permissions.json'), 'utf8'), '{"allow":["read"]}');
});

test('large valid custom bytes apply without recursive-regex stack overflow', async t => {
  const { source, target, put, engine, input } = await fixture(t);
  const content = Buffer.alloc(3.5 * 1024 * 1024, 65);
  await put(source, 'extensions/large.bin', content);
  const plan = choose(engine, await engine.preview(input));
  const result = await apply(engine, plan);
  assert.equal(result.targets[0].status, 'applied');
  assert.deepEqual(await fs.readFile(path.join(target.agentDir, 'extensions/large.bin')), content);
});


test('large valid models base64 uses stack-safe validation', () => {
  const content = Buffer.from(JSON.stringify({ padding: 'x'.repeat(3.5 * 1024 * 1024) }));
  assert.deepEqual(decodeModels(content.toString('base64')), content);
  for (const invalid of ['A===', 'A=AA', 'AAAA=', '!!!!', 'A']) assert.throws(() => decodeModels(invalid), /INVALID_MODELS_ENCODING/);
});
