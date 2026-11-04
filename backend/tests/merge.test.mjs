import test from 'node:test';
import assert from 'node:assert/strict';
import {mergeShared, validateShared} from '../merge.mjs';
import {mergeDocuments, validateDocument} from '../documents.mjs';
const base = source => ({version: 1, source});
const snapshot = (values = {}, previous = null) => ({...Object.fromEntries(['settings','mcp','subagents'].map(n => [n, {value: values[n] ?? {}}])), state: {exists: !!previous, value: previous}});
const docs = (values = {}) => ({settings: {}, mcp: {}, subagents: {}, ...values});

test('independent fields merge while target-only edits and unknown/local settings survive', () => {
  const local = {deviceId:'device', trackingId:'track', shellPath:'/bin/zsh', shellCommandPrefix:'env LOCAL=1', defaultProvider:'private', defaultModel:'custom', future:{nested:true}, retry:{enabled:true,maxRetries:12,unknown:7}};
  const target = {...local, theme:'dark'};
  const before = structuredClone(target);
  const m = mergeShared({theme:'light',retry:{enabled:false,maxRetries:3}}, target, base({theme:'dark',retry:{enabled:true,maxRetries:3}}));
  assert.deepEqual(m.conflicts, []);
  assert.equal(m.result.theme, 'light'); assert.equal(m.result.retry.enabled, false); assert.equal(m.result.retry.maxRetries,12); assert.equal(m.result.retry.unknown,7);
  for (const k of Object.keys(local).filter(k=> k !== 'retry')) assert.deepEqual(m.result[k],local[k]);
  assert.deepEqual(m.kept,['/retry/maxRetries']); assert.deepEqual(target,before);
});
test('true conflict requires explicit source or target selection', () => {
  const args = [{theme:'light'}, {theme:'custom'}, base({theme:'dark'})];
  const m = mergeShared(...args); assert.equal(m.conflicts[0].reason,'both-changed'); assert.equal(m.result.theme,'custom');
  assert.equal(mergeShared(...args,{'/theme':'source'}).result.theme,'light');
  assert.equal(mergeShared(...args,{'/theme':'target'}).result.theme,'custom');
});
test('first run and newly owned missing fields require review, identical fields do not', () => {
  for (const state of [null,base({})]) {
    const m = mergeShared({theme:'light',retry:{enabled:true}}, {theme:'light'},state);
    assert.equal(m.conflicts.length,1); assert.equal(m.conflicts[0].path,'/retry/enabled'); assert.equal(m.conflicts[0].reason,'first-run-review');
    assert.deepEqual(m.conflicts[0].target,{present:false}); assert.deepEqual(m.result,{theme:'light'});
    assert.equal(mergeShared({theme:'light'}, {},state,{'/theme':'source'}).result.theme,'light');
  }
});
test('absent shared declarations relinquish ownership without deleting target data', () => {
  const target={theme:'custom',retry:{enabled:false},packages:['npm:local']};
  const m=mergeShared({},target,base({theme:'light',retry:{enabled:true},packages:['npm:shared']}));
  assert.deepEqual(m.result,target); assert.deepEqual(m.changes,[]); assert.deepEqual(m.conflicts,[]); assert.deepEqual(m.state,base({}));
});
test('ordered packages and filters are atomic and never unioned', () => {
  const old=['npm:a','npm:b']; const source=['npm:b','npm:a']; const target=['npm:a','npm:c'];
  const m=mergeShared({packages:source},{packages:target},base({packages:old}));
  assert.deepEqual(m.conflicts.map(c=>c.path),['/packages']);
  assert.deepEqual(mergeShared({packages:source},{packages:target},base({packages:old}),{'/packages':'source'}).result.packages,source);
  assert.deepEqual(mergeShared({packages:source},{packages:old},base({packages:old})).result.packages,source);
});
test('explicit builtin:mcp enable replaces only its directives', () => {
  const target={extensions:['./local.ts','!builtin:mcp','+other','-builtin:mcp','builtin:mcp']};
  const m=mergeShared({extensions:['+builtin:mcp']},target,null,{'/extensions/builtin:mcp':'source'});
  assert.deepEqual(m.result.extensions,['./local.ts','+other','+builtin:mcp']);
  assert.throws(()=>validateShared({extensions:['builtin:mcp']}),/reviewed/);
});
test('MCP leaf merges preserve other servers, Argent env, and unknown subagent fields', () => {
  const previous=docs({mcp:{mcpServers:{argent:{command:'argent',timeout:60}}},subagents:{maxConcurrent:2}});
  const state={version:1,documents:Object.fromEntries(Object.entries(previous).map(([n,v])=>[n,base(v)]))};
  const current=docs({mcp:{mcpServers:{argent:{command:'local-argent',timeout:60,env:{LOCAL_SECRET:'preserve'},future:true},other:{url:'https://local.invalid'}}},subagents:{maxConcurrent:2,customAgent:{model:'local'}}});
  const incoming=docs({mcp:{mcpServers:{argent:{command:'argent',timeout:600}}},subagents:{maxConcurrent:4}});
  const m=mergeDocuments(incoming,snapshot(current,state));
  assert.deepEqual(m.documents.mcp.result.mcpServers,{argent:{command:'local-argent',timeout:600,env:{LOCAL_SECRET:'preserve'},future:true},other:{url:'https://local.invalid'}});
  assert.deepEqual(m.documents.subagents.result,{maxConcurrent:4,customAgent:{model:'local'}});
  assert.deepEqual(m.documents.mcp.conflicts,[]);
  assert.throws(()=>validateDocument('mcp',{mcpServers:{argent:{env:{TOKEN:'bad'}}}}),/Unreviewed/);
});
test('stale, unknown, or invalid resolutions and malformed baselines are rejected', () => {
  for(const resolutions of [{'/theme':'source'},{'/unknown':'target'}]) assert.throws(()=>mergeShared({theme:'light'},{theme:'light'},null,resolutions),/stale or unknown/);
  assert.throws(()=>mergeShared({theme:'light'},{},null,{'/theme':'maybe'}),/Invalid resolution/);
  assert.throws(()=>mergeShared({theme:'light'},{},{version:2,source:{}}),/baseline/);
  assert.throws(()=>mergeDocuments(docs(),snapshot(),{unknown:{}}),/Unknown resolution/);
  assert.throws(()=>mergeShared({retry:{enabled:true}},{retry:'local'},null,{'/retry/enabled':'source'}),/parent is not an object/);
});
test('local settings and unreviewed nested fields cannot be shared', () => {
  for(const key of ['deviceId','trackingId','shellPath','shellCommandPrefix','defaultProvider','defaultModel','unknown']) assert.throws(()=>validateShared({[key]:'value'}),/Unreviewed/);
  assert.throws(()=>validateShared({retry:{token:'secret'}}),/Unreviewed/);
});

test('excluded document baselines survive untouched without requiring excluded snapshots',()=>{
  const mcp=base({mcpServers:{argent:{timeout:600}}});
  const previous={version:1,documents:{settings:base({theme:'dark'}),mcp,subagents:base({maxConcurrent:2})}};
  const before=structuredClone(previous);
  const merged=mergeDocuments({settings:{theme:'light'}},{settings:{value:{theme:'dark'}},state:{exists:true,value:previous}});
  assert.deepEqual(Object.keys(merged.documents),['settings']);
  assert.deepEqual(merged.state.documents.mcp,mcp);assert.deepEqual(merged.state.documents.subagents,previous.documents.subagents);
  assert.deepEqual(previous,before);assert.notEqual(merged.state.documents.mcp,mcp);
  assert.throws(()=>mergeDocuments({settings:{}},snapshot({},previous),{mcp:{}}),/Unknown resolution document/);
});
