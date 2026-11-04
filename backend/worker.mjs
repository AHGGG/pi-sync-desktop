// Self-contained worker: transport may append `await workerMain()` to this source.
import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash, randomUUID } from 'node:crypto';

const MAX_BYTES = 4 * 1024 * 1024;
const own = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
const object = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const digest = (b) => createHash('sha256').update(b).digest('hex');
const fail = (code) => { const e = new Error(code); e.workerCode = code; throw e; };
const missing = (e) => e.code === 'ENOENT';

async function stat(file) {
  try { return await fs.lstat(file); } catch (e) { if (missing(e)) return null; throw e; }
}

// Refuse links in every component, including the supplied agent directory.
async function inspectPath(file, directory = false) {
  const parsed = path.parse(file);
  let current = parsed.root;
  const parts = file.slice(parsed.root.length).split(path.sep).filter(Boolean);
  for (let i = 0; i < parts.length; i++) {
    current = path.join(current, parts[i]);
    const s = await stat(current);
    if (!s) return;
    if (s.isSymbolicLink()) fail('SYMLINK_REJECTED');
    if ((i < parts.length - 1 || directory) && !s.isDirectory()) fail('NOT_DIRECTORY');
    if (i === parts.length - 1 && !directory && !s.isFile()) fail('NOT_REGULAR_FILE');
  }
}

async function safeLocation(request) {
  let dir = request.agentDir ?? '~/.pi/agent';
  if (typeof dir !== 'string' || !dir || dir.includes('\0')) fail('INVALID_AGENT_DIR');
  if (dir === '~' || dir.startsWith('~/') || dir.startsWith('~\\')) dir = path.join(os.homedir(), dir.slice(2));
  if (!path.isAbsolute(dir)) fail('INVALID_AGENT_DIR');
  dir = path.resolve(dir);
  if (dir === path.parse(dir).root) fail('INVALID_AGENT_DIR');
  await inspectPath(dir, true);
  if (request.forbiddenRoot) {
    if (typeof request.forbiddenRoot !== 'string' || !path.isAbsolute(request.forbiddenRoot)) fail('INVALID_FORBIDDEN_ROOT');
    const rel = path.relative(path.resolve(request.forbiddenRoot), dir);
    if (!rel || (!rel.startsWith(`..${path.sep}`) && rel !== '..' && !path.isAbsolute(rel))) fail('REPOSITORY_TARGET');
  }
  for (let current = dir;; current = path.dirname(current)) {
    const marker = path.join(current, '.git');
    const git = await stat(marker);
    // Empty scaffold directories are not Git repositories. Gitfiles and links are unsafe.
    if (git && (!git.isDirectory() || await stat(path.join(marker, 'HEAD')) || await stat(path.join(marker, 'config')))) fail('REPOSITORY_TARGET');
    if (current === path.dirname(current)) break;
  }
  return dir;
}

function parseObject(bytes) {
  if (bytes.length > MAX_BYTES) fail('FILE_TOO_LARGE');
  let value;
  try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
  catch { fail('INVALID_JSON'); }
  if (!object(value)) fail('INVALID_JSON_OBJECT');
  return value;
}

// Dependency-free, fail-closed subset of Pi's model-config/model-schema definitions.
// Unknown fields (including unsupported complex compat routing) require review, not copying.
const nonempty = (v) => typeof v === 'string' && v.trim().length > 0;
const number = (v) => typeof v === 'number' && Number.isFinite(v);
const positive = (v) => number(v) && v > 0;
const integer = (v) => positive(v) && Number.isInteger(v);
const boolean = (v) => typeof v === 'boolean';
const string = (v) => typeof v === 'string';
const choices = (...values) => (v) => values.includes(v);
const record = (test) => (v) => object(v) && Object.values(v).every(test);
const array = (test) => (v) => Array.isArray(v) && v.every(test);
const shape = (fields, required = []) => (v) => object(v)
  && required.every((key) => own(v, key))
  && Object.entries(v).every(([key, value]) => own(fields, key) && fields[key](value));
