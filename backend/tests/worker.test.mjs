import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import fsDefault from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { handle, decodeModels } from '../worker.mjs';

const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-worker-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  return { root, agentDir: path.join(root, 'agent') };
}
async function seed(agentDir, value = { device: 'keep' }) {
  await fs.mkdir(agentDir, { recursive: true });
  await fs.writeFile(path.join(agentDir, 'settings.json'), JSON.stringify(value));
}
async function expected(agentDir, includeModels = false) {
  const snapshot = await handle({ op: 'snapshot', agentDir, includeModels });
  assert.equal(snapshot.ok, true, JSON.stringify(snapshot));
  return Object.fromEntries(['settings', 'state', ...(includeModels ? ['models'] : [])].map((k) => [k, snapshot[k].hash]));
}

test('snapshot of missing directory does not create it', async (t) => {
  const { agentDir } = await fixture(t);
  const result = await handle({ op: 'snapshot', agentDir, includeModels: true });
  assert.deepEqual(result, { ok: true, settings: { exists: false, hash: null, value: null }, state: { exists: false, hash: null, value: null }, models: { exists: false, hash: null }, mcp: { exists: false, hash: null, value: {} }, subagents: { exists: false, hash: null, value: {} } });
  await assert.rejects(fs.stat(agentDir), { code: 'ENOENT' });
});

test('malformed, non-object, oversized and directory settings fail without contents', async (t) => {
  const { agentDir } = await fixture(t);
  await fs.mkdir(agentDir);
  for (const content of ['{"SECRET_TOKEN":', '[]', 'null', ' '.repeat(4 * 1024 * 1024 + 1)]) {
    await fs.writeFile(path.join(agentDir, 'settings.json'), content);
    const result = await handle({ op: 'snapshot', agentDir });
    assert.equal(result.ok, false);
    assert.doesNotMatch(JSON.stringify(result), /SECRET_TOKEN/);
  }
  await fs.unlink(path.join(agentDir, 'settings.json'));
  await fs.mkdir(path.join(agentDir, 'settings.json'));
  assert.equal((await handle({ op: 'snapshot', agentDir })).error.code, 'NOT_REGULAR_FILE');
});

test('symlink roots, parents, settings, state and models are rejected', async (t) => {
  const { root, agentDir } = await fixture(t);
  await seed(agentDir);
  const linked = path.join(root, 'linked');
  await fs.symlink(agentDir, linked, 'dir');
  assert.equal((await handle({ op: 'snapshot', agentDir: linked })).error.code, 'SYMLINK_REJECTED');
  assert.equal((await handle({ op: 'snapshot', agentDir: path.join(linked, 'nested') })).error.code, 'SYMLINK_REJECTED');
  await fs.writeFile(path.join(root, 'external.json'), '{}');
  await fs.unlink(path.join(agentDir, 'settings.json'));
  await fs.symlink(path.join(root, 'external.json'), path.join(agentDir, 'settings.json'));
  assert.equal((await handle({ op: 'snapshot', agentDir })).error.code, 'SYMLINK_REJECTED');
  await fs.unlink(path.join(agentDir, 'settings.json'));
  await fs.mkdir(path.join(agentDir, '.harness-sync'));
  await fs.symlink(path.join(root, 'external.json'), path.join(agentDir, '.harness-sync/shared-state.json'));
  assert.equal((await handle({ op: 'snapshot', agentDir })).error.code, 'SYMLINK_REJECTED');
  await fs.unlink(path.join(agentDir, '.harness-sync/shared-state.json'));
  await fs.symlink(path.join(root, 'external.json'), path.join(agentDir, 'models.json'));
  assert.equal((await handle({ op: 'snapshot', agentDir, includeModels: true })).error.code, 'SYMLINK_REJECTED');
});

