import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { SyncEngine } from '../engine.mjs';
import { handle } from '../worker.mjs';

async function setup(t, items = ['models'], before = '{"old":1}', after = '{"apiKey":"sk-private"}') {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'full-review-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = { id: 'source', type: 'local', agentDir: path.join(root, 'source') };
  const target = { id: 'target', type: 'local', agentDir: path.join(root, 'target') };
  for (const dir of [source, target]) await fs.mkdir(dir.agentDir);
  const filename = items[0].startsWith('path:') ? items[0].slice(5) : items[0] + '.json';
  if (before !== null) await fs.writeFile(path.join(target.agentDir, filename), before);
  await fs.writeFile(path.join(source.agentDir, filename), after);
  const calls = [];
  const engine = new SyncEngine(async (target, request) => { calls.push(request); return handle(request); });
  const input = { source, targets: [{ ...target, type: 'ssh' }], items };
  const plan = await engine.preview(input);
  const args = { planId: plan.planId, targetId: target.id, document: items[0].startsWith('path:') ? 'files' : items[0], path: items[0].startsWith('path:') ? filename : '/', page: 0, approved: true };
  return { engine, input, plan, args, source, target, calls, filename };
}
async function collect(engine, args) {
  const first = await engine.reviewFile(args);
  const pages = [first];
  for (let page = 1; page < first.pageCount; page++) pages.push(await engine.reviewFile({ ...args, page }));
  return { first, before: pages.map(p => p.before).join(''), after: pages.map(p => p.after).join('') };
}

test('models review requires approval and scope; default review exposes only hashes', async t => {
  const { engine, args, plan, calls } = await setup(t);
  assert.doesNotMatch(JSON.stringify(plan), /sk-private|apiKey/);
  assert.equal(calls.some(c => c.op === 'review-models'), false);
  for (const mutation of [{ approved: false }, { approved: undefined }, { document: 'files', path: 'other.txt' }, { targetId: 'other' }, { path: '/models' }, { document: 'settings' }, { page: -1 }, { page: 0.5 }])
    await assert.rejects(engine.reviewFile({ ...args, ...mutation }));
  assert.equal(calls.some(c => c.op === 'review-models'), false);
  const page = await engine.reviewFile(args);
  assert.equal(page.before, '{"old":1}');
  assert.equal(page.after, '{"apiKey":"sk-private"}');
  assert.equal(page.format, 'text');
  assert.deepEqual(Object.keys(calls.at(-1)).sort(), ['agentDir', 'approved', 'expectedHash', 'op']);
  await assert.rejects(engine.reviewFile({ ...args, page: 1 }), /INVALID_REVIEW_PAGE/);
  const resolved = engine.resolve({ planId: args.planId, modelChoices: { target: 'skip' } });
  assert.equal((await engine.reviewFile({ ...args, planId: resolved.planId })).after, page.after);
});

test('captured models survive later edits and resolve, while first reveal rejects a stale target', async t => {
  const { engine, args, source, target, calls } = await setup(t);
  const first = await engine.reviewFile(args);
  await fs.writeFile(path.join(source.agentDir, 'models.json'), '{"newSource":true}');
  await fs.writeFile(path.join(target.agentDir, 'models.json'), '{"newTarget":true}');
  assert.deepEqual(await engine.reviewFile(args), first);
  const next = engine.resolve({ planId: args.planId });
  assert.equal((await engine.reviewFile({ ...args, planId: next.planId })).before, first.before);
  assert.equal(calls.filter(c => c.op === 'review-models').length, 1);
  const fresh = await engine.preview({ source, targets: [target], items: ['models'] });
  await fs.writeFile(path.join(target.agentDir, 'models.json'), '{}');
  await assert.rejects(engine.reviewFile({ ...args, planId: fresh.planId }), /TARGET_CHANGED/);
});

