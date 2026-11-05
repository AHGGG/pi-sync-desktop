// Self-contained worker: transport may append `await workerMain()` to this source.
import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash, randomUUID } from 'node:crypto';

// Bound the private JSON envelope independently of per-file limits. Audited documents,
// models and custom bytes can coexist, including base64 expansion and metadata.
export const MAX_PROTOCOL_BYTES = 48 * 1024 * 1024;
const MAX_BYTES = 4 * 1024 * 1024;
const own = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
const object = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const digest = (b) => createHash('sha256').update(b).digest('hex');
const fail = (code) => { const e = new Error(code); e.workerCode = code; throw e; };
const missing = (e) => e.code === 'ENOENT';

// Custom selections deliberately cannot bypass audited configuration or permission boundaries.
const MAX_CUSTOM_FILES = 256;
const MAX_CUSTOM_BYTES = 8 * 1024 * 1024;
export function validateRelativePath(value, allowProtected = false) {
  if (typeof value !== 'string' || !value || value.length > 1024 || /[\\\x00-\x1f\x7f:?*<>|\"]/.test(value) || value.startsWith('/') || value.split('/').some(p => !p || p === '.' || p === '..')) fail('INVALID_CUSTOM_PATH');
  const parts = value.split('/');
  for (const part of parts) {
    const p = part.normalize('NFKC').toLowerCase();
    if (/[\\/\x00-\x1f\x7f:?*<>|\"]/.test(p) || p === '.' || p === '..') fail('INVALID_CUSTOM_PATH');
    if (['__proto__', 'prototype', 'constructor'].includes(p) || /[. ]$/.test(p) || /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/.test(p)) fail('INVALID_CUSTOM_PATH');
    if (!allowProtected && (p.startsWith('.') || /credential|password|secret|(?:^|[._-])(?:oauth|authentication|authorization)(?:[._-]|$)/i.test(p) || /^(auth|credentials?|secrets?|tokens?|passwords?)([._-]|$)/i.test(p) || /(?:^|[._-])(credentials?|secrets?|tokens?)(?:[._-]|$)/i.test(p) || /^(id_rsa|id_ed25519|id_ecdsa|known_hosts|authorized_keys)$/.test(p) || /\.(pem|key|p12|pfx|env)$/i.test(p))) fail('PROTECTED_CUSTOM_PATH');
  }
  if (!allowProtected && ['settings.json', 'mcp.json', 'subagents.json', 'models.json'].includes(parts[0].normalize('NFKC').toLowerCase())) fail('AUDITED_DOCUMENT_REQUIRED');
  return value;
}
// Reject names that alias on case-insensitive or normalization-insensitive filesystems.
// Prefixes matter too: A/x and a/y collide even though their complete paths differ.
export function validatePathCollisions(paths, filesOnly = false) {
  const prefixes = new Map();
  const fileNames = new Set(paths);
  for (const rel of paths) {
    validateRelativePath(rel);
    const parts = rel.split('/');
    for (let i = 1; i <= parts.length; i++) {
      const prefix = parts.slice(0, i).join('/');
      const key = prefix.normalize('NFKC').toLowerCase();
      if (prefixes.has(key) && prefixes.get(key) !== prefix) fail('CUSTOM_PATH_COLLISION');
      if (filesOnly && i < parts.length && fileNames.has(prefix)) fail('CUSTOM_PATH_COLLISION');
      prefixes.set(key, prefix);
    }
  }
}
function checkedPaths(paths) {
  if (!Array.isArray(paths) || paths.length > MAX_CUSTOM_FILES) fail('INVALID_CUSTOM_SELECTION');
  const selected = [...new Set(paths.map(validateRelativePath))].sort();
  validatePathCollisions(selected);
  return selected;
}
export const CONTENT_LIMITS = Object.freeze({ fileBytes: MAX_BYTES, totalBytes: MAX_CUSTOM_BYTES, files: MAX_CUSTOM_FILES, entries: 4096, depth: 32 });
// Raw code-unit order is total and independent of locale/ICU collation.
const pathOrder = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const portable = rel => rel.normalize('NFKC').toLowerCase();
export function validateContentRules(rules = {}) {
  if (!object(rules) || Object.keys(rules).length > CONTENT_LIMITS.entries) fail('INVALID_CONTENT_RULES');
  const seen = new Set();
  for (const [rel, included] of Object.entries(rules)) {
    if (typeof included !== 'boolean') fail('INVALID_CONTENT_RULES');
    validateRelativePath(rel, !included);
    const key = portable(rel);
    if (seen.has(key)) fail('INVALID_CONTENT_RULES');
    seen.add(key);
  }
  return rules;
}
function contentSelection(rules) {
  const entries = Object.entries(validateContentRules(rules)).map(([rel, include]) => [portable(rel), include]);
  const included = rel => {
    const key = portable(rel);
    let result = true, length = -1;
    for (const [prefix, include] of entries) if ((key === prefix || key.startsWith(prefix + '/')) && prefix.length > length) {
      result = include; length = prefix.length;
    }
    return result;
  };
  return { included, traverse: rel => included(rel) || entries.some(([key, include]) => include && key.startsWith(portable(rel) + '/')) };
}
const limitFail = (rel, size, kind) => {
  const error = new Error(kind === 'fileBytes' ? 'FILE_TOO_LARGE' : 'CUSTOM_SELECTION_TOO_LARGE');
  error.workerCode = error.message;
  error.detail = { path: rel, size, limit: CONTENT_LIMITS[kind], kind };
  throw error;
};
// Iterate directory entries without allocating an unbounded directory listing.
async function children(file, visit) {
  const handle = await fs.opendir(file);
  for await (const entry of handle) await visit(entry.name);
}
const CUSTOM_DIRECTORIES = Symbol("customDirectories");
async function customSnapshot(dir, selections, required = false, rules = {}) {
  const selection = contentSelection(rules);
  const records = Object.create(null), observedPaths = new Set(), directories = [];
  records[CUSTOM_DIRECTORIES] = directories;
  let count = 0, total = 0, visited = 0, scanned = 0;
  async function visit(rel, depth = 0) {
    // Excluded branches are never inspected, validated for protection, read or hashed.
    if (!selection.traverse(rel)) return;
    if (++visited > CONTENT_LIMITS.entries || depth > CONTENT_LIMITS.depth) fail('CUSTOM_SELECTION_TOO_LARGE');
    validateRelativePath(rel);
    if (observedPaths.has(rel)) return;
    observedPaths.add(rel);
    validatePathCollisions([...observedPaths]);
    const file = path.join(dir, rel);
    await inspectPath(path.dirname(file), true);
    await rejectRepositoryAncestors(path.dirname(file));
    const info = await stat(file);
    if (!info) { if (required) fail('SOURCE_CUSTOM_PATH_MISSING'); return; }
    if (info.isSymbolicLink()) fail('SYMLINK_REJECTED');
    if (info.isDirectory()) {
      directories.push(rel);
      await rejectRepositoryAncestors(file);
      await children(file, async entry => {
        if (++scanned > CONTENT_LIMITS.entries) fail('CUSTOM_SELECTION_TOO_LARGE');
        await visit(rel + '/' + entry, depth + 1);
      });
      return;
    }
    if (!selection.included(rel)) return;
    if (info.size > MAX_BYTES) limitFail(rel, info.size, 'fileBytes');
    if (count + 1 > MAX_CUSTOM_FILES) limitFail(rel, count + 1, 'files');
    if (total + info.size > MAX_CUSTOM_BYTES) limitFail(rel, total + info.size, 'totalBytes');
    const record = await read(file, true, rel);
    if (!record.exists) fail('CUSTOM_PATH_CHANGED');
    if ((total += record.bytes.length) > MAX_CUSTOM_BYTES) limitFail(rel, total, 'totalBytes');
    count++;
    records[rel] = record;
  }
  for (const rel of checkedPaths(selections)) await visit(rel);
  return records;
}
async function contents(dir, selections, rules = {}) {
  const selection = contentSelection(rules);
  const entries = [], seen = new Set();
  async function visit(rel, depth = 0) {
    if (seen.has(rel)) return;
    if (entries.length >= CONTENT_LIMITS.entries || depth > CONTENT_LIMITS.depth) fail('CONTENT_INVENTORY_TOO_LARGE');
    seen.add(rel);
    const row = { path: rel, kind: 'blocked', size: null };
    entries.push(row);
    try {
      validateRelativePath(rel);
      const file = path.join(dir, rel);
      await inspectPath(path.dirname(file), true);
      await rejectRepositoryAncestors(path.dirname(file));
      const info = await stat(file);
      if (!info) { row.kind = 'missing'; return; }
      if (info.isSymbolicLink()) fail('SYMLINK_REJECTED');
      if (!info.isDirectory() && !info.isFile()) fail('NOT_REGULAR_FILE');
      if (info.isDirectory()) await rejectRepositoryAncestors(file);
      row.kind = info.isDirectory() ? 'directory' : 'file';
      row.size = info.isFile() ? info.size : null;
      const name = path.posix.basename(rel).toLowerCase();
      if (/^(logs?|.*\.log)$/.test(name)) row.suggestedExclude = 'logs';
      else if (/^(cache|caches|node_modules|__pycache__)$/.test(name)) row.suggestedExclude = 'cache';
      if (info.isDirectory()) {
        if (!selection.traverse(rel)) row.omitted = true;
        else await children(file, name => visit(rel + '/' + name, depth + 1));
      }
    } catch (e) {
      if (e.workerCode === 'CONTENT_INVENTORY_TOO_LARGE') throw e;
      row.kind = 'blocked'; row.size = null;
      row.blocked = e.workerCode ?? 'IO_ERROR';
    }
  }
  let error;
  try { for (const rel of checkedPaths(selections)) await visit(rel); }
  catch (e) { error = e.workerCode ?? 'IO_ERROR'; }
  return { ok: true, ...(error ? { error } : {}), entries: entries.sort((a, b) => pathOrder(a.path, b.path)), limits: CONTENT_LIMITS };
}
const customPublic = records => Object.fromEntries(Object.entries(records).sort(([a], [b]) => pathOrder(a, b)).map(([rel, r]) => [rel, { hash: r.hash, base64: r.bytes.toString('base64') }]));
const backupName = key => key.startsWith('path:') ? `custom-${digest(key)}.bin` : `${key}.json`;

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

async function rejectRepositoryAncestors(dir) {
  for (let current = dir;; current = path.dirname(current)) {
    const marker = path.join(current, '.git');
    const git = await stat(marker);
    // Empty scaffold directories are not Git repositories. Gitfiles and links are unsafe.
    if (git && (!git.isDirectory() || await stat(path.join(marker, 'HEAD')) || await stat(path.join(marker, 'config')))) fail('REPOSITORY_TARGET');
    if (current === path.dirname(current)) break;
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
  await rejectRepositoryAncestors(dir);
  return dir;
}

function parseJson(bytes) {
  if (bytes.length > MAX_BYTES) fail('FILE_TOO_LARGE');
  let value;
  try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
  catch { fail('INVALID_JSON'); }
  return value;
}

function parseObject(bytes) {
  const value = parseJson(bytes);
  if (!object(value)) fail('INVALID_JSON_OBJECT');
  return value;
}

function validBase64(encoded) {
  if (typeof encoded !== 'string' || encoded.length > Math.ceil(MAX_BYTES / 3) * 4 || encoded.length % 4 !== 0) return false;
  const padding = encoded.indexOf('=');
  const content = padding === -1 ? encoded : encoded.slice(0, padding);
  if (padding !== -1 && !['=', '=='].includes(encoded.slice(padding))) return false;
  return !/[^A-Za-z0-9+/]/.test(content);
}

// Models are private configuration bytes, not a schema this sync tool owns.
// Validate only readable UTF-8 JSON; Pi remains responsible for its runtime semantics.
export function decodeModels(encoded) {
  if (!validBase64(encoded)) fail('INVALID_MODELS_ENCODING');
  const bytes = Buffer.from(encoded, 'base64');
  parseJson(bytes);
  return bytes;
}

async function read(file, raw = false, customRel) {
  await inspectPath(file);
  let handle;
  try {
    handle = await fs.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    const s = await handle.stat();
    if (!s.isFile()) fail('NOT_REGULAR_FILE');
    if (s.size > MAX_BYTES) { if (customRel) limitFail(customRel, s.size, 'fileBytes'); fail('FILE_TOO_LARGE'); }
    const bytes = await handle.readFile();
    if (bytes.length > MAX_BYTES) { if (customRel) limitFail(customRel, bytes.length, 'fileBytes'); fail('FILE_TOO_LARGE'); }
    const value = raw ? undefined : parseObject(bytes);
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
    if (!object(request) || !['contents', 'snapshot', 'export-models', 'review-models', 'apply'].includes(request.op)) fail('INVALID_REQUEST');
    const dir = await safeLocation(request);
    if (request.op === 'contents') return await contents(dir, request.paths ?? [], request.contentRules);
    const rules = validateContentRules(request.contentRules);
    const selection = contentSelection(rules);
    const meta = path.join(dir, '.harness-sync');
    const files = { settings: path.join(dir, 'settings.json'), state: path.join(meta, 'shared-state.json'), models: path.join(dir, 'models.json'), mcp: path.join(dir, 'mcp.json'), subagents: path.join(dir, 'subagents.json') };
    if (request.op === 'review-models') {
      if (request.approved !== true) fail('APPROVAL_REQUIRED');
      if (!own(request, 'expectedHash') || (request.expectedHash !== null &&
          (typeof request.expectedHash !== 'string' || !/^[a-f0-9]{64}$/.test(request.expectedHash))))
        fail('EXPECTED_HASH_REQUIRED');
      const record = await read(files.models, true);
      if (record.hash !== request.expectedHash) fail('TARGET_CHANGED');
      return { ok: true, hash: record.hash, modelsBase64: record.bytes?.toString('base64') ?? '' };
    }
    if (request.op === 'export-models') {
      const record = await read(files.models, true);
      if (!record.exists) fail('SOURCE_MODELS_MISSING');
      try { parseJson(record.bytes); } catch (e) {
        if (e.workerCode === 'INVALID_JSON') fail('SOURCE_MODELS_INVALID_JSON');
        throw e;
      }
      return { ok: true, hash: record.hash, modelsBase64: record.bytes.toString('base64') };
    }
    const includeModels = request.includeModels === true || own(request, 'modelsBase64') || (object(request.expected) && own(request.expected, 'models'));
    if (request.documents !== undefined && (!Array.isArray(request.documents) || request.documents.some(k => !['settings', 'mcp', 'subagents'].includes(k)))) fail('INVALID_DOCUMENT_SELECTION');
    const optional = ['mcp', 'subagents'].filter((k) => (request.op === 'snapshot' && (request.documents === undefined || request.documents.includes(k))) || own(request, k) || (object(request.expected) && own(request.expected, k)));
    const includeDocuments = request.op === 'snapshot'
      ? request.documents === undefined || request.documents.length > 0
      : ['settings', 'state', 'mcp', 'subagents'].some(k => own(request, k) || (object(request.expected) && own(request.expected, k)));
    const keys = [...(includeDocuments ? ['settings', 'state'] : []), ...optional, ...(includeModels ? ['models'] : [])];
    const readAll = async () => Object.fromEntries(await Promise.all(keys.map(async (key) => {
      try {
        if (key.startsWith('path:')) await rejectRepositoryAncestors(path.dirname(files[key]));
        return [key, await read(files[key], key === 'models' || key.startsWith('path:'))];
      }
      catch (e) {
        if (key === 'models' && !e.workerCode) fail('TARGET_MODELS_UNREADABLE');
        throw e;
      }
    })));
    const selections = checkedPaths(request.paths ?? []);
    let custom = await customSnapshot(dir, selections, request.requirePaths === true, rules);
    let previous = await readAll();
    if (request.op === 'snapshot') {
      const snapshot = { ok: true, ...(selections.length ? { customFiles: customPublic(custom), customDirectories: custom[CUSTOM_DIRECTORIES].sort() } : {}), ...Object.fromEntries(keys.map((k) => [k, publicRecord(!previous[k].exists && ['mcp', 'subagents'].includes(k) ? { ...previous[k], value: {} } : previous[k], k !== 'models')])) };
      return snapshot;
    }
    const writes = {};
    for (const key of ['settings', 'state', 'mcp', 'subagents']) if (own(request, key)) writes[key] = encode(request[key]);
    if (own(request, 'modelsBase64')) {
      const bytes = decodeModels(request.modelsBase64);
      writes.models = bytes;
    }
    if (!object(request.expected)) fail('EXPECTED_HASH_REQUIRED');
    if (request.fileWrites !== undefined && !object(request.fileWrites)) fail('INVALID_CUSTOM_WRITES');
    const expectedFiles = request.expectedFiles ?? {};
    if (!object(expectedFiles)) fail('EXPECTED_HASH_REQUIRED');
    validatePathCollisions(Object.keys(expectedFiles), true);
    const withinSelection = rel => selections.some(root => rel === root || rel.startsWith(root + '/'));
    for (const rel of Object.keys(expectedFiles)) {
      validateRelativePath(rel);
      if (!withinSelection(rel) || !selection.included(rel)) fail('INVALID_CUSTOM_SELECTION');
      const k = 'path:' + rel;
      files[k] = path.join(dir, rel);
      keys.push(k);
      request.expected[k] = expectedFiles[rel];
    }
    const addCustom = (records, scanned) => {
      if (request.expectedDirectories !== undefined && JSON.stringify([...scanned[CUSTOM_DIRECTORIES]].sort()) !== JSON.stringify(request.expectedDirectories)) fail("STALE_SNAPSHOT");
      if (Object.keys(scanned).some(rel => !own(expectedFiles, rel))) fail('STALE_SNAPSHOT');
      for (const rel of Object.keys(expectedFiles)) records['path:' + rel] = own(scanned, rel) ? scanned[rel] : { exists: false, hash: null, bytes: null };
      return records;
    };
    previous = addCustom(previous, custom);
    let writeTotal = 0;
    for (const [rel, encoded] of Object.entries(request.fileWrites ?? {})) {
      validateRelativePath(rel);
      if (!own(expectedFiles, rel) || !validBase64(encoded)) fail('INVALID_CUSTOM_WRITES');
      const bytes = Buffer.from(encoded, 'base64');
      if (bytes.length > MAX_BYTES || (writeTotal += bytes.length) > MAX_CUSTOM_BYTES) fail('CUSTOM_SELECTION_TOO_LARGE');
      writes['path:' + rel] = bytes;
    }
    if (Object.keys(expectedFiles).length > MAX_CUSTOM_FILES) fail('CUSTOM_SELECTION_TOO_LARGE');
    const checkExpected = (records) => {
      for (const k of keys) {
        if (!own(request.expected, k) || (request.expected[k] !== null && !/^[a-f0-9]{64}$/.test(request.expected[k]))) fail('EXPECTED_HASH_REQUIRED');
        if (records[k].hash !== request.expected[k]) fail('STALE_SNAPSHOT');
      }
    };
    checkExpected(previous);
    await privateDirs(meta);
    const lockPath = path.join(meta, 'lock');
    await inspectPath(lockPath, true);
    try { await fs.mkdir(lockPath, { mode: 0o700 }); lock = lockPath; }
    catch (e) { if (e.code === 'EEXIST') fail('LOCK_BUSY'); throw e; }
    await safeLocation(request);
    previous = addCustom(await readAll(), await customSnapshot(dir, selections, false, rules));
    checkExpected(previous);
    // A baseline must never acknowledge a shared file that has not been installed.
    const changed = [...keys.filter(k => k !== 'state'), ...(keys.includes('state') ? ['state'] : [])].filter((k) => own(writes, k) && digest(writes[k]) !== previous[k].hash);
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
      if (previous[k].exists) await exclusiveWrite(path.join(backupPath, backupName(k)), previous[k].bytes);
    }
    await exclusiveWrite(path.join(backupPath, 'manifest.json'), encode(manifest));
    for (const k of changed) {
      if (k.startsWith('path:')) await rejectRepositoryAncestors(path.dirname(files[k]));
      await privateDirs(path.dirname(files[k]));
      const tmp = path.join(path.dirname(files[k]), `.harness-${randomUUID()}.tmp`);
      temps.add(tmp);
      await exclusiveWrite(tmp, writes[k]);
      await safeLocation(request);
      // This narrows but cannot eliminate races with Pi or other non-cooperating writers.
      if (k === 'models' && previous.settings) {
        const settingsNow = await read(files.settings);
        const settingsHash = own(writes, 'settings') ? digest(writes.settings) : previous.settings.hash;
        if (settingsNow.hash !== settingsHash) fail('CONCURRENT_UPDATE');
      }
      if (k.startsWith('path:')) await rejectRepositoryAncestors(path.dirname(files[k]));
      const current = await read(files[k], k === 'models' || k.startsWith('path:'));
      if (current.hash !== previous[k].hash) fail('CONCURRENT_UPDATE');
      if (k.startsWith('path:')) await rejectRepositoryAncestors(path.dirname(files[k]));
      await fs.rename(tmp, files[k]);
      temps.delete(tmp);
      const result = { file: k, hash: digest(writes[k]), backupPath: previous[k].exists ? path.join(backupPath, backupName(k)) : null, verified: false };
      applied.push(result);
      if (k.startsWith('path:')) await rejectRepositoryAncestors(path.dirname(files[k]));
      const verified = await read(files[k], k === 'models' || k.startsWith('path:'));
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
        if (!manifest.exists || Object.keys(manifest.value).some(k => !['settings', 'state', 'mcp', 'subagents', 'models'].includes(k) && (!k.startsWith('path:') || !validateRelativePath(k.slice(5))))) fail('BACKUP_CLEANUP_REQUIRED');
        const entries = await fs.readdir(old, { withFileTypes: true });
        for (const f of entries) {
          if (!f.isFile() || !['manifest.json', ...Object.keys(manifest.value).map(k => backupName(k))].includes(f.name)) fail('BACKUP_CLEANUP_REQUIRED');
          await inspectPath(path.join(old, f.name));
        }
        for (const f of entries) await fs.unlink(path.join(old, f.name));
        await fs.rmdir(old);
      }
    }
    return { ok: true, applied, backupPath, atomic: false };
  } catch (e) {
    return { ok: false, error: { ...(e.detail ? { detail: e.detail } : {}), code: e.workerCode ?? ({ EACCES: 'PERMISSION_DENIED', EPERM: 'PERMISSION_DENIED', ENOSPC: 'DISK_FULL', EROFS: 'READ_ONLY_FILESYSTEM' })[e.code] ?? 'IO_ERROR' }, applied, ...(backupPath ? { backupPath } : {}), partial: applied.length > 0 };
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
      if (length > MAX_PROTOCOL_BYTES) fail('REQUEST_TOO_LARGE');
      chunks.push(chunk);
    }
    response = await handle(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  } catch { response = { ok: false, error: { code: 'INVALID_REQUEST' }, applied: [], partial: false }; }
  process.stdout.write(`${JSON.stringify(response)}\n`);
  if (!response.ok) process.exitCode = 1;
}