test('stale or missing expected hashes never change target contents', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const before = await fs.readFile(path.join(agentDir, 'settings.json'));
  for (const e of [{ settings: null, state: null }, {}]) {
    const result = await handle({ op: 'apply', agentDir, expected: e, settings: { changed: true } });
    assert.equal(result.ok, false);
    assert.equal(result.applied.length, 0);
    assert.deepEqual(await fs.readFile(path.join(agentDir, 'settings.json')), before);
  }
  await assert.rejects(fs.stat(path.join(agentDir, '.harness-sync')), { code: 'ENOENT' });
});

test('existing lock prevents writes and remains owned by other process', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const exp = await expected(agentDir);
  await fs.mkdir(path.join(agentDir, '.harness-sync/lock'), { recursive: true });
  const result = await handle({ op: 'apply', agentDir, expected: exp, settings: { updated: true } });
  assert.equal(result.error.code, 'LOCK_BUSY');
  assert.equal((await fs.stat(path.join(agentDir, '.harness-sync/lock'))).isDirectory(), true);
  assert.equal((await handle({ op: 'snapshot', agentDir })).settings.hash, exp.settings);
});

test('apply preserves exact backup bytes, records absence, and creates private files', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const original = Buffer.from('{ "device": "keep", "private": "original" }\n');
  await fs.writeFile(path.join(agentDir, 'settings.json'), original);
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir), settings: { device: 'keep', shared: true }, state: { version: 1 } });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.atomic, false);
  assert.equal(result.applied.length, 2);
  assert.equal(result.applied.every((f) => f.verified), true);
  assert.deepEqual(await fs.readFile(path.join(result.backupPath, 'settings.json')), original);
  const manifest = JSON.parse(await fs.readFile(path.join(result.backupPath, 'manifest.json'), 'utf8'));
  assert.deepEqual(manifest.state, { exists: false, hash: null });
  assert.equal(manifest.settings.hash, sha(original));
  assert.deepEqual((await handle({ op: 'snapshot', agentDir })).settings.value, { device: 'keep', shared: true });
  if (process.platform !== 'win32') {
    for (const file of ['settings.json', '.harness-sync/shared-state.json']) assert.equal((await fs.stat(path.join(agentDir, file))).mode & 0o777, 0o600);
    for (const file of ['settings.json', 'manifest.json']) assert.equal((await fs.stat(path.join(result.backupPath, file))).mode & 0o777, 0o600);
    for (const dir of [agentDir, path.join(agentDir, '.harness-sync'), result.backupPath]) assert.equal((await fs.stat(dir)).mode & 0o777, 0o700);
  }
  await assert.rejects(fs.stat(path.join(agentDir, '.harness-sync/lock')), { code: 'ENOENT' });
  assert.equal((await fs.readdir(agentDir)).some((name) => name.endsWith('.tmp')), false);
});

test('models are transferred byte-for-byte, backed up privately, and never returned', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir, { defaultProvider: 'new', defaultModel: 'supported' });
  const original = Buffer.from('{"providers":{"old":{"apiKey":"OLD_SECRET"}}}\n');
  const replacement = Buffer.from('{ "providers": { "new": {"apiKey":"NEW_SECRET","models":[{"id":"supported"}]} } }\n');
  await fs.writeFile(path.join(agentDir, 'models.json'), original);
  const exp = await expected(agentDir, true);
  const result = await handle({ op: 'apply', agentDir, expected: exp, modelsBase64: replacement.toString('base64') });
  assert.equal(result.ok, true);
  assert.deepEqual(await fs.readFile(path.join(agentDir, 'models.json')), replacement);
  assert.deepEqual(await fs.readFile(path.join(result.backupPath, 'models.json')), original);
  const snapshot = await handle({ op: 'snapshot', agentDir, includeModels: true });
  assert.deepEqual(snapshot.models, { exists: true, hash: sha(replacement) });
  assert.doesNotMatch(JSON.stringify([snapshot, result]), /OLD_SECRET|NEW_SECRET|apiKey/);
  if (process.platform !== 'win32') assert.equal((await fs.stat(path.join(agentDir, 'models.json'))).mode & 0o777, 0o600);
});