const levels = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
const levelMap = (test) => shape(Object.fromEntries(levels.map((key) => [key, test])));
const rates = { input: number, output: number, cacheRead: number, cacheWrite: number };
const cost = (partial = false) => shape({ ...rates, tiers: array(shape({ ...rates, inputTokensAbove: number }, [...Object.keys(rates), 'inputTokensAbove'])) }, partial ? [] : Object.keys(rates));
const inputLimits = shape({
  maxRequestBytes: integer,
  images: shape({ maxPerMessage: integer, maxPerRequest: integer, resize: shape({
    maxWidth: integer, maxHeight: integer, maxBytes: integer,
    jpegQuality: (v) => integer(v) && v <= 100,
  }) }),
});
const compatBooleans = [
  'supportsStore', 'supportsDeveloperRole', 'supportsReasoningEffort', 'supportsUsageInStreaming',
  'supportsFinishReason', 'requiresToolResultName', 'requiresAssistantAfterToolResult',
  'requiresThinkingAsText', 'requiresReasoningContentOnAssistantMessages', 'zaiToolStream',
  'supportsThinkingTokenBudget', 'supportsOpenAIGrammarTools', 'supportsMidConvoSystemMessages',
  'supportsMidConvoToolAdditions', 'supportsStrictMode', 'sendSessionAffinityHeaders',
  'supportsLongCacheRetention', 'supportsAdditionalTools', 'supportsToolSearch',
  'supportsExplicitPromptCacheMode', 'supportsMidConvoEffort', 'supportsMaxOutputTokens',
  'supportsEagerToolInputStreaming', 'supportsCacheControlOnTools', 'supportsTemperature',
  'forceAdaptiveThinking', 'allowEmptySignature', 'supportsStrictTools', 'supportsMidConvoToolChanges',
];
const compat = shape({
  ...Object.fromEntries(compatBooleans.map((key) => [key, boolean])),
  maxTokensField: choices('max_completion_tokens', 'max_tokens'),
  sessionAffinityFormat: choices('openai', 'openai-nosession', 'openrouter'),
  thinkingTokenBudgetField: choices('thinking_token_budget', 'thinking_budget', 'thinking_budget_tokens'),
  thinkingFormat: choices('openai', 'openrouter', 'deepseek', 'together', 'baseten', 'zai', 'qwen', 'chat-template', 'qwen-chat-template', 'string-thinking', 'ant-ling'),
  cacheControlFormat: choices('anthropic'),
  vllmPriority: number,
});
const modelFields = {
  name: nonempty, reasoning: boolean, headers: record(string), compat,
  thinkingLevelMap: levelMap((v) => v === null || string(v)),
  input: array(choices('text', 'image')), inputLimits,
  cost: cost(), promptCache: shape({ short: positive, long: positive }),
  contextWindow: positive, maxTokens: positive,
  samplingParams: object, samplingParamsByThinkingLevel: levelMap(object),
};
const modelDefinition = shape({ ...modelFields, id: nonempty, api: nonempty, baseUrl: nonempty }, ['id']);
const modelOverride = shape({ ...modelFields, cost: cost(true) });
const providerDefinition = shape({
  name: nonempty, baseUrl: nonempty, apiKey: nonempty, api: nonempty,
  oauth: choices('radius'), headers: record(string), compat, authHeader: boolean,
  models: (v) => array(modelDefinition)(v) && new Set(v.map((model) => model.id)).size === v.length,
  modelOverrides: record(modelOverride),
});

export function validateModels(value) {
  if (!shape({ $schema: string, providers: record(providerDefinition) }, ['providers'])(value)) fail('INVALID_MODELS');
  return value;
}