for (const before of ['not valid JSON', Buffer.from([255, 0, 254]), null]) test(`target models raw review accepts ${before === null ? 'missing' : typeof before}`, async t => {
  const { engine, args } = await setup(t, ['models'], before);
  const page = await engine.reviewFile(args);
  assert.equal(page.format, Buffer.isBuffer(before) ? 'hex' : 'text');
  assert.equal(page.before, Buffer.isBuffer(before) ? '00000000: ff 00 fe\n' : before ?? '');
});

for (const binary of [false, true]) test(`custom ${binary ? 'binary' : 'sensitive text'} pages reveal full captured contents`, async t => {
  const after = binary ? Buffer.alloc(16000, 255) : '\ufefftoken: private\n' + '😀'.repeat(12020) + '\nLAST';
  const before = binary ? Buffer.alloc(4300, 1) : 'command: hidden\n';
  const { engine, args, plan, source, target, filename } = await setup(t, ['path:notes.txt'], before, after);
  assert.match(plan.targets[0].files[0].after, /hidden/);
  await fs.writeFile(path.join(source.agentDir, filename), 'mutated');
  await fs.writeFile(path.join(target.agentDir, filename), 'mutated');
  const result = await collect(engine, args);
  assert.ok(result.first.pageCount > 1);
  if (binary) {
    assert.match(result.after, /^00001000: /m);
    assert.deepEqual(Buffer.from(result.after.replace(/^[0-9a-f]{8}: /gm, '').replace(/\s/g, ''), 'hex'), after);
    assert.deepEqual(Buffer.from(result.before.replace(/^[0-9a-f]{8}: /gm, '').replace(/\s/g, ''), 'hex'), before);
  } else {
    assert.equal(result.after, after);
    assert.equal(result.before, before);
    for (let page = 0; page < result.first.pageCount; page++) {
      const value = (await engine.reviewFile({ ...args, page })).after;
      assert.ok(value.length <= 12000);
      assert.doesNotMatch(value, /^[\udc00-\udfff]|[\ud800-\udbff]$/u);
    }
  }
});

test('custom plain truncated content and target-only files obey selected scope', async t => {
  const text = 'x'.repeat(30000) + 'END';
  const { engine, args, input, target } = await setup(t, ['path:notes.txt'], null, text);
  assert.equal((await collect(engine, args)).after, text);
  await fs.writeFile(path.join(target.agentDir, 'other.txt'), 'target only');
  await assert.rejects(engine.reviewFile({ ...args, path: 'other.txt' }), /INVALID_REVIEW_SELECTION/);
  await assert.rejects(engine.reviewFile({ ...args, path: '.env' }), /PROTECTED_CUSTOM_PATH/);
  const next = await engine.preview(input);
  engine.discard(next.planId);
  await assert.rejects(engine.reviewFile({ ...args, planId: next.planId }), /PLAN_NOT_FOUND/);
});

test('discard and expiry prevent private review including a pending target read', async t => {
  const { engine, args } = await setup(t);
  const now = Date.now;
  try {
    Date.now = () => now() + 31 * 60 * 1000;
    await assert.rejects(engine.reviewFile(args), /PLAN_EXPIRED/);
  } finally { Date.now = now; }
  await assert.rejects(engine.reviewFile(args), /PLAN_NOT_FOUND/);
  const f = await setup(t);
  let release;
  f.engine.remote = async (target, request) => {
    await new Promise(resolve => { release = resolve; });
    return handle(request);
  };
  const pending = f.engine.reviewFile(f.args);
  f.engine.discard(f.args.planId);
  release();
  await assert.rejects(pending, /PLAN_NOT_FOUND/);
});

test('worker raw model export checks approval and expected hash without JSON parsing', async t => {
  const { target } = await setup(t, ['models'], Buffer.from([255]));
  assert.equal((await handle({ op: 'review-models', agentDir: target.agentDir })).error.code, 'APPROVAL_REQUIRED');
  assert.equal((await handle({ op: 'review-models', agentDir: target.agentDir, approved: true })).error.code, 'EXPECTED_HASH_REQUIRED');
  assert.equal((await handle({ op: 'review-models', agentDir: target.agentDir, approved: true, expectedHash: null })).error.code, 'TARGET_CHANGED');
});