test('invalid models input is rejected before creating agent directory', async (t) => {
  const { agentDir } = await fixture(t);
  for (const modelsBase64 of ['$$$', Buffer.from('{"SECRET":').toString('base64'), Buffer.from([0xff]).toString('base64')]) {
    const result = await handle({ op: 'apply', agentDir, expected: { settings: null, state: null, models: null }, modelsBase64 });
    assert.match(result.error.code, /^(INVALID_MODELS_ENCODING|INVALID_JSON)$/);
    assert.doesNotMatch(JSON.stringify(result), /SECRET/);
  }
  await assert.rejects(fs.stat(agentDir), { code: 'ENOENT' });
});

test('repository and explicit forbidden-root protections cover nested paths', async (t) => {
  const { root, agentDir } = await fixture(t);
  await fs.mkdir(path.join(root, '.git'));
  await fs.writeFile(path.join(root, '.git/HEAD'), 'ref: refs/heads/main\n');
  assert.equal((await handle({ op: 'apply', agentDir, expected: { settings: null, state: null }, settings: {} })).error.code, 'REPOSITORY_TARGET');
  await fs.rm(path.join(root, '.git'), { recursive: true });
  await fs.writeFile(path.join(root, '.git'), 'gitdir: elsewhere');
  assert.equal((await handle({ op: 'snapshot', agentDir })).error.code, 'REPOSITORY_TARGET');
  await fs.unlink(path.join(root, '.git'));
  assert.equal((await handle({ op: 'snapshot', agentDir, forbiddenRoot: root })).error.code, 'REPOSITORY_TARGET');
  assert.equal((await handle({ op: 'snapshot', agentDir, forbiddenRoot: `${root}-different` })).ok, true);
});

test('no-op apply does not make backup or replace files', async (t) => {
  const { agentDir } = await fixture(t);
  const settings = { stable: true };
  await seed(agentDir, settings);
  await fs.writeFile(path.join(agentDir, 'settings.json'), `${JSON.stringify(settings, null, 2)}\n`);
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir), settings });
  assert.deepEqual(result, { ok: true, applied: [], backupPath: null });
});

test('self-contained remote worker accepts stdin and prints a single JSON response', async (t) => {
  const { agentDir } = await fixture(t);
  const code = await fs.readFile(new URL('../worker.mjs', import.meta.url), 'utf8');
  const run = spawnSync(process.execPath, ['--input-type=module', '-e', `${code}\nawait workerMain();`], { input: JSON.stringify({ op: 'snapshot', agentDir }), encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(run.stdout.trim().split('\n').length, 1);
  assert.equal(JSON.parse(run.stdout).ok, true);
  assert.equal(run.stderr, '');
});

test('mcp and subagents are snapshotted and applied with independent stale guards', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  await fs.writeFile(path.join(agentDir, 'mcp.json'), '{"local":"keep","mcpServers":{}}');
  await fs.writeFile(path.join(agentDir, 'subagents.json'), '{"local":"keep","agentOverrides":{}}');
  const snapshot = await handle({ op: 'snapshot', agentDir });
  const exp = Object.fromEntries(['settings', 'state', 'mcp', 'subagents'].map((k) => [k, snapshot[k].hash]));
  const first = await handle({ op: 'apply', agentDir, expected: { ...exp, mcp: null }, mcp: { replacement: true } });
  assert.equal(first.error.code, 'STALE_SNAPSHOT');
  const result = await handle({ op: 'apply', agentDir, expected: exp, mcp: { ...snapshot.mcp.value, mcpServers: { reviewed: { command: 'node' } } }, subagents: { ...snapshot.subagents.value, agentOverrides: { reviewer: { model: 'reviewed' } } }, state: { version: 1 } });
  assert.equal(result.ok, true);
  const after = await handle({ op: 'snapshot', agentDir });
  assert.equal(after.mcp.value.local, 'keep');
  assert.equal(after.subagents.value.local, 'keep');
  assert.equal(after.settings.hash, snapshot.settings.hash);
  assert.equal(result.applied.at(-1).file, 'state');
  assert.equal(JSON.parse(await fs.readFile(path.join(result.backupPath, 'mcp.json'), 'utf8')).local, 'keep');
  assert.equal(JSON.parse(await fs.readFile(path.join(result.backupPath, 'subagents.json'), 'utf8')).local, 'keep');
});

test('symlinked backup directories cannot redirect private writes', async (t) => {
  const { agentDir, root } = await fixture(t);
  await seed(agentDir);
  await fs.mkdir(path.join(agentDir, '.harness-sync'));
  const outside = path.join(root, 'outside');
  await fs.mkdir(outside);
  await fs.symlink(outside, path.join(agentDir, '.harness-sync/backups'), 'dir');
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir), settings: { changed: true } });
  assert.equal(result.error.code, 'SYMLINK_REJECTED');
  assert.equal(result.partial, false);
  assert.deepEqual(await fs.readdir(outside), []);
  assert.deepEqual((await handle({ op: 'snapshot', agentDir })).settings.value, { device: 'keep' });
});

