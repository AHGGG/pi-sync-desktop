// Only these reviewed leaves are shared. Unknown and device-local fields survive.
export const LOCAL_FIELDS = new Set(['deviceId', 'trackingId', 'shellPath',
  'shellCommandPrefix', 'defaultProvider', 'defaultModel', 'lastChangelogVersion']);
export const FIELDS = Object.freeze({
  '/theme': 'string', '/packages': 'array', '/extensions/builtin:mcp': 'array', '/defaultThinkingLevel': 'string',
  '/enableSkillCommands': 'boolean', '/treeFilterMode': 'string', '/tuiMode': 'string',
  '/fullscreenScrollbar': 'string', '/transport': 'string', '/httpIdleTimeoutMs': 'number',
  '/retry/enabled': 'boolean', '/retry/maxRetries': 'number', '/retry/baseDelayMs': 'number',
  '/hideThinkingBlock': 'boolean', '/fullscreenCopyOnSelect': 'boolean',
  '/showCacheMissNotices': 'boolean', '/markdown/mermaid': 'string',
  '/compaction/enabled': 'boolean', '/compaction/reserveTokens': 'number',
  '/compaction/keepRecentTokens': 'number', '/terminal/showImages': 'boolean',
  '/terminal/imageWidthCells': 'number', '/images/autoResize': 'boolean',
  '/steeringMode': 'string', '/doubleEscapeAction': 'string',
});
export const object = value => !!value && typeof value === 'object' && !Array.isArray(value);
export const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const parts = path => path.slice(1).split('/');
export function cell(value, path) {
  if (path === '/extensions/builtin:mcp') {
    const items = value.extensions;
    if (items === undefined) return {present: false};
    if (!Array.isArray(items) || items.some(x => typeof x !== 'string')) throw new Error('Target extensions must be a string array');
    const selected = items.filter(x => /^[+!\-]?builtin:mcp$/.test(x));
    return selected.length ? {present: true, value: selected} : {present: false};
  }
  for (const key of parts(path)) {
    if (!object(value) || !Object.hasOwn(value, key)) return {present: false};
    value = value[key];
  }
  return {present: true, value};
}
export function project(value) {
  return Object.fromEntries(Object.keys(FIELDS).map(path => [path, cell(value, path)]));
}
export function validateShared(value) {
  if (!object(value)) throw new Error('Shared settings must be a JSON object');
  function visit(node, prefix = '') {
    for (const [key, item] of Object.entries(node)) {
      const path = `${prefix}/${key}`;
      if (path === '/extensions') {
        if (!same(item, ['+builtin:mcp'])) throw new Error('Only the reviewed +builtin:mcp extension directive is shared');
        continue;
      }
      if (Object.hasOwn(FIELDS, path)) {
        const type = Array.isArray(item) ? 'array' : typeof item;
        if (type !== FIELDS[path] || (type === 'number' && (!Number.isFinite(item) || item < 0)))
          throw new Error(`Invalid shared field: ${path}`);
      } else if (object(item) && Object.keys(FIELDS).some(p => p.startsWith(`${path}/`))) {
        visit(item, path);
      } else throw new Error(`Unreviewed shared field: ${path}`);
    }
  }
  visit(value);
  if (value.packages) {
    const seen = new Set();
    for (const entry of value.packages) {
      const source = typeof entry === 'string' ? entry : entry?.source;
      // No private URLs, arbitrary commands, machine paths, or credentials in public settings.
      if (typeof source !== 'string' || !/^(npm:[@a-zA-Z0-9][a-zA-Z0-9@/._~-]*|git:github\.com\/[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+(?:@[a-zA-Z0-9_./-]+)?)$/.test(source))
        throw new Error('Only reviewed npm or public GitHub package sources are allowed');
      if (seen.has(source)) throw new Error('Duplicate package source');
      seen.add(source);
      if (typeof entry !== 'string') {
        if (!object(entry)) throw new Error('Invalid package entry');
        for (const [key, filters] of Object.entries(entry)) {
          if (key === 'source') continue;
          if (!['extensions', 'skills', 'prompts', 'themes'].includes(key) ||
              !Array.isArray(filters) || filters.some(f => typeof f !== 'string'))
            throw new Error('Unreviewed package option');
        }
      }
    }
  }
  return value;
}
export function mergeShared(shared, target, state, resolutions = {}) {
  return mergeDocument(shared, target, state, resolutions, FIELDS, validateShared);
}
export function mergeDocument(shared, target, state, resolutions = {}, fields = FIELDS, validate = validateShared) {
  validate(shared);
  if (!object(target)) throw new Error('Target settings must be an object');
  if (state !== null && (!object(state) || state.version !== 1 || !object(state.source)))
    throw new Error('Unsupported baseline; inspect it rather than assuming an ancestor');
  if (state) validate(state.source);
  if (!object(resolutions)) throw new Error('Resolutions must be an object');
  const result = structuredClone(target), conflicts = [], changes = [], kept = [], used = new Set();
  for (const path of Object.keys(fields)) {
    const incoming = cell(shared, path), current = cell(target, path);
    // A removed shared declaration relinquishes ownership; it does not delete target data.
    if (!incoming.present) continue;
    const previous = state ? cell(state.source, path) : {present: false};
    let chosen = current;
    let reason;
    if (same(incoming, current)) chosen = incoming;
    else if (!state || !previous.present) reason = 'first-run-review';
    else if (same(previous, current)) chosen = incoming;
    else if (same(previous, incoming)) kept.push(path);
    else reason = 'both-changed';
    if (reason || (!same(incoming, current) && Object.hasOwn(resolutions, path))) {
      const resolution = resolutions[path];
      if (resolution === 'source' || resolution === 'target' || resolution === 'skip') {
        chosen = resolution === 'source' ? incoming : current;
        used.add(path);
      } else if (resolution !== undefined) throw new Error(`Invalid resolution for ${path}`);
      else conflicts.push({path, reason, source: incoming, target: current, base: previous});
    }
    if (!same(chosen, current)) {
      if (path === '/extensions/builtin:mcp') {
        result.extensions = [...(result.extensions ?? []).filter(x => !/^[+!\-]?builtin:mcp$/.test(x)), ...chosen.value];
        changes.push({path, before: current, after: chosen});
        continue;
      }
      const keys = parts(path); let node = result;
      for (const key of keys.slice(0, -1)) {
        if (Object.hasOwn(node, key) && !object(node[key])) {
          throw new Error(`Target parent is not an object: /${key}; repair explicitly`);
        }
        if (!Object.hasOwn(node, key)) node[key] = {};
        node = node[key];
      }
      node[keys.at(-1)] = structuredClone(chosen.value);
      changes.push({path, before: current, after: chosen});
    }
  }
  for (const path of Object.keys(resolutions)) if (!used.has(path))
    throw new Error(`Resolution is stale or unknown: ${path}`);
  return {result, conflicts, changes, kept, state: {version: 1, source: structuredClone(shared)}};
}
