import {FIELDS, object, validateShared, mergeDocument} from './merge.mjs';

export const SCHEMAS = {
  settings: FIELDS,
  mcp: Object.fromEntries(['command', 'args', 'description', 'exposure', 'timeout']
    .map(k => [`/mcpServers/argent/${k}`, k === 'args' ? 'array' : k === 'timeout' ? 'number' : 'string'])),
  subagents: {
    '/maxConcurrent': 'number', '/defaultMaxTurns': 'number', '/graceTurns': 'number',
    '/defaultJoinMode': 'string', '/schedulingEnabled': 'boolean', '/scopeModels': 'boolean',
    '/disableDefaultAgents': 'boolean', '/toolDescriptionMode': 'string', '/fleetView': 'boolean',
    '/widgetMode': 'string', '/outputTranscript': 'boolean',
  },
};
export function validateDocument(name, value) {
  if (name === 'settings') return validateShared(value);
  if (!object(value)) throw new Error(`Invalid ${name} document`);
  const fields = SCHEMAS[name];
  if (!fields) throw new Error('Unreviewed document');
  function walk(node, path = '') {
    for (const [key, v] of Object.entries(node)) {
      const p = `${path}/${key}`;
      if (Object.hasOwn(fields, p)) {
        const type = Array.isArray(v) ? 'array' : typeof v;
        if (type !== fields[p] || (type === 'number' && (!Number.isFinite(v) || v < 0))) throw new Error(`Invalid field: ${p}`);
      } else if (object(v) && Object.keys(fields).some(k => k.startsWith(`${p}/`))) walk(v, p);
      else throw new Error(`Unreviewed field: ${p}`);
    }
  }
  walk(value);
  return value;
}
export function mergeDocuments(shared, snapshot, resolutions = {}) {
  const state = snapshot.state.value;
  if (snapshot.state.exists && (!object(state) || state.version !== 1 || !object(state.documents)))
    throw new Error('Unknown baseline format; inspect and migrate it explicitly');
  if (!object(shared) || Object.keys(shared).some(n => !Object.hasOwn(SCHEMAS, n))) throw new Error('Unknown shared document');
  if (!object(resolutions) || Object.keys(resolutions).some(n => !Object.hasOwn(shared, n))) throw new Error('Unknown resolution document');
  const documents = {}, nextState = {version: 1, documents: structuredClone(state?.documents ?? {})};
  for (const name of Object.keys(shared)) {
    documents[name] = mergeDocument(shared[name], snapshot[name].value ?? {},
      state?.documents?.[name] ?? null, resolutions[name] ?? {}, SCHEMAS[name], v => validateDocument(name, v));
    nextState.documents[name] = documents[name].state;
  }
  return {documents, state: nextState};
}