test('stale models hashes reject all writes without disclosing private content', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  await fs.writeFile(path.join(agentDir, 'models.json'), '{"providers":{"secret":{"apiKey":"DO_NOT_RETURN"}}}');
  const exp = await expected(agentDir, true);
  await fs.writeFile(path.join(agentDir, 'models.json'), '{"providers":{"secret":{"apiKey":"CHANGED_SECRET"}}}');
  const result = await handle({ op: 'apply', agentDir, expected: exp, settings: { changed: true }, modelsBase64: Buffer.from('{"providers":{}}').toString('base64') });
  assert.equal(result.error.code, 'STALE_SNAPSHOT');
  assert.equal(result.partial, false);
  assert.doesNotMatch(JSON.stringify(result), /DO_NOT_RETURN|CHANGED_SECRET/);
  assert.deepEqual((await handle({ op: 'snapshot', agentDir })).settings.value, { device: 'keep' });
});

test('hashes are reread after acquiring the lock', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const exp = await expected(agentDir);
  const original = fsDefault.promises.mkdir;
  fsDefault.promises.mkdir = async (file, options) => {
    const result = await original(file, options);
    if (file === path.join(agentDir, '.harness-sync/lock')) await fs.writeFile(path.join(agentDir, 'settings.json'), '{"external":true}');
    return result;
  };
  syncBuiltinESMExports();
  try {
    const result = await handle({ op: 'apply', agentDir, expected: exp, settings: { changed: true } });
    assert.equal(result.error.code, 'STALE_SNAPSHOT');
    assert.equal(result.applied.length, 0);
    await assert.rejects(fs.stat(path.join(agentDir, '.harness-sync/backups')), { code: 'ENOENT' });
  } finally { fsDefault.promises.mkdir = original; syncBuiltinESMExports(); }
});

test('concurrent edits immediately before replacement are not overwritten', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const exp = await expected(agentDir);
  const original = fsDefault.promises.open;
  fsDefault.promises.open = async (file, ...options) => {
    const result = await original(file, ...options);
    if (file.endsWith('.tmp')) await fs.writeFile(path.join(agentDir, 'settings.json'), '{"external":true}');
    return result;
  };
  syncBuiltinESMExports();
  try {
    const result = await handle({ op: 'apply', agentDir, expected: exp, settings: { changed: true } });
    assert.equal(result.error.code, 'CONCURRENT_UPDATE');
    assert.equal(result.applied.length, 0);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(agentDir, 'settings.json'), 'utf8')), { external: true });
    assert.equal((await fs.readdir(agentDir)).some((name) => name.endsWith('.tmp')), false);
  } finally { fsDefault.promises.open = original; syncBuiltinESMExports(); }
});

