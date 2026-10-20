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
async function dispatch(threadId, scene = { agentKind: 'codex', model: 'light-model' }, fail = false) {
  const calls = [], errors = [];
  let handler;
  evaluate(listener, {
    listen: (_, fn) => { handler = fn; },
    state: { settings: {} },
    lastUsed: { agentKind: () => 'lyra', model: () => 'last-model', reasoningEffort: () => 'high' },
    // Butterfly 使用轻量化场景模型；没有可用模型时 sceneModel 抛错。
    sceneModel: key => { assert.equal(key, 'lightweight'); if (!scene) throw Error('「轻量化」场景没有可用模型'); return scene; },
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
  ['create', 'temp/Nova-scratch/new', 'codex', 'light-model', 'build', ''],
  ['send', 'new-thread', 'hello', []],
]);
assert.deepEqual((await dispatch('existing')).calls, [['send', 'existing', 'hello', []]]);
const missing = await dispatch(null, null);
assert.equal(missing.calls.length, 0);
assert.match(missing.errors[0], /场景没有可用模型/);
assert.match((await dispatch('existing', undefined, true)).errors[0], /offline/);

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
let hasCard = true, panel = true, tab, active, focusRequest = 0;
const loadContext = {
  revision: 0, fresh: true, focusPending: false,
  setPanel() {}, setTab: value => { tab = value; },
  setActive: value => { active = value; },
  setFocusRequest: update => { focusRequest = update(focusRequest); rendered.push('focus'); },
  invoke: async () => ({ cards: hasCard ? [{ key: 'live:thread' }] : [], permissions: [], light: false, panel }),
  document: { documentElement: { classList: { toggle() {} } } },
  setCards: () => rendered.push('cards'), reconcile: x => x,
  permissions: () => [], setPermissions() {},
  requestAnimationFrame: callback => { rendered.push('render'); callback(); },
  place: async () => { rendered.push('acknowledge'); return true; },
  hide: () => { rendered.push('hide'); },
};
const reloadCard = evaluate(loadExpression, loadContext);
assert.equal(await reloadCard(), true);
assert.equal(await reloadCard(), true);
assert.deepEqual(rendered, ['cards', 'render', 'acknowledge', 'cards', 'render', 'acknowledge']);
assert.equal(tab, 'assistant');
assert.equal(focusRequest, 0, 'hover never requests focus');
assert.equal(active, true);
rendered.length = 0;
panel = false;
loadContext.fresh = true;
await reloadCard();
assert.deepEqual(rendered, ['cards', 'render', 'acknowledge', 'focus'], 'click focuses only after placement');
await reloadCard();
assert.equal(focusRequest, 1, 'polls never steal focus');
loadContext.fresh = true;
loadContext.place = async () => false;
await reloadCard();
assert.equal(focusRequest, 1, 'failed placement cannot focus');
loadContext.place = async () => true;
const frames = [];
loadContext.requestAnimationFrame = callback => frames.push(callback);
loadContext.fresh = true;
const firstOpen = reloadCard();
await new Promise(setImmediate);
const overlappingPoll = reloadCard();
await new Promise(setImmediate);
frames.forEach(callback => callback());
assert.deepEqual(await Promise.all([firstOpen, overlappingPoll]), [false, true]);
assert.equal(focusRequest, 2, 'a newer poll cannot swallow a pending click focus');
loadContext.requestAnimationFrame = callback => callback();
rendered.length = 0;
hasCard = false;
await reloadCard();
assert.deepEqual(rendered, ['hide'], 'a hidden/prewarmed bubble must not acknowledge a result');
assert.equal(active, false);
// A late IPC result after hiding must not revive/focus the assistant.
let resolvePayload;
loadContext.invoke = () => new Promise(resolve => { resolvePayload = resolve; });
const stale = reloadCard();
++loadContext.revision;
resolvePayload({ cards: [{ key: 'late' }], permissions: [], light: false, panel: false });
rendered.length = 0;
assert.equal(await stale, false);
assert.deepEqual(rendered, []);
// Right click opens on the quick actions, left click on the assistant; both focus the input below.
for (const [native, expected, focus] of [['quick', 'quick', 1], [undefined, 'assistant', 1]]) {
  const before = focusRequest;
  loadContext.invoke = async () => ({ cards: [{ key: 'live:' }], permissions: [], light: false, panel: false, tab: native });
  loadContext.fresh = true;
  assert.equal(await reloadCard(), true);
  assert.equal(tab, expected);
  assert.equal(focusRequest - before, focus);
}

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
assert.match(placement, /WANTED\.load\(Ordering::Acquire\) && unsafe \{ IsWindowVisible\(hwnd\) \} != 0/,
  'only a genuinely visible bubble can mark its rendered revision seen');
assert.match(placement, /Some\(\(thread, stamp\)\)[\s\S]*seen\.at\.entry\(thread\)[\s\S]*max\(stamp\)/,
  'acknowledgement must use the rendered timestamp, without rolling back newer acknowledgements');
const closing = native.slice(native.indexOf('pub(super) fn hide('), native.indexOf('/// Close the card, optionally'));
assert.doesNotMatch(closing, /mark_seen/, 'closing must not mark a newer, unrendered result read');
console.log('Butterfly dispatch, reply collapse, visible-result acknowledgement and placement checks passed.');