test('audited JSON views preserve target fields, unresolved choices and command gate', async t => {
  const before = { mcpServers: { argent: { command: 'old', args: ['old'] }, private: { token: 'private-token' } }, untouched: true };
  const after = { mcpServers: { argent: { command: 'new', args: ['new'] } } };
  const { engine, args } = await setup(t, ['mcp'], JSON.stringify(before), JSON.stringify(after));
  const initial = await engine.reviewFile(args);
  assert.equal(initial.format, 'json');
  assert.equal(initial.unresolved, 2);
  assert.deepEqual(JSON.parse(initial.after), before);
  const resolutions = { '/mcpServers/argent/command': 'source', '/mcpServers/argent/args': 'source' };
  const page = await engine.reviewFile({ ...args, resolutions });
  assert.equal(page.unresolved, 0);
  const expected = structuredClone(before); expected.mcpServers.argent = after.mcpServers.argent;
  assert.deepEqual(JSON.parse(page.after), expected);
  assert.equal((await engine.reviewFile(args)).unresolved, 2);
  await assert.rejects(engine.reviewFile({ ...args, resolutions: { '/untouched': 'source' } }), /INVALID_REVIEW_SELECTION/);
  const next = engine.resolve({ planId: args.planId, resolutions: { target: { mcp: resolutions } } });
  await assert.rejects(engine.apply({ planId: next.planId, targetIds: ['target'], approved: true, commandReviewApproved: true }), /COMMAND_REVIEW_REQUIRED/);
});

for (const doc of ['settings', 'subagents']) test(`${doc} JSON review exposes only selected incoming changes`, async t => {
  const field = doc === 'settings' ? 'theme' : 'maxConcurrent';
  const oldValue = doc === 'settings' ? 'dark' : 1;
  const newValue = doc === 'settings' ? 'light' : 2;
  const { engine, args } = await setup(t, [doc], JSON.stringify({ [field]: oldValue, device: 'local' }), JSON.stringify({ [field]: newValue }));
  const view = await engine.reviewFile({ ...args, resolutions: { ['/' + field]: 'source' } });
  assert.deepEqual(JSON.parse(view.after), { [field]: newValue, device: 'local' });
  assert.equal(view.unresolved, 0);
});

test('JSON review field selection remains narrow and uses immutable snapshots', async t => {
  const before = { theme: 'dark', defaultThinkingLevel: 'low', deviceId: 'target' };
  const after = { theme: 'light', defaultThinkingLevel: 'high' };
  const f = await setup(t, ['settings'], JSON.stringify(before), JSON.stringify(after));
  const p = await f.engine.preview({ ...f.input, fields: { settings: ['/theme'] } });
  const args = { ...f.args, planId: p.planId, resolutions: { '/theme': 'source' } };
  const first = await f.engine.reviewFile(args);
  assert.deepEqual(JSON.parse(first.after), { ...before, theme: 'light' });
  await assert.rejects(f.engine.reviewFile({ ...args, resolutions: { '/defaultThinkingLevel': 'source' } }), /INVALID_REVIEW_SELECTION/);
  for (const target of [f.source, f.target]) await fs.writeFile(path.join(target.agentDir, 'settings.json'), '{}');
  assert.deepEqual(await f.engine.reviewFile(args), first);
});

test('deep target-only JSON uses complete compact output instead of unbounded indentation', async t => {
  let nested = { leaf: 'retained' };
  for (let i = 0; i < 800; i++) nested = { child: nested };
  const before = { theme: 'dark', nested };
  const f = await setup(t, ['settings'], JSON.stringify(before), '{"theme":"light"}');
  const result = await collect(f.engine, { ...f.args, resolutions: { '/theme': 'source' } });
  assert.equal(result.first.format, 'json');
  assert.equal(result.after, JSON.stringify({ ...before, theme: 'light' }) + '\n');
  assert.ok(result.after.length < 12000);
});