test('partial failures report applied files and preserve backups without secret errors', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  const exp = await expected(agentDir);
  const original = fsDefault.promises.rename;
  fsDefault.promises.rename = async (source, target) => {
    if (target.endsWith('mcp.json')) throw new Error('SECRET_FROM_OS_OR_PATH');
    return original(source, target);
  };
  syncBuiltinESMExports();
  try {
    const result = await handle({ op: 'apply', agentDir, expected: { ...exp, mcp: null }, settings: { changed: true }, state: { version: 1 }, mcp: { servers: {} } });
    assert.equal(result.error.code, 'IO_ERROR');
    assert.equal(result.partial, true);
    assert.equal(result.applied.length, 1);
    assert.equal(result.applied[0].file, 'settings');
    assert.equal(result.applied[0].verified, true);
    assert.doesNotMatch(JSON.stringify(result), /SECRET_FROM_OS_OR_PATH/);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(result.backupPath, 'settings.json'), 'utf8')), { device: 'keep' });
    await assert.rejects(fs.stat(path.join(agentDir, '.harness-sync/shared-state.json')), { code: 'ENOENT' });
    assert.equal((await fs.readdir(path.join(agentDir, '.harness-sync'))).some((name) => name.endsWith('.tmp')), false);
    await assert.rejects(fs.stat(path.join(agentDir, '.harness-sync/lock')), { code: 'ENOENT' });
  } finally { fsDefault.promises.rename = original; syncBuiltinESMExports(); }
});

test('models accept all JSON roots and unknown provider fields without validating Pi schemas', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir);
  for (const value of [null, [], true, 3, "private", {}, { providers: [] }, { providers: { p: null } }, { providers: { p: { models: {}, futureOption: { unknown: true } } } }]) {
    const bytes = Buffer.from(JSON.stringify(value));
    const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir, true), modelsBase64: bytes.toString('base64') });
    assert.equal(result.ok, true, JSON.stringify(result));
    assert.deepEqual(await fs.readFile(path.join(agentDir, 'models.json')), bytes);
    assert.equal((await handle({ op: 'export-models', agentDir })).modelsBase64, bytes.toString('base64'));
  }
});

test('models replacement preserves configured defaults without requiring registry compatibility', async (t) => {
  const { agentDir } = await fixture(t);
  for (const defaults of [{}, { defaultProvider: 'custom', defaultModel: 'SECRET_MODEL' }, { defaultProvider: 'custom' }, { defaultModel: 'supported' }]) {
    await seed(agentDir, defaults);
    const before = await fs.readFile(path.join(agentDir, 'settings.json'));
    const bytes = Buffer.from(JSON.stringify({ providers: {}, future: defaults }));
    const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir, true), modelsBase64: bytes.toString('base64') });
    assert.equal(result.ok, true, JSON.stringify(result));
    assert.doesNotMatch(JSON.stringify(result), /SECRET_MODEL/);
    assert.deepEqual(await fs.readFile(path.join(agentDir, 'settings.json')), before);
  }
});

test('matching explicit default is retained byte-for-byte by private models push', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir, { defaultProvider: 'custom', defaultModel: 'supported', local: true });
  const settingsBefore = await fs.readFile(path.join(agentDir, 'settings.json'));
  const modelsBase64 = Buffer.from(JSON.stringify({ providers: { custom: { models: [{ id: 'supported' }], apiKey: 'SECRET' } } })).toString('base64');
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir, true), modelsBase64 });
  assert.equal(result.ok, true);
  assert.deepEqual(await fs.readFile(path.join(agentDir, 'settings.json')), settingsBefore);
  assert.deepEqual(result.applied.map((entry) => entry.file), ['models']);
  assert.doesNotMatch(JSON.stringify(result), /SECRET/);
});

