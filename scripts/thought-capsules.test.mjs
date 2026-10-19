import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import ts from 'typescript';

// Pure, bounded checks against the actual component functions. No browser, desktop,
// network, Solid test adapter or production IPC. Run: node scripts/thought-capsules.test.mjs
const source = ts.createSourceFile('ThoughtCapsules.tsx', readFileSync(new URL('../src/components/ThoughtCapsules.tsx', import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
function expression(name) {
  let found;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(source) === name) found = node.initializer;
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node;
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert.ok(found, `Missing function: ${name}`);
  const text = found.getText(source).replace(/^export\s+/, '');
  return ts.transpile(`(${text})`, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }).replace(/^"use strict";\s*/, '').replace(/;\s*$/, '');
}
function bind(name, context = {}) { return new Function(...Object.keys(context), `return ${expression(name)}`)(...Object.values(context)); }
const capsule = (id, overrides = {}) => ({ id, title: `Idea ${id}`, text: `original ${id}`, summary: '', files: [], status: 'pending', createdAt: 1, ...overrides });
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));

const ipc = [];
const captureCapsule = bind('captureCapsule', { invoke: async (command, args) => { ipc.push([command, args]); return [capsule('saved')]; } });
const original = '  原文\n\t保留空格与换行 🙂  ';
assert.equal((await captureCapsule(original, ['/tmp/a', '/tmp/a', '/tmp/b']))[0].id, 'saved');
assert.deepEqual(ipc.pop(), ['capsule_capture', { text: original, paths: ['/tmp/a', '/tmp/b'] }]);
await captureCapsule('', ['/tmp/file']);
assert.equal(ipc.pop()[1].text, '');
for (const [text, paths] of [[' \n ', []], ['', ['']], ['', ['bad\0path']], ['', [42]], [null, []], ['', null]]) {
  await assert.rejects(captureCapsule(text, paths));
}
assert.equal(ipc.length, 0, 'invalid input must never reach IPC');

const select = bind('select');
let selection = new Set(['a', 'b']);
assert.deepEqual([...select(selection, 'c', false)], ['c'], 'ordinary click replaces selection');
assert.deepEqual([...select(selection, 'c', true)], ['a', 'b', 'c'], 'Ctrl/Meta/checkbox adds independently');
assert.deepEqual([...select(new Set(['c']), 'c', false)], [], 'clicking the selected card again deselects it');
assert.deepEqual([...select(selection, 'a', false)], ['a'], 'plain click on one of several keeps only it');
assert.deepEqual([...select(selection, 'a', true)], ['b'], 'additive selection toggles');
assert.deepEqual([...selection], ['a', 'b'], 'the old selection is never mutated');
const hasSummary = bind('hasSummary');
assert.equal(hasSummary(capsule('a', { text: '短文', summary: '短文', status: 'ready' })), false, 'identical short summaries are labeled original');
assert.equal(hasSummary(capsule('a', { text: ' 原文 ', summary: '原文\n', status: 'ready' })), false);
assert.equal(hasSummary(capsule('a', { text: '长文', summary: '提要', status: 'ready' })), true);
const matches = bind('matches');
const all = [capsule('a', { text: original }), capsule('b', { summary: 'Blue sky', files: [{ name: '计划.md', path: '/notes/roadmap', size: 5, mimeType: 'text/plain' }] })];
assert.ok(matches(all[0], '保留空格'));
assert.ok(matches(all[1], 'blue'));
assert.ok(matches(all[1], '计划'));
assert.ok(matches(all[1], 'roadmap'));
const visible = all.filter(item => matches(item, '计划'));
assert.equal([...selection].filter(id => !visible.some(item => item.id === id)).length, 1, 'hidden selected count is explicit');
assert.equal(selection.size, 2, 'search must not clear hidden selections');
const excerpt = bind('excerpt');
assert.equal(excerpt('ab🙂cd', 3), 'ab…', 'truncation never leaves a lone high surrogate');
assert.equal(all[0].text, original, 'summary/search/preview never overwrite original');

// A tiny signal harness: execute real handlers with controlled async IPC completion.
const signals = ['selected', 'draft', 'busy', 'loading', 'loaded', 'error', 'loadError', 'notice', 'view'];
function harness(overrides = {}) {
  const state = { capsules: [], selected: new Set(), draft: '', busy: null, loading: false, loaded: false, error: '', loadError: '', notice: '', view: null, calls: [], focused: 0, captured: 0, ...overrides };
  let implementation = async () => [];
  const context = {
    state, props: { active: true, onCaptured: () => state.captured++ },
    invoke: async (command, args) => { state.calls.push([command, args]); return implementation(command, args); },
    reconcile: value => value,
    setCapsules: value => { state.capsules.splice(0, state.capsules.length, ...value); },
    closeView: () => { state.view = null; },
    selectedIds: () => state.capsules.filter(item => state.selected.has(item.id)).map(item => item.id),
    removal: () => state.view?.kind === 'remove' ? state.view : undefined,
    captureCapsule, select,
  };
  for (const key of signals) {
    context[key] = () => state[key];
    context[`set${key[0].toUpperCase()}${key.slice(1)}`] = value => { state[key] = typeof value === 'function' ? value(state[key]) : value; };
  }
  // Shared mutable revision is crucial: independent requests can resolve out of order.
  const handlers = new Function(...Object.keys(context), `
    let revision = 0, disposed = false;
    const capsules = state.capsules;
    const refresh = ${expression('refresh')};
    const capture = ${expression('capture')};
    const openSelected = ${expression('openSelected')};
    const removeConfirmed = ${expression('removeConfirmed')};
    const choose = ${expression('choose')};
    return { refresh, capture, openSelected, removeConfirmed, choose, dispose: () => { disposed = true; ++revision; } };
  `)(...Object.values(context));
  return { state, context, handlers, ipc: fn => { implementation = fn; } };
}