// Only compatibility metadata may leave the source during destination preview.
export function summarizeModels(value) {
  return { providers: Object.fromEntries(Object.entries(value?.providers ?? {}).map(([name, provider]) => [name, {
    models: (provider.models ?? []).map(model => ({ id: model.id })),
  }])) };
}
function validateModelSummary(value) {
  if (!shape({ providers: record(shape({ models: array(shape({ id: nonempty }, ['id'])) }, ['models'])) }, ['providers'])(value)) fail('INVALID_MODEL_SUMMARY');
  return value;
}

export function assertDefaultCompatible(models, settings, previousModels) {
  if (!settings || (!own(settings, 'defaultProvider') && !own(settings, 'defaultModel'))) fail('DEFAULT_MODEL_UNVERIFIED');
  const provider = settings.defaultProvider;
  const model = settings.defaultModel;
  // If neither custom file touches this provider, this replacement cannot affect its
  // built-in/extension registry. Preserve the existing selection without executing Pi.
  // This proves non-interference, not runtime availability or credential validity.
  if (typeof provider === 'string' && provider.trim() && typeof model === 'string' && model.trim()
    && previousModels !== undefined && !own(models.providers, provider)
    && !own(previousModels?.providers ?? {}, provider)) return;
  // Changed custom providers must explicitly retain the configured default model.
  if (typeof provider !== 'string' || !provider.trim() || typeof model !== 'string' || !model.trim()
    || !own(models.providers, provider) || !models.providers[provider].models?.some((entry) => entry.id === model)) {
    fail('DEFAULT_MODEL_UNVERIFIED');
  }
}

async function read(file) {
  await inspectPath(file);
  let handle;
  try {
    handle = await fs.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    const s = await handle.stat();
    if (!s.isFile()) fail('NOT_REGULAR_FILE');
    if (s.size > MAX_BYTES) fail('FILE_TOO_LARGE');
    const bytes = await handle.readFile();
    const value = parseObject(bytes);
    return { exists: true, hash: digest(bytes), value, bytes };
  } catch (e) {
    if (missing(e)) return { exists: false, hash: null, value: null, bytes: null };
    throw e;
  } finally { await handle?.close(); }
}

function publicRecord(record, content = true) {
  return { exists: record.exists, hash: record.hash, ...(content ? { value: record.value } : {}) };
}

async function privateDirs(dir) {
  await inspectPath(dir, true);
  const parent = path.dirname(dir);
  if (!(await stat(dir))) {
    if (parent !== dir) await privateDirs(parent);
    try { await fs.mkdir(dir, { mode: 0o700 }); }
    catch (e) { if (e.code !== 'EEXIST') throw e; }
    await inspectPath(dir, true);
  }
}

async function exclusiveWrite(file, bytes) {
  await inspectPath(path.dirname(file), true);
  const handle = await fs.open(file, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600);
  try { await handle.writeFile(bytes); await handle.sync(); }
  finally { await handle.close(); }
}

