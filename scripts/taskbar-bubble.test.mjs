import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

// Execute the real handlers with IPC/store doubles; no desktop or model request required.
function find(path, predicate) {
  const source = ts.createSourceFile(path, readFileSync(new URL(`../${path}`, import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let found;
  function visit(node) {
    if (predicate(node)) found = node;
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert.ok(found, `handler not found in ${path}`);
  return found.getText(source);
}
function evaluate(expression, context) {
  return runInNewContext(ts.transpile(`(${expression})`, { target: ts.ScriptTarget.ES2022 }), context);
}
const listener = find('src/store.ts', n => ts.isCallExpression(n) && n.expression.getText() === 'listen' && n.arguments[0]?.text === 'remote-prompt:dispatch');
async function dispatch(threadId, enabled = true, fail = false, settings = { butterflyAgentKind: 'codex', butterflyModel: 'butterfly-model' }) {
  const calls = [], errors = [];
  let handler;
  evaluate(listener, {
    listen: (_, fn) => { handler = fn; },
    state: { settings },
    lastUsed: { agentKind: () => 'lyra', model: () => 'last-model', reasoningEffort: () => 'high' },
    resolveEnabledAgentKind: k => k,
    enabledAgentKinds: () => enabled ? ['lyra', 'codex'] : ['lyra'],
    ensureModelOptions: async () => {},
    resolveAvailableModel: (_, model) => model,
    api: { scratchDir: async () => 'temp/Nova-scratch/new' },
    createThread: async (...args) => { calls.push(['create', ...args]); return 'new-thread'; },
    sendPromptTo: async (...args) => { if (fail) throw Error('offline'); calls.push(['send', ...args]); },
    showToast: message => errors.push(message),
    console: { error() {} },
  });
  handler({ payload: { threadId, text: 'hello' } });
  await new Promise(setImmediate);
  return { calls: JSON.parse(JSON.stringify(calls)), errors };
}
assert.deepEqual((await dispatch(null)).calls, [
  ['create', 'temp/Nova-scratch/new', 'codex', 'butterfly-model', 'build', ''],
  ['send', 'new-thread', 'hello', []],
]);
assert.deepEqual((await dispatch('existing')).calls, [['send', 'existing', 'hello', []]]);
const disabled = await dispatch(null, false);
assert.equal(disabled.calls.length, 0);
assert.match(disabled.errors[0], /启用 Butterfly 配置的模型后端/);
assert.deepEqual((await dispatch(null, true, false, {})).calls[0],
  ['create', 'temp/Nova-scratch/new', 'lyra', '', 'build', '']);
assert.deepEqual((await dispatch(null, true, false, { butterflyAgentKind: 'codex', butterflyModel: '' })).calls[0],
  ['create', 'temp/Nova-scratch/new', 'codex', '', 'build', '']);
assert.match((await dispatch('existing', true, true)).errors[0], /offline/);

const reply = find('src/components/TaskbarBubble.tsx', n => ts.isFunctionDeclaration(n) && n.name?.text === 'Reply');
const replySource = ts.createSourceFile('reply.tsx', reply, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let send;
function visit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(replySource) === 'send') send = node.initializer.getText(replySource);
  ts.forEachChild(node, visit);
}
visit(replySource);
async function replyCheck(threadId, fail) {
  let text = ' hello ', busy = false, error = '', hidden = false;
  const calls = [];
  const handler = evaluate(send, {
    text: () => text, busy: () => busy,
    setText: value => { text = value; }, setBusy: value => { busy = value; }, setError: value => { error = value; },
    props: { card: { threadId }, onSent: () => { hidden = true; } },
    invoke: async (command, args) => { calls.push([command, args.threadId, args.text]); if (fail) throw Error('failed'); },
  });
  await handler();
  assert.equal(busy, false);
  assert.equal(hidden, !fail);
  assert.equal(text, fail ? ' hello ' : '');
  assert.equal(!!error, !!fail);
  assert.deepEqual(calls, [['taskbar_bubble_reply', threadId, 'hello']]);
}
await replyCheck(null, false);
await replyCheck('existing', false);
await replyCheck(null, true);
// A completed reply may have exactly the same height as its streaming preview.
// Every successful refresh must acknowledge the rendered card, not only a resize or close.
const load = find('src/components/TaskbarBubble.tsx', n => ts.isVariableDeclaration(n) && n.name.getText() === 'load');
const loadExpression = load.slice(load.indexOf('=') + 1);
const rendered = [];
let hasCard = true;
const reloadCard = evaluate(loadExpression, {
  invoke: async () => ({ cards: hasCard ? [{ key: 'live:thread' }] : [], permissions: [], light: false }),
  document: { documentElement: { classList: { toggle() {} } } },
  setCards: () => rendered.push('cards'), reconcile: x => x,
  permissions: () => [], setPermissions() {},
  requestAnimationFrame: callback => { rendered.push('render'); callback(); },
  place: async () => { rendered.push('acknowledge'); return true; },
  hide: () => { rendered.push('hide'); },
});
assert.equal(await reloadCard(), true);
assert.equal(await reloadCard(), true);
assert.deepEqual(rendered, ['cards', 'render', 'acknowledge', 'cards', 'render', 'acknowledge']);
rendered.length = 0;
hasCard = false;
await reloadCard();
assert.deepEqual(rendered, ['hide'], 'a hidden/prewarmed bubble must not acknowledge a result');

// Native placement must reject post-hide ResizeObserver notifications before touching HWND
// geometry. No AppHandle/HWND is available in this headless check, so pin the guard's ordering.
const native = readFileSync(new URL('../src-tauri/src/taskbar_status/bubble.rs', import.meta.url), 'utf8');
const placement = native.slice(native.indexOf('pub(super) fn place('), native.indexOf('/// Bubble "open session"'));
const hiddenGuard = placement.indexOf('if !WANTED.load(Ordering::Acquire) { return false; }');
assert.ok(hiddenGuard >= 0 && hiddenGuard < placement.indexOf('let Some(anchor)'),
  'closed bubbles must return before any window lookup, resize or move (not is_open() click grace)');
const construction = native.slice(native.indexOf('pub(super) fn window('), native.indexOf('pub(super) fn open('));
assert.match(construction, /let disabled = 1i32;[\s\S]*DwmSetWindowAttribute\(hwnd\.0 as _, DWMWA_TRANSITIONS_FORCEDISABLED/,
  'the transparent flyout must not retain a native hide-animation surface');
assert.match(placement, /WANTED\.load\(Ordering::Acquire\) && window\.is_visible\(\)\.unwrap_or\(false\)/,
  'only a genuinely visible bubble can mark its rendered revision seen');
assert.match(placement, /Some\(\(thread, stamp\)\)[\s\S]*seen\.at\.entry\(thread\)[\s\S]*max\(stamp\)/,
  'acknowledgement must use the rendered timestamp, without rolling back newer acknowledgements');
const closing = native.slice(native.indexOf('pub(super) fn hide('), native.indexOf('/// Close the card, optionally'));
assert.doesNotMatch(closing, /mark_seen/, 'closing must not mark a newer, unrendered result read');
console.log('Butterfly dispatch, reply collapse, visible-result acknowledgement and placement checks passed.');