test('byte-identical models allow a no-op despite unverifiable built-in defaults', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir, { defaultProvider: 'builtin', defaultModel: 'not-in-custom-file' });
  const bytes = Buffer.from('{"providers":{}}');
  await fs.writeFile(path.join(agentDir, 'models.json'), bytes);
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir, true), modelsBase64: bytes.toString('base64') });
  assert.deepEqual(result, { ok: true, applied: [], backupPath: null });
});

test('supported model options and literal credential references are retained, never resolved', async (t) => {
  const { root, agentDir } = await fixture(t);
  const marker = path.join(root, 'must-not-exist');
  const config = { $schema: 'https://pi.dev/schemas/models.schema.json', providers: { custom: {
    apiKey: `!touch ${marker}`, baseUrl: 'https://example.invalid', api: 'openai-completions',
    headers: { Authorization: '${PRIVATE_TOKEN}' }, authHeader: true,
    compat: { supportsStore: false, maxTokensField: 'max_tokens' },
    modelOverrides: { builtin: { cost: { input: 1 }, contextWindow: 2048 } },
    models: [{ id: 'm', name: 'Model', reasoning: true, input: ['text', 'image'],
      contextWindow: 32768, maxTokens: 4096,
      cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0, tiers: [{ inputTokensAbove: 1000, input: 2, output: 3, cacheRead: 0, cacheWrite: 0 }] },
      inputLimits: { maxRequestBytes: 2048, images: { resize: { maxWidth: 1024, jpegQuality: 80 }, maxPerRequest: 5 } },
      promptCache: { short: 300 }, thinkingLevelMap: { off: null, high: 'high' },
      samplingParams: { temperature: 0.5 }, samplingParamsByThinkingLevel: { high: { temperature: 0.7 } },
    }],
  } } };
  const bytes = Buffer.from(JSON.stringify(config));
  assert.deepEqual(decodeModels(bytes.toString('base64')), bytes);
  await seed(agentDir);
  const result = await handle({ op: 'apply', agentDir, expected: await expected(agentDir, true), modelsBase64: bytes.toString('base64') });
  assert.equal(result.ok, true);
  assert.deepEqual(await fs.readFile(path.join(agentDir, 'models.json')), bytes);
  assert.equal(config.providers.custom.apiKey, `!touch ${marker}`);
  await assert.rejects(fs.stat(marker), { code: 'ENOENT' });
});

test('private models replacement rereads final settings hash immediately before rename', async (t) => {
  const { agentDir } = await fixture(t);
  await seed(agentDir, { defaultProvider: 'custom', defaultModel: 'supported' });
  const exp = await expected(agentDir, true);
  const original = fsDefault.promises.open;
  fsDefault.promises.open = async (file, ...options) => {
    const result = await original(file, ...options);
    if (file.endsWith('.tmp')) await fs.writeFile(path.join(agentDir, 'settings.json'), '{"defaultProvider":"external","defaultModel":"changed"}');
    return result;
  };
  syncBuiltinESMExports();
  try {
    const result = await handle({ op: 'apply', agentDir, expected: exp, modelsBase64: Buffer.from(JSON.stringify({ providers: { custom: { models: [{ id: 'supported' }] } } })).toString('base64') });
    assert.equal(result.error.code, 'CONCURRENT_UPDATE');
    assert.equal(result.applied.length, 0);
    await assert.rejects(fs.stat(path.join(agentDir, 'models.json')), { code: 'ENOENT' });
  } finally { fsDefault.promises.open = original; syncBuiltinESMExports(); }
});