function encode(value) {
  if (!object(value)) fail('INVALID_JSON_OBJECT');
  let bytes;
  try { bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`); }
  catch { fail('INVALID_JSON'); }
  parseObject(bytes);
  return bytes;
}

/** Settings and state may be returned; models bytes and errors never contain file content. */
export async function handle(request) {
  const applied = [];
  let backupPath;
  let lock;
  const temps = new Set();
  try {
    if (!object(request) || !['snapshot', 'export-models', 'check-models', 'apply'].includes(request.op)) fail('INVALID_REQUEST');
    const dir = await safeLocation(request);
    const meta = path.join(dir, '.harness-sync');
    const files = { settings: path.join(dir, 'settings.json'), state: path.join(meta, 'shared-state.json'), models: path.join(dir, 'models.json'), mcp: path.join(dir, 'mcp.json'), subagents: path.join(dir, 'subagents.json') };
    if (request.op === 'export-models') {
      const record = await read(files.models);
      if (!record.exists) fail('SOURCE_MODELS_MISSING');
      validateModels(record.value);
      return { ok: true, hash: record.hash, modelsBase64: record.bytes.toString('base64') };
    }
    const includeModels = request.op === 'check-models' || request.includeModels === true || own(request, 'modelsBase64') || (object(request.expected) && own(request.expected, 'models'));
    if (request.documents !== undefined && (!Array.isArray(request.documents) || request.documents.some(k => !['settings', 'mcp', 'subagents'].includes(k)))) fail('INVALID_DOCUMENT_SELECTION');
    const optional = ['mcp', 'subagents'].filter((k) => (request.op === 'snapshot' && (request.documents === undefined || request.documents.includes(k))) || own(request, k) || (object(request.expected) && own(request.expected, k)));
    const keys = ['settings', 'state', ...optional, ...(includeModels ? ['models'] : [])];
    const readAll = async () => Object.fromEntries(await Promise.all(keys.map(async (key) => {
      const record = await read(files[key]);
      if (key === 'models' && record.exists) validateModels(record.value);
      return [key, record];
    })));
    let previous = await readAll();
    if (request.op === 'snapshot') {
      const snapshot = { ok: true, ...Object.fromEntries(keys.map((k) => [k, publicRecord(!previous[k].exists && ['mcp', 'subagents'].includes(k) ? { ...previous[k], value: {} } : previous[k], k !== 'models')])) };
      if (includeModels && request.includeModelSummary === true) snapshot.models.summary = summarizeModels(previous.models.value);
      return snapshot;
    }
    const writes = {};
    let candidateModels;
    if (request.op === 'check-models') {
      if (['modelsBase64', 'settings', 'state', 'mcp', 'subagents'].some(key => own(request, key))) fail('INVALID_MODEL_SUMMARY');
      candidateModels = validateModelSummary(request.modelSummary);
    }
    for (const key of ['settings', 'state', 'mcp', 'subagents']) if (own(request, key)) writes[key] = encode(request[key]);
    if (own(request, 'modelsBase64')) {
      if (typeof request.modelsBase64 !== 'string' || request.modelsBase64.length > Math.ceil(MAX_BYTES / 3) * 4 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(request.modelsBase64)) fail('INVALID_MODELS');
      const bytes = Buffer.from(request.modelsBase64, 'base64');
      try { candidateModels = validateModels(parseObject(bytes)); } catch { fail('INVALID_MODELS'); }
      writes.models = bytes;
    }
    if (!object(request.expected)) fail('EXPECTED_HASH_REQUIRED');
    const checkExpected = (records) => {
      for (const k of keys) {
        if (!own(request.expected, k) || (request.expected[k] !== null && !/^[a-f0-9]{64}$/.test(request.expected[k]))) fail('EXPECTED_HASH_REQUIRED');
        if (records[k].hash !== request.expected[k]) fail('STALE_SNAPSHOT');
      }
    };
    checkExpected(previous);
    if (candidateModels && (request.op === 'check-models' || digest(writes.models) !== previous.models.hash)) {
      assertDefaultCompatible(candidateModels, previous.settings.value, previous.models.value);
      if (own(writes, 'settings')) assertDefaultCompatible(candidateModels, request.settings, previous.models.value);
    }
    if (request.op === 'check-models') return { ok: true };
    await privateDirs(meta);
    const lockPath = path.join(meta, 'lock');
    await inspectPath(lockPath, true);
    try { await fs.mkdir(lockPath, { mode: 0o700 }); lock = lockPath; }
    catch (e) { if (e.code === 'EEXIST') fail('LOCK_BUSY'); throw e; }
    await safeLocation(request);
    previous = await readAll();
    checkExpected(previous);
    // A baseline must never acknowledge a shared file that has not been installed.
    const changed = ['settings', 'mcp', 'subagents', 'models', 'state'].filter((k) => own(writes, k) && digest(writes[k]) !== previous[k].hash);
    if (!changed.length) return { ok: true, applied: [], backupPath: null };
    // POSIX modes do not establish Windows ACLs; callers must use a private user profile.
    await fs.chmod(dir, 0o700);
    await fs.chmod(meta, 0o700);
    const backups = path.join(meta, 'backups');
    await privateDirs(backups);
    await fs.chmod(backups, 0o700);
    backupPath = path.join(backups, randomUUID());
    await fs.mkdir(backupPath, { mode: 0o700 });
    const manifest = {};
    for (const k of (request.retainOneBackup === true ? keys : changed)) {
      manifest[k] = { exists: previous[k].exists, hash: previous[k].hash };
      if (previous[k].exists) await exclusiveWrite(path.join(backupPath, `${k}.json`), previous[k].bytes);
    }
    await exclusiveWrite(path.join(backupPath, 'manifest.json'), encode(manifest));
    for (const k of changed) {
      const tmp = path.join(path.dirname(files[k]), `.harness-${randomUUID()}.tmp`);
      temps.add(tmp);
      await exclusiveWrite(tmp, writes[k]);
      await safeLocation(request);
      // This narrows but cannot eliminate races with Pi or other non-cooperating writers.
      if (k === 'models') {
        const settingsNow = await read(files.settings);
        const settingsHash = own(writes, 'settings') ? digest(writes.settings) : previous.settings.hash;
        if (settingsNow.hash !== settingsHash) fail('CONCURRENT_UPDATE');
        assertDefaultCompatible(candidateModels, settingsNow.value, previous.models.value);
      }
      const current = await read(files[k]);
      if (current.hash !== previous[k].hash) fail('CONCURRENT_UPDATE');
      await fs.rename(tmp, files[k]);
      temps.delete(tmp);
      const result = { file: k, hash: digest(writes[k]), backupPath: previous[k].exists ? path.join(backupPath, `${k}.json`) : null, verified: false };
      applied.push(result);
      const verified = await read(files[k]);
      if (verified.hash !== result.hash) fail('VERIFY_FAILED');
      result.verified = true;
    }
    // Prune only after every write verifies, retaining recovery evidence on partial failures.
    if (request.retainOneBackup === true) {
      for (const entry of await fs.readdir(backups, { withFileTypes: true })) {
        if (entry.name === path.basename(backupPath)) continue;
        if (!entry.isDirectory() || !/^[a-f0-9-]{36}$/.test(entry.name)) fail('BACKUP_CLEANUP_REQUIRED');
        const old = path.join(backups, entry.name);
        await inspectPath(old, true);
        const manifest = await read(path.join(old, 'manifest.json'));
        if (!manifest.exists || Object.keys(manifest.value).some(k => !Object.hasOwn(files, k))) fail('BACKUP_CLEANUP_REQUIRED');
        const entries = await fs.readdir(old, { withFileTypes: true });
        for (const f of entries) {
          if (!f.isFile() || !['manifest.json', ...Object.keys(files).map(k => `${k}.json`)].includes(f.name)) fail('BACKUP_CLEANUP_REQUIRED');
          await inspectPath(path.join(old, f.name));
        }
        for (const f of entries) await fs.unlink(path.join(old, f.name));
        await fs.rmdir(old);
      }
    }
    return { ok: true, applied, backupPath, atomic: false };
  } catch (e) {
    return { ok: false, error: { code: e.workerCode ?? 'IO_ERROR' }, applied, ...(backupPath ? { backupPath } : {}), partial: applied.length > 0 };
  } finally {
    for (const tmp of temps) await fs.unlink(tmp).catch(() => {});
    if (lock) await fs.rmdir(lock).catch(() => {});
  }
}

export async function workerMain() {
  let response;
  try {
    const chunks = [];
    let length = 0;
    for await (const chunk of process.stdin) {
      length += chunk.length;
      if (length > MAX_BYTES * 4) fail('REQUEST_TOO_LARGE');
      chunks.push(chunk);
    }
    response = await handle(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  } catch { response = { ok: false, error: { code: 'INVALID_REQUEST' }, applied: [], partial: false }; }
  process.stdout.write(`${JSON.stringify(response)}\n`);
  if (!response.ok) process.exitCode = 1;
}
