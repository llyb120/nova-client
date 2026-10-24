import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import { build } from 'esbuild';

// Run with: node src/backendRemoval.test.mjs
// Session prewarm is retired; image/window prewarming is unrelated.
for (const file of ['./components/HomeView.tsx', './ipc.ts']) {
  assert.doesNotMatch(readFileSync(new URL(file, import.meta.url), 'utf8'), /prewarm/i, `${file}: remove the session prewarm chain`);
}
for (const file of ts.sys.readDirectory(fileURLToPath(new URL('.', import.meta.url)), ['.ts', '.tsx'], ['**/node_modules/**', '**/dist/**'])) {
  assert.doesNotMatch(readFileSync(file, 'utf8'), /\bapi\s*(?:\.\s*prewarm\b|\[\s*['"]prewarm['"]\s*\])/, `${file}: no session prewarm calls`);
}
const { outputFiles } = await build({
  stdin: {
    contents: 'export * from "./types"; export * from "./lyraConfig";',
    resolveDir: fileURLToPath(new URL('.', import.meta.url)), loader: 'ts',
  },
  bundle: true, platform: 'node', format: 'esm', write: false,
});
const { isSupportedAgentKind, isLocalPreset, draftFromConfig, configFromDraft, validate } =
  await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].contents).toString('base64')}`);

// Exercise the actual store entry points without booting a WebView or invoking Rust.
const source = ts.createSourceFile('store.ts', readFileSync(new URL('./store.ts', import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true);
const names = ['assertThreadBackend', 'tryBuiltinPrompt', 'sendPrompt', 'sendPromptTo', 'deliverPrompt', 'editUserMessage', 'createThread', 'createThreadOptimistic', 'createRoamingThread', 'startFireRelay', 'createFireThread', 'pickThreadModel', 'setThreadModel', 'setThreadMode', 'showThreadSnapshot'];
const units = source.statements.filter(node => ts.isFunctionDeclaration(node) && names.includes(node.name?.text));
assert.equal(units.length, names.length);
const code = ts.transpileModule(units.map(node => node.getText(source)).join('\n').replace(/export /g, ''), {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText;
const state = { currentId: 'history', agentKind: 'unsupported', threads: [], running: {} };
let fallbackKind;
const api = new Proxy({}, { get: (target, method) => method in target ? target[method] : method === 'getThread'
  ? async () => ({ agentKind: fallbackKind })
  : () => assert.fail(`Unexpected IPC: ${String(method)}`) });
const store = new Function('state', 'api', 'isSupportedAgentKind', 'isSubagentThread', 'batch', 'setState', 'recoverProposedPlan', 'message', 'openThread',
  `${code}; return { ${names.join(', ')} };`)(state, api, isSupportedAgentKind, () => false, fn => fn(), patch => Object.assign(state, patch), () => null, async () => {}, async () => {});
// Rust owns the binding check: only retired (Legacy) sources may be rebound to Lyra.
const legacyKinds = ['claude', 'codex', 'codexplus', 'devin', 'codebuddy', 'codebuddyplus', 'workbuddy'];
const bound = [];
api.bindThreadToLyra = async (id, model) => {
  if (!legacyKinds.includes(state.agentKind)) throw new Error(`不支持的后端：${state.agentKind}`);
  bound.push([id, model]);
};

for (const kind of ['claude', 'codex', 'codexplus', 'devin', 'codebuddy', 'codebuddyplus', 'workbuddy', 'alkaid', 'unknown', '', null, undefined]) {
  const items = [{ type: 'assistant', id: 1, text: 'saved history' }];
  store.showThreadSnapshot({ id: 'history', agentKind: kind, items, model: 'saved-model', reasoningEffort: 'high' }, false);
  assert.equal(state.agentKind, kind);
  assert.equal(state.items, items);
  assert.equal(state.model, 'saved-model');
  assert.equal(state.reasoningEffort, 'high');
  state.agentKind = kind;
  for (const text of ['hello', '/stage review', '/fire task', '/setup model']) {
    await assert.rejects(store.sendPrompt(text), /不支持的后端/);
  }
  await assert.rejects(store.deliverPrompt('history', 'hello', []), /不支持的后端/);
  await assert.rejects(store.editUserMessage(1, 'hello'), /不支持的后端/);
  state.threads = [{ id: 'background', agentKind: kind }];
  fallbackKind = kind;
  await assert.rejects(store.sendPromptTo('background', 'hello', []), /不支持的后端/);
  await assert.rejects(store.sendPromptTo('uncached', 'hello', []), /不支持的后端/);
  await assert.rejects(store.createThread('.', kind, '', '', ''), /不支持的后端/);
  await assert.rejects(store.createRoamingThread({}, '.', kind, '', '', 'hello'), /不支持的后端/);
  await assert.rejects(store.startFireRelay('task', null, 'background'), /不支持的后端/);
  await assert.rejects(store.createFireThread({ rootId: 'background' }, 'stage', 'task'), /不支持的后端/);
  assert.throws(() => store.createThreadOptimistic('.', kind, '', '', '', 'hello', [], false, ''), /不支持的后端/);
  await assert.rejects(store.pickThreadModel(kind, ''), /不支持的后端/);
  // Legacy sessions are rebound only via the explicit backend command; unknown kinds stay rejected.
  bound.length = 0;
  if (legacyKinds.includes(kind)) {
    await assert.rejects(store.pickThreadModel('lyra', ''), /Lyra 模型/);
    await store.pickThreadModel('lyra', 'p/replacement-model');
    assert.deepEqual(bound, [['history', 'p/replacement-model']]);
  } else {
    await assert.rejects(store.pickThreadModel('lyra', 'p/replacement-model'), /不支持的后端/);
    assert.deepEqual(bound, []);
  }
  assert.equal(state.agentKind, kind);
  assert.equal(state.model, 'saved-model');
  await assert.rejects(store.setThreadMode('build'), /不支持的后端/);
}
state.agentKind = 'lyra';
await store.assertThreadBackend('history');
const usage = { type: 'turn', id: 1, ts: 1, durationMs: 10, inputTokens: 120, contextTokens: 80, outputTokens: 40, cacheReadTokens: 30, cacheWriteTokens: 10, totalTokens: 160, actualModel: 'saved-model · high', stopReason: 'end_turn' };
store.showThreadSnapshot({ id: 'migrated', agentKind: 'lyra', items: [usage], model: 'local-codex/saved-model/variant/high', reasoningEffort: 'high' }, false);
assert.equal(state.agentKind, 'lyra');
assert.equal(state.model, 'local-codex/saved-model/variant/high');
assert.equal(state.reasoningEffort, 'high');
assert.deepEqual(state.items, [usage]);

// Browser preferences are not Rust thread files: do not erase an unmapped raw kind
// or pair its saved model/effort with Lyra without an explicit migration result.
const lastUsedNode = source.statements.flatMap(node => ts.isVariableStatement(node) ? [...node.declarationList.declarations] : [])
  .find(node => node.name.getText(source) === 'lastUsed');
assert.ok(lastUsedNode);
const lastUsedCode = ts.transpileModule(`const ${lastUsedNode.getText(source)};`, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText;
const saved = new Map();
const lastUsed = new Function('localStorage', 'modelChoices', `${lastUsedCode}; return lastUsed;`)({
  getItem: key => saved.get(key) ?? null,
  setItem: (key, value) => saved.set(key, value),
}, () => []);
assert.equal(lastUsed.agentKind(), 'lyra');
saved.set('fd:lastUsedModel', 'orphan-model');
assert.equal(lastUsed.agentKind(), '', 'a model without source kind must not default to Lyra');
saved.clear();
saved.set('fd:lastAgentKind', 'alkaid');
assert.equal(lastUsed.agentKind(), 'lyra', 'alkaid is the old Lyra name');
for (const kind of ['codex', 'claude', 'future-backend', '']) {
  saved.clear();
  saved.set('fd:lastAgentKind', kind);
  saved.set(`fd:${kind}:lastModel`, 'saved-model');
  saved.set(`fd:${kind}:lastReasoningEffort`, 'high');
  assert.equal(lastUsed.agentKind(), kind);
  assert.equal(lastUsed.model(kind), 'saved-model');
  assert.equal(lastUsed.reasoningEffort(kind), 'high');
  assert.equal(lastUsed.model('lyra'), '');
}

for (const id of ['local-codex', 'local-claude-code', 'devin', 'codebuddy', 'workbuddy']) {
  assert.equal(isLocalPreset(id), true);
  const raw = { model: `${id}/test`, provider: { [id]: { preset: id, options: { proxy: 'http://localhost:8000' }, models: { test: { cost: { input: 1, output: 2, cache_read: 0.1 } } } } } };
  const draft = draftFromConfig(raw);
  assert.equal(validate(draft.providers, [{ id, name: id, baseURL: '', local: true }]), null);
  const saved = configFromDraft(raw, draft);
  assert.equal(saved.provider[id].preset, id);
  assert.equal(saved.provider[id].options.proxy, undefined, 'Lyra follows the system proxy');
  assert.equal(saved.model, raw.model);
  assert.deepEqual(saved.provider[id].models.test.cost, raw.provider[id].models.test.cost);
}

const storeText = readFileSync(new URL('./store.ts', import.meta.url), 'utf8');
assert.doesNotMatch(storeText, /acp:(?:status|commands)/, 'no backend emits these events any more');
const settings = readFileSync(new URL('./components/SettingsModal.tsx', import.meta.url), 'utf8');
const lyraPane = settings.slice(settings.indexOf('<div class="settings-tab-pane lyra-settings"'), settings.indexOf('{/* ===== 模型后端 ===== */}'));
assert.match(lyraPane, /仅用于 Lyra 的本地 provider/);
assert.doesNotMatch(settings, /Proxy\b|ProxyField/, 'Lyra follows the system proxy');
for (const field of ['devinPath', 'workbuddyPath']) {
  const setter = `set${field[0].toUpperCase()}${field.slice(1)}`;
  assert.ok(lyraPane.includes(`value={${field}()}`), `${field} remains visible`);
  assert.ok(lyraPane.includes(setter), `${field} remains editable`);
  assert.ok(settings.includes(`${field}: ${field}().trim()`), `${field} remains saved`);
}
assert.match(settings, /connected: settings\.lyraEnabled !== false/, 'saving the Lyra switch updates connected');
assert.doesNotMatch(settings, /\b(?:devinEnabled|codebuddyEnabled|codebuddyplusEnabled|workbuddyEnabled|codebuddyPath|codebuddyIntegration|acpArgs|codebuddyArgs)\b/);
console.log('Unmapped backend rejection, raw migration fields and Lyra local provider configuration passed.');