for (const kind of ['malformed', 'symlink']) test(`snapshot selection and apply ignore excluded ${kind} optional files`, async t => {
  const {root,agentDir}=await fixture(t);await seed(agentDir);
  for(const name of ['mcp','subagents']) {
    const file=path.join(agentDir,`${name}.json`);
    if(kind==='malformed') await fs.writeFile(file,'INVALID_JSON');
    else {const external=path.join(root,`${name}.json`);await fs.writeFile(external,'INVALID_JSON');await fs.symlink(external,file);}
  }
  const result=await handle({op:'snapshot',agentDir,documents:['settings']});assert.equal(result.ok,true);
  assert.deepEqual(Object.keys(result).sort(),['ok','settings','state']);
  const applied=await handle({op:'apply',agentDir,expected:{settings:result.settings.hash,state:result.state.hash},settings:{device:'updated'}});
  assert.equal(applied.ok,true,JSON.stringify(applied));
  for(const name of ['mcp','subagents']) {
    assert.equal(await fs.readFile(path.join(agentDir,`${name}.json`),'utf8'),'INVALID_JSON');
    if(kind==='symlink') assert((await fs.lstat(path.join(agentDir,`${name}.json`))).isSymbolicLink());
    assert.equal((await handle({op:'snapshot',agentDir,documents:['settings',name]})).ok,false);
  }
});
for(const documents of [['models'],['state'],['unknown'],['settings',null],'mcp',{},null]) test(`invalid snapshot document selection rejects ${JSON.stringify(documents)}`,async t=>{
  const {agentDir}=await fixture(t);
  const result=await handle({op:'snapshot',agentDir,documents});assert.equal(result.ok,false);assert.equal(result.error.code,'INVALID_DOCUMENT_SELECTION');
  await assert.rejects(fs.stat(agentDir),{code:'ENOENT'});
});

test('model snapshots never expose contents, even if legacy summaries are requested', async t => {
 const {agentDir}=await fixture(t);
 await seed(agentDir);
 await fs.writeFile(path.join(agentDir,'models.json'),'{"SECRET_PROVIDER":{"models":"SECRET_MODEL","apiKey":"SECRET_KEY"}}');
 const snapshot=await handle({op:'snapshot',agentDir,includeModels:true,includeModelSummary:true,documents:[]});
 assert.equal(snapshot.ok,true);
 assert.deepEqual(Object.keys(snapshot).sort(), ['models','ok']);
 assert.deepEqual(Object.keys(snapshot.models).sort(), ['exists','hash']);
 assert.doesNotMatch(JSON.stringify(snapshot),/SECRET_|apiKey|models.*value/);
});

for (const original of ['{ "SECRET":', 'null', '[]', Buffer.from([0xff, 0xfe])]) test(`invalid target models can be backed up and replaced (${String(original)})`, async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  const bytes=Buffer.from(original);
  await fs.writeFile(path.join(agentDir,'models.json'),bytes);
  const replacement=Buffer.from('{ "futureFormat": { "providers": null } }\r\n');
  const result=await handle({op:'apply',agentDir,expected:await expected(agentDir,true),modelsBase64:replacement.toString('base64')});
  assert.equal(result.ok,true,JSON.stringify(result));
  assert.deepEqual(await fs.readFile(path.join(result.backupPath,'models.json')),bytes);
  assert.deepEqual(await fs.readFile(path.join(agentDir,'models.json')),replacement);
});

test('invalid source JSON export fails safely, while target snapshot stays opaque', async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  for(const bytes of [Buffer.from('{"SECRET":'), Buffer.from([0xff])]) {
    await fs.writeFile(path.join(agentDir,'models.json'),bytes);
    const result=await handle({op:'export-models',agentDir});
    assert.equal(result.error.code,'SOURCE_MODELS_INVALID_JSON');
    assert.doesNotMatch(JSON.stringify(result),/SECRET/);
    assert.equal((await handle({op:'snapshot',agentDir,documents:[],includeModels:true})).models.hash,sha(bytes));
  }
});