const lists = harness({ capsules: [capsule('a'), capsule('b')], selected: new Set(['a', 'b']) });
const older = deferred(), newer = deferred();
let requests = 0;
lists.ipc(() => ++requests === 1 ? older.promise : newer.promise);
const first = lists.handlers.refresh(), second = lists.handlers.refresh();
newer.resolve([capsule('b', { createdAt: 2 }), capsule('c', { createdAt: 3 })]);
await second;
older.resolve([capsule('a')]);
await first;
assert.deepEqual(lists.state.capsules.map(item => item.id), ['c', 'b'], 'stale lists cannot resurrect deleted records');
assert.deepEqual([...lists.state.selected], ['b'], 'only actual deletion prunes selection');
assert.equal(lists.state.loading, false);
lists.ipc(async () => { throw Error('offline'); });
await lists.handlers.refresh();
assert.equal(lists.state.capsules.length, 2, 'failed refresh retains cached records');
assert.match(lists.state.loadError, /offline/);

const opened = harness({ capsules: all, selected: new Set(['a', 'b']) });
const opening = deferred();
opened.ipc(() => opening.promise);
const openingOnce = opened.handlers.openSelected();
await opened.handlers.openSelected();
assert.deepEqual(opened.state.calls, [['capsule_open', { ids: ['a', 'b'] }]], 'hidden selections included; busy blocks duplicate opening');
opening.resolve(); await openingOnce;
assert.match(opened.state.notice, /尚未发送/);
opened.state.selected.clear(); await opened.handlers.openSelected();
assert.equal(opened.state.calls.length, 1, 'empty selection cannot open');
opened.state.selected.add('a'); opened.ipc(async () => { throw Error('open failed'); });
await opened.handlers.openSelected();
assert.match(opened.state.error, /open failed/);
assert.ok(opened.state.selected.has('a'));

const removed = harness({ capsules: [...all], selected: new Set(['a', 'b']) });
await removed.handlers.removeConfirmed();
assert.equal(removed.state.calls.length, 0, 'deletion requires a confirmation snapshot');
removed.state.view = { kind: 'remove', ids: ['a'], hidden: 0 };
removed.ipc(async command => { if (command === 'capsule_remove') throw Error('delete failed'); return all; });
await removed.handlers.removeConfirmed();
assert.equal(removed.state.capsules.length, 2);
assert.equal(removed.state.selected.size, 2);
removed.ipc(async command => { if (command === 'capsule_list') throw Error('reload failed'); });
await removed.handlers.removeConfirmed(); await tick();
assert.deepEqual(removed.state.capsules.map(item => item.id), ['b'], 'successful deletion stays deleted if refresh fails');
assert.deepEqual([...removed.state.selected], ['b']);
assert.equal(removed.state.view, null);

const saving = harness({ draft: original });
await saving.handlers.capture(original);
assert.equal(saving.state.captured, 1);
assert.equal(saving.state.draft, '', 'successful capture clears the submitted draft');
saving.context.props.active = false;
await saving.handlers.capture('ignore');
assert.equal(saving.state.captured, 1, 'inactive pane cannot capture');
const pendingCapture = deferred();
const savingLater = harness({ draft: original });
let captureCalls = 0;
const saveLater = bind('capture', { ...savingLater.context, disposed: false, captureCapsule: () => { ++captureCalls; return pendingCapture.promise; }, refresh: async () => {} });
const savingOnce = saveLater(original);
await saveLater(original);
assert.equal(captureCalls, 1, 'busy capture cannot submit twice');
savingLater.state.draft = 'a newer draft';
pendingCapture.resolve([]); await savingOnce;
assert.equal(savingLater.state.draft, 'a newer draft', 'completion never clears a newer draft');
assert.equal(savingLater.state.captured, 1);

// A failed capture preserves typed text without notifying success.
const failed = harness({ draft: original });
const failCapture = bind('capture', { ...failed.context, disposed: false, captureCapsule: async () => { throw Error('offline'); }, refresh: async () => {} });
await failCapture(original);
assert.equal(failed.state.draft, original);
assert.equal(failed.state.captured, 0);
assert.equal(failed.state.busy, null);

// The shell owns the full drag lifecycle. Intercepting even one stage can strand its overlay.
const dragAttributes = [];
(function visit(node) {
  if (ts.isJsxAttribute(node) && /^(onDrop|onDrag\w*)$/i.test(node.name.getText(source))) dragAttributes.push(node.name.getText(source));
  ts.forEachChild(node, visit);
})(source);
assert.deepEqual(dragAttributes, [], 'all DOM/native drops must reach the root shell');
assert.match(source.text, /完整内容保存在附件中/);
assert.match(source.text, /Ctrl \/ ⌘ 点击多选/);
assert.match(source.text, /轻量场景模型/);
assert.match(source.text, /<h2>星屑 /, 'the panel uses the new display name');
assert.match(source.text, /aria-label="星屑"/);
assert.doesNotMatch(source.text, /\u80f6\u56ca/, 'no old display name remains in labels, messages or fallbacks');

// Cleanup and rejected/out-of-order responses cannot change unmounted state.
const unmounted = harness();
const pending = deferred(); unmounted.ipc(() => pending.promise);
const pendingList = unmounted.handlers.refresh(); unmounted.handlers.dispose();
pending.resolve([capsule('late')]); await pendingList;
assert.equal(unmounted.state.capsules.length, 0);
assert.ok(!opened.state.calls.some(([command]) => /send/.test(command)), 'the component never sends a message');
console.log('星屑: display name, capture validation, original preservation, selection/filtering, async ordering, deletion confirmation, root-owned drops and draft-only opening passed.');
