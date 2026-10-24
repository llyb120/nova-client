import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const { outputFiles } = await build({
  entryPoints: [fileURLToPath(new URL('./modelSelectionMigration.ts', import.meta.url))],
  bundle: true, platform: 'node', format: 'esm', write: false,
});
const { migrateStoredModelSelections: migrate } = await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].contents).toString('base64')}`);
const memory = initial => {
  const values = new Map(Object.entries(initial));
  return {
    get length() { return values.size; },
    key: index => [...values.keys()][index] ?? null,
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: key => values.delete(key),
    snapshot: () => Object.fromEntries(values),
  };
};
const initial = {
  'fd:lastAgentKind': 'codex',
  'fd:lastUsedModel': 'gpt-5',
  'fd:lastUsedModelName': 'Old Codex label',
  'fd:codex:lastModel': 'gpt-4',
  'fd:codex:lastReasoningEffort': 'high',
  'fd:claude:lastModel': 'claude-sonnet',
  'fd:claude:lastReasoningEffort': 'max',
  'fd:lyra:lastReasoningEffort': 'low',
  'fd:modelFavorites': JSON.stringify(['codex:gpt-5', 'claude:claude%3Amodel%2Fwith%25percent', 'future:model', 'lyra:existing%2Fmodel']),
  'fd:theme': 'ink-light',
};
const storage = memory(initial);
let calls = 0;
const warnings = await migrate(storage, async targets => {
  calls++;
  assert.deepEqual(storage.snapshot(), initial, 'no writes before provider save completes');
  assert.deepEqual(targets, [
    { agentKind: 'codex', model: 'gpt-5', reasoningEffort: 'high' },
    { agentKind: 'codex', model: 'gpt-4', reasoningEffort: 'high' },
    { agentKind: 'claude', model: 'claude-sonnet', reasoningEffort: 'max' },
    { agentKind: 'codex', model: 'gpt-5', reasoningEffort: null },
    { agentKind: 'claude', model: 'claude:model/with%percent', reasoningEffort: null },
    { agentKind: 'future', model: 'model', reasoningEffort: null },
  ]);
  return targets.map(t => t.agentKind === 'future' ? { error: 'unknown backend' } : {
    model: `${t.agentKind === 'codex' ? 'local-codex' : 'local-claude-code'}/${t.model}${t.reasoningEffort ? `/variant/${t.reasoningEffort}` : ''}`,
  });
});
assert.equal(calls, 1);
assert.deepEqual(warnings, ['future / model：unknown backend']);
assert.equal(storage.getItem('fd:lastAgentKind'), 'lyra');
assert.equal(storage.getItem('fd:lastUsedModel'), 'local-codex/gpt-5/variant/high');
assert.equal(storage.getItem('fd:lastUsedModelName'), null);
assert.equal(storage.getItem('fd:lyra:lastReasoningEffort'), null);
assert.equal(storage.getItem('fd:codex:lastModel'), null, 'retire only successfully mapped per-kind caches');
assert.equal(storage.getItem('fd:codex:lastReasoningEffort'), null);
assert.equal(storage.getItem('fd:theme'), 'ink-light');
assert.deepEqual(JSON.parse(storage.getItem('fd:modelFavorites')), [
  'lyra:local-codex%2Fgpt-5', 'lyra:local-claude-code%2Fclaude%3Amodel%2Fwith%25percent', 'future:model', 'lyra:existing%2Fmodel',
]);

// Per-kind fallback and identical favorites are collected only once, without guessing effort for favorites.
const fallback = memory({ 'fd:lastAgentKind': 'claude', 'fd:claude:lastModel': 'sonnet', 'fd:modelFavorites': '["claude:sonnet","claude:sonnet"]' });
await migrate(fallback, async targets => {
  assert.deepEqual(targets, [{ agentKind: 'claude', model: 'sonnet', reasoningEffort: null }]);
  return [{ model: 'local-claude-code/sonnet' }];
});
assert.equal(fallback.getItem('fd:lastUsedModel'), 'local-claude-code/sonnet');
assert.equal(fallback.getItem('fd:modelFavorites'), '["lyra:local-claude-code%2Fsonnet"]');
assert.deepEqual(await migrate(fallback, () => assert.fail('successful migration must not repeat')), []);

// An orphan model (no source kind) is never sent to migration and is reported once.
const orphan = memory({ 'fd:lastUsedModel': 'orphan-model' });
const orphanErrors = await migrate(orphan, () => assert.fail('orphan model must not call migration API'));
assert.equal(orphanErrors.length, 1);
assert.match(orphanErrors[0], /orphan-model/);
assert.equal(orphan.getItem('fd:lastUsedModel'), 'orphan-model');
assert.equal(orphan.getItem('fd:lastAgentKind'), null);
assert.deepEqual(await migrate(orphan, () => assert.fail('no repeat')), [], 'warn only once');

// alkaid is Lyra's old name: rename locally, never send to backend.
const alkaid = memory({
  'fd:lastAgentKind': 'alkaid', 'fd:lastUsedModel': 'p/model',
  'fd:alkaid:lastModel': 'p/cached', 'fd:alkaid:lastReasoningEffort': 'high',
  'fd:modelFavorites': '["alkaid:p%2Fmodel"]',
});
assert.deepEqual(await migrate(alkaid, () => assert.fail('alkaid must not call migration API')), []);
assert.deepEqual(alkaid.snapshot(), {
  'fd:lastAgentKind': 'lyra', 'fd:lastUsedModel': 'p/model',
  'fd:lyra:lastModel': 'p/cached', 'fd:lyra:lastReasoningEffort': 'high',
  'fd:modelFavorites': '["lyra:p%2Fmodel"]',
});

for (const raw of [
  { 'fd:lastAgentKind': 'future', 'fd:lastUsedModel': 'original', 'fd:future:lastReasoningEffort': 'high' },
]) {
  const failed = memory(raw);
  const errors = await migrate(failed, async targets => {
    assert.equal(targets[0].agentKind, raw['fd:lastAgentKind']);
    return targets.map(() => ({ error: 'cannot map original model' }));
  });
  assert.ok(errors[0].includes('cannot map original model'));
  assert.deepEqual(failed.snapshot(), raw);
}
for (const result of [[], [{}], [{ model: '' }], [{ model: 'not-qualified' }], [{ error: true, model: 'provider/model' }], null]) {
  const failed = memory({ 'fd:lastAgentKind': 'codex', 'fd:lastUsedModel': 'gpt' });
  const before = failed.snapshot();
  await assert.rejects(migrate(failed, async () => result), /返回结果无效/);
  assert.deepEqual(failed.snapshot(), before);
}
const failed = memory(initial);
await assert.rejects(migrate(failed, async () => { throw new Error('provider save failed'); }), /provider save failed/);
assert.deepEqual(failed.snapshot(), initial);
const malformed = memory({ ...initial, 'fd:modelFavorites': '{bad json' });
await assert.rejects(migrate(malformed, () => assert.fail('do not migrate malformed favorites')));
assert.equal(malformed.getItem('fd:modelFavorites'), '{bad json');

const concurrent = memory(initial);
await assert.rejects(migrate(concurrent, async targets => {
  concurrent.setItem('fd:lastUsedModel', 'user-new-selection');
  return targets.map(() => ({ model: 'provider/model' }));
}), /发生变化/);
assert.equal(concurrent.getItem('fd:lastUsedModel'), 'user-new-selection');
assert.equal(concurrent.getItem('fd:lastAgentKind'), 'codex');

const quota = memory(initial);
const write = quota.setItem;
let writes = 0;
quota.setItem = (key, value) => {
  if (++writes === 3) throw new Error('quota');
  write(key, value);
};
await assert.rejects(migrate(quota, async targets => targets.map(() => ({ model: 'provider/model' }))), /quota/);
assert.deepEqual(quota.snapshot(), initial, 'partial writes must roll back including absent keys');

for (const raw of [{}, { 'fd:lastAgentKind': 'lyra', 'fd:lastUsedModel': 'local-codex/gpt/variant/high', 'fd:modelFavorites': '["lyra:provider%2Fmodel"]' }]) {
  const current = memory(raw);
  assert.deepEqual(await migrate(current, () => assert.fail('already Lyra needs no migration')), []);
  assert.deepEqual(current.snapshot(), raw);
}

// Kimi/Cursor are not migration candidates: current, cache and favorites stay unknown.
for (const kind of ['kimi', 'cursor']) {
  const raw = {
    'fd:lastAgentKind': kind,
    'fd:lastUsedModel': 'original-model',
    [`fd:${kind}:lastModel`]: 'cached-model',
    [`fd:${kind}:lastReasoningEffort`]: 'high',
    'fd:modelFavorites': JSON.stringify([`${kind}:original-model`]),
  };
  const excluded = memory(raw);
  assert.deepEqual(await migrate(excluded, () => assert.fail(`${kind} must not call migration API`)), []);
  assert.deepEqual(excluded.snapshot(), raw);

  const mixed = memory({ ...raw, 'fd:codex:lastModel': 'gpt-5' });
  assert.deepEqual(await migrate(mixed, async targets => {
    assert.deepEqual(targets, [{ agentKind: 'codex', model: 'gpt-5', reasoningEffort: null }]);
    return [{ model: 'local-codex/gpt-5' }];
  }), []);
  assert.deepEqual(mixed.snapshot(), raw, 'other successful migrations must not delete excluded caches or change their kind');
}

// A failing active model must retain its source effort even if an older cache maps.
const partial = memory({ 'fd:lastAgentKind': 'codex', 'fd:lastUsedModel': 'missing', 'fd:codex:lastModel': 'known', 'fd:codex:lastReasoningEffort': 'high' });
const partialBefore = partial.snapshot();
await migrate(partial, async targets => targets.map(t => t.model === 'known' ? { model: 'local-codex/known/variant/high' } : { error: 'missing model' }));
assert.deepEqual(partial.snapshot(), partialBefore);

const entry = readFileSync(new URL('./index.tsx', import.meta.url), 'utf8');
assert.ok(entry.indexOf('await migrateLocalModelSelections()') < entry.indexOf('render(() => <App />'));
const store = readFileSync(new URL('./store.ts', import.meta.url), 'utf8');
assert.match(store, /localModelMigration \?\?=/, 'initStore shares bootstrap batch');
assert.match(store, /status\.migrationWarnings/);
console.log('Local model selection migration: batching, exact provenance, effort, favorites, warnings, save failures and bootstrap ordering passed.');