test('model reads enforce the size cap again after reading a growing file', async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  const file=path.join(agentDir,'models.json'); await fs.writeFile(file,'{}');
  const original=fsDefault.promises.open;
  fsDefault.promises.open=async (name,...options)=>{
    const handle=await original(name,...options);
    if(name===file) handle.readFile=async()=>Buffer.alloc(4*1024*1024+1);
    return handle;
  };
  syncBuiltinESMExports();
  try {
    assert.equal((await handle({op:'snapshot',agentDir,documents:[],includeModels:true})).error.code,'FILE_TOO_LARGE');
    assert.equal((await handle({op:'export-models',agentDir})).error.code,'FILE_TOO_LARGE');
  } finally { fsDefault.promises.open=original; syncBuiltinESMExports(); }
});

test('models-only copies do not parse or rewrite unselected settings and baseline', async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  await fs.writeFile(path.join(agentDir,'settings.json'),'BROKEN_PRIVATE_SETTINGS');
  await fs.mkdir(path.join(agentDir,'.harness-sync'));
  await fs.writeFile(path.join(agentDir,'.harness-sync/shared-state.json'),'BROKEN_BASELINE');
  const snapshot=await handle({op:'snapshot',agentDir,documents:[],includeModels:true});
  assert.equal(snapshot.ok,true);
  const result=await handle({op:'apply',agentDir,expected:{models:snapshot.models.hash},modelsBase64:Buffer.from('{}').toString('base64')});
  assert.equal(result.ok,true,JSON.stringify(result));
  assert.deepEqual(result.applied.map(a=>a.file),['models']);
  assert.equal(await fs.readFile(path.join(agentDir,'settings.json'),'utf8'),'BROKEN_PRIVATE_SETTINGS');
  assert.equal(await fs.readFile(path.join(agentDir,'.harness-sync/shared-state.json'),'utf8'),'BROKEN_BASELINE');
});

test('unreadable target models fail before writes or backups with a safe actionable code', async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  const file=path.join(agentDir,'models.json'); await fs.writeFile(file,'old bytes');
  const exp=await expected(agentDir,true);
  const original=fsDefault.promises.open;
  fsDefault.promises.open=async (name,...options)=>{
    if(name===file) { const error=new Error('PRIVATE_PATH_CONTENTS'); error.code='EACCES'; throw error; }
    return original(name,...options);
  };
  syncBuiltinESMExports();
  try {
    const result=await handle({op:'apply',agentDir,expected:exp,settings:{changed:true},modelsBase64:Buffer.from('{}').toString('base64')});
    assert.equal(result.error.code,'TARGET_MODELS_UNREADABLE');
    assert.deepEqual(result.applied,[]);
    assert.doesNotMatch(JSON.stringify(result),/PRIVATE_PATH_CONTENTS/);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(agentDir,'settings.json'))),{device:'keep'});
    await assert.rejects(fs.stat(path.join(agentDir,'.harness-sync')),{code:'ENOENT'});
  } finally { fsDefault.promises.open=original; syncBuiltinESMExports(); }
});

test('concurrent malformed target model edits immediately before replacement are not overwritten', async t => {
  const {agentDir}=await fixture(t); await seed(agentDir);
  const file=path.join(agentDir,'models.json'); await fs.writeFile(file,'old malformed bytes');
  const snapshot=await handle({op:'snapshot',agentDir,documents:[],includeModels:true});
  const original=fsDefault.promises.open;
  fsDefault.promises.open=async (name,...options)=>{
    const handle=await original(name,...options);
    if(name.endsWith('.tmp')) await fs.writeFile(file,'concurrent malformed bytes');
    return handle;
  };
  syncBuiltinESMExports();
  try {
    const result=await handle({op:'apply',agentDir,expected:{models:snapshot.models.hash},modelsBase64:Buffer.from('{}').toString('base64')});
    assert.equal(result.error.code,'CONCURRENT_UPDATE');
    assert.deepEqual(result.applied,[]);
    assert.equal(await fs.readFile(file,'utf8'),'concurrent malformed bytes');
    assert.equal(await fs.readFile(path.join(result.backupPath,'models.json'),'utf8'),'old malformed bytes');
  } finally { fsDefault.promises.open=original; syncBuiltinESMExports(); }
});
