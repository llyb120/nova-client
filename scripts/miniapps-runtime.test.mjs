// Run: node --test scripts/miniapps-runtime.test.mjs (Node >= 22.18).
// Execute the real component logic in the same realm as its model validators.
// Only signals/lifecycle, storage and IPC are stubbed; no DOM or real tools run.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { randomUUID } from 'node:crypto';
import { setImmediate as settle } from 'node:timers/promises';
import * as model from '../src/miniapps.ts';

const source = readFileSync(new URL('../src/components/MiniAppsView.tsx', import.meta.url), 'utf8');
const start = source.indexOf('const STORAGE =');
const end = source.indexOf('  function blockView(');
assert.ok(start >= 0 && end > start, 'Component extraction boundaries changed');
const publishDisabled = source.match(/<button\b[^>]*disabled=\{([^}]+)\}[^>]*onClick=\{publish\}/)?.[1];
assert.ok(publishDisabled, 'Publish button extraction boundary changed');
const optionsOpen = source.match(/<SearchSelect\b[^\n]*\bonOpen=\{(.*?)\} onChange=/)?.[1];
assert.ok(optionsOpen, 'SearchSelect onOpen extraction boundary changed');
const threadHelper = source.match(/\n  async function showThread\(\) \{[\s\S]*?\n  \}/)?.[0];
assert.ok(threadHelper, 'showThread extraction boundary changed');
const compiled = stripTypeScriptTypes(
  source.slice(start, end).replace('export default function MiniAppsView', 'function MiniAppsView') + threadHelper + `
  return { apps, activeId, doc, values, setValues, error, logs, lastThread, locked, permissions, showThread,
    choose, switchMode, selectBlock, selectedIds, setPageId,
    browserMatch, setBrowserMatch, saveBrowserConfig, undo,
    optionLoads, optionsKey, availableOptions, requestOptions, openOptions: block => (${optionsOpen})(),
    activate, test, checks, publish, publishLabel, pinVersion, restoreVersion, unpublish, startRun, pollRun, cancelRun,
    generate, instruction, setInstruction, designImages, addDesignImages, pasteDesignImages, removeDesignImage,
    designKeyDown, previewStyle, publishDisabled: () => (${publishDisabled}) };
}
`,
) + '\nreturn MiniAppsView(props);';
const KEY = 'nova:miniapps:v1';
const clone = value => JSON.parse(JSON.stringify(value));
const saved = (patch = {}) => ({ id: randomUUID(), draft: clone(model.DEMO_APP), updatedAt: 1, messages: [], history: [], ...patch });
const run = (patch = {}) => ({ threadId: 'mock-thread', target: 'report', name: 'Mock action', at: 1, status: 'running', output: '', ...patch });
const answer = text => ({ type: 'assistant', text });
const turn = (stopReason = 'end_turn') => ({ type: 'turn', stopReason });

function mount(t, items, overrides = {}, props = {}) {
  const mounts = [], effects = [], cleanups = [], calls = [], titles = [];
  const listeners = new Map();
  const storage = new Map([[KEY, JSON.stringify(items)]]);
  const unexpected = async () => { throw new Error('Unexpected IPC call'); };
  const dependencies = {
    ...model, props, crypto: { randomUUID }, navigator: { platform: 'Win32' },
    localStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
    sessionStorage: { getItem: () => null, removeItem() {} },
    createSignal(initial) {
      let value = initial;
      return [() => value, next => (value = typeof next === 'function' ? next(value) : next)];
    },
    flush: fn => fn?.(), createMemo: fn => fn,
    createEffect(compute, apply) {
      let previous, cleanup;
      effects.push(() => {
        if (typeof cleanup === 'function') cleanup();
        const value = compute(previous);
        cleanup = apply(value, previous);
        previous = value;
      });
      cleanups.push(() => { if (typeof cleanup === 'function') cleanup(); });
    },
    onSettled: fn => mounts.push(fn), onCleanup: fn => cleanups.push(fn),
    window: { addEventListener() {}, removeEventListener() {}, setInterval: () => 1, clearInterval() {}, setTimeout, clearTimeout },
    state: { permissions: [], settings: {} },
    sceneModel: () => ({ agentKind: 'lyra', model: 'mock-model' }),
    getCurrentWindow: () => ({ setTitle: async title => { titles.push(title); }, onCloseRequested: async () => () => {} }),
    listen: (event, handler) => {
      listeners.set(event, handler);
      return Promise.resolve(() => listeners.delete(event));
    },
    invoke: unexpected, sendPromptTo: unexpected, openThread: unexpected, refreshThreads: async () => {},
    fileToAttachment: async file => ({ name: file.name, mimeType: file.type, size: file.size, data: Buffer.from(await file.arrayBuffer()).toString('base64') }),
    ...overrides,
    api: { scratchDir: async () => 'mock-dir', createThread: async () => ({ id: 'mock-thread' }),
      sendPrompt: unexpected, respondPermission: unexpected,
      listThreads: unexpected, getThreadView: unexpected, cancelTurn: unexpected, ...overrides.api },
  };
  dependencies.state.relay ??= { enabled: false };
  // Record even caught failures, so an attempted AI fallback cannot pass silently.
  for (const [target, names] of [[dependencies, ['invoke', 'sceneModel', 'sendPromptTo', 'openThread', 'refreshThreads']], [dependencies.api, Object.keys(dependencies.api)]]) {
    for (const name of names) {
      const original = target[name];
      target[name] = (...args) => {
        if (name === 'invoke' && args[0] === 'miniapp_take_launch') return Promise.resolve(overrides.launch?.() ?? null);
        calls.push(name === 'invoke' ? args[0] : name);
        return original(...args);
      };
    }
  }
  const view = new Function(...Object.keys(dependencies), compiled)(...Object.values(dependencies));
  // Never run effects inside component construction (later bindings may be in TDZ).
  // Signal changes are explicit: tests flush after the state transition under test.
  const flushEffects = () => effects.forEach(fn => fn());
  mounts.forEach(fn => { const cleanup = fn(); if (typeof cleanup === 'function') cleanups.push(cleanup); });
  flushEffects();
  t.after(() => cleanups.reverse().forEach(fn => fn()));
  return { ...view, calls, titles, flushEffects, storage: dependencies.localStorage,
    emit: (event, payload) => listeners.get(event)?.({ payload }),
    persisted: () => JSON.parse(dependencies.localStorage.getItem(KEY)) };
}

async function startAction(view) {
  view.switchMode('try');
  view.setValues({ ...view.values(), tasks: 'Actual test input' });
  view.activate('draft');
  assert.equal(view.locked(), true, 'activation starts immediately, without a confirmation step');
  await settle();
  assert.equal(view.apps()[0].run?.threadId, 'mock-thread', view.error());
  assert.equal(view.apps()[0].run.commandId, undefined);
  assert.equal(view.lastThread(), 'mock-thread');
}

test('runtime DOM gates omit the workbench shell and confirmation UI, retaining app content and stop controls', () => {
  assert.doesNotMatch(source, /\b(?:Confirmation|confirmAction|setConfirmAction)\b/);
  assert.match(source, /class=\{\["mini-studio", \{ "mini-runtime": !!props\.runtimeAppId \}\]\}/);
  for (const name of ['mini-topbar', 'mini-library', 'mini-canvas-toolbar', 'mini-copilot', 'mini-preview-footer', 'mini-empty']) {
    assert.match(source, new RegExp(`<Show when=\\{!props\\.runtimeAppId\\}>\\s*(?:<TitleBarPortal>\\s*)?<(?:header|div|aside|footer) class="${name}"`), `${name} must be outside runtime DOM`);
  }
  assert.match(source, /<Show when=\{!props\.runtimeAppId && [^\n]*<section class="mini-debug"/);
  assert.match(source, /<Show when=\{!props\.runtimeAppId && mode\(\) === "design"\}><button class="mini-add-page"/);
  assert.match(source, /<Show when=\{doc\(\) && \(!props\.runtimeAppId \|\| doc\(\)!\.pages\.length > 1\)\}><nav class="mini-page-tabs"/);
  assert.match(source, /\{blockView\(block\)\}/);
  assert.match(source, /<Show when=\{props\.runtimeAppId && running\(\)\}><div class="mini-run-status"[^\n]*cancelRun\(\)/);
});

test('the miniapp entry branch mounts only its runtime and never App or initStore', () => {
  const entry = readFileSync(new URL('../src/index.tsx', import.meta.url), 'utf8');
  const branch = entry.match(/else if \([^\n]*\.has\("miniapp"\)[^\n]*\) \{([\s\S]*?)\n\} else \{/);
  assert.ok(branch, 'Independent miniapp entry branch must exist');
  assert.match(branch[1], /import\("\.\/components\/MiniAppsView"\)/);
  assert.match(branch[1], /\.get\("miniapp"\) \|\| "invalid"/);
  assert.match(branch[1], /<MiniAppsView runtimeAppId=\{appId\} \/>/);
  const mainOnly = /\binitStore\b|import\(["']\.\/App["']\)|<App\b/;
  assert.doesNotMatch(branch[1], mainOnly);
  assert.doesNotMatch(entry.slice(0, entry.indexOf('\nif (')), /\b(?:App|initStore)\b/);
});

test('browser activation can be configured without AI, preserving bindings, publication and undo', t => {
  const draft = clone(model.DEMO_APP);
  draft.browser = { match: ['https://old.example/*'], actionId: 'draft', urlField: 'tasks', titleField: 'tasks' };
  const item = saved({ draft, published: clone(draft) });
  const view = mount(t, [item]);
  assert.equal(view.browserMatch(), 'https://old.example/*');
  view.calls.length = 0;
  view.setBrowserMatch('  https://example.com/work/*  \r\n\n*://other.example/*\nhttps://example.com/work/*');
  view.saveBrowserConfig();
  assert.equal(view.error(), '');
  const stored = view.persisted()[0];
  assert.deepEqual(stored.draft.browser, { ...draft.browser, match: ['https://example.com/work/*', '*://other.example/*'] });
  assert.deepEqual(stored.draft.pages, draft.pages);
  assert.deepEqual(stored.draft.actions, draft.actions);
  assert.deepEqual(stored.published, draft, 'configuration must not silently change the live version');
  assert.deepEqual(stored.history, [draft]);
  assert.deepEqual(view.calls, [], 'manual configuration must not invoke AI or tools');
  assert.equal(view.publishDisabled(), false);
  view.publish();
  assert.deepEqual(model.browserTriggers(JSON.stringify(view.persisted()))[0].match, stored.draft.browser.match);
  view.setBrowserMatch(' \n ');
  view.saveBrowserConfig();
  assert.equal(Object.hasOwn(view.persisted()[0].draft, 'browser'), false);
  assert.ok(view.persisted()[0].published.browser, 'clearing the draft must not disable the live version before publishing');
  view.undo();
  assert.deepEqual(view.persisted()[0].draft.browser, stored.draft.browser);
});

test('browser configuration rejects invalid rules, respects read-only mode and reloads per app', t => {
  const items = [saved(), saved()];
  items[1].draft.browser = { match: ['https://second.example/*'] };
  const view = mount(t, items);
  const original = view.storage.getItem(KEY);
  for (const text of ['file:///private', 'https://a b', Array.from({ length: 21 }, (_, i) => `https://example.com/${i}`).join('\n'), `https://example.com/${'x'.repeat(2048)}`]) {
    view.setBrowserMatch(text);
    view.saveBrowserConfig();
    assert.ok(view.error());
    assert.equal(view.storage.getItem(KEY), original);
  }
  view.choose(items[1].id);
  view.flushEffects();
  assert.equal(view.browserMatch(), 'https://second.example/*');
  view.setBrowserMatch('https://changed.example/*');
  view.saveBrowserConfig();
  assert.equal(view.error(), '');
  view.publish();
  const published = view.storage.getItem(KEY);
  view.switchMode('run');
  view.setBrowserMatch('https://not-saved.example/*');
  view.saveBrowserConfig();
  assert.equal(view.storage.getItem(KEY), published);
});

test('design composer sends Enter, but Shift+Enter, held Enter and IME confirmation never submit', async t => {
  let sent = 0, prevented = 0;
  const view = mount(t, [saved()], { invoke: async () => { sent++; return JSON.stringify(model.DEMO_APP); } });
  const key = (patch = {}) => ({ key: 'Enter', shiftKey: false, isComposing: false, keyCode: 13, repeat: false, preventDefault: () => prevented++, ...patch });
  view.setInstruction('把选择框拉满');
  for (const event of [key({ shiftKey: true }), key({ isComposing: true }), key({ keyCode: 229 }), key({ repeat: true }), key({ key: 'a' })]) view.designKeyDown(event);
  await settle();
  assert.equal(sent, 0);
  assert.equal(view.instruction(), '把选择框拉满');
  view.designKeyDown(key());
  await settle();
  assert.equal(sent, 1);
  assert.equal(view.instruction(), '');
  assert.equal(prevented, 2, 'only plain/held Enter suppress their default newline');
});

test('design composer accepts pasted images and sends image-only creation and repair with the same attachments', async t => {
  const requests = [];
  const view = mount(t, [], { invoke: async (command, args) => {
    assert.equal(command, 'miniapp_generate');
    requests.push(args);
    return requests.length === 1 ? 'invalid app JSON' : JSON.stringify(model.DEMO_APP);
  } });
  let prevented = 0;
  view.pasteDesignImages({ clipboardData: { files: [] }, preventDefault: () => prevented++ });
  assert.equal(prevented, 0, 'ordinary text paste keeps its native behavior');
  view.pasteDesignImages({ clipboardData: { files: [new File(['reference-image-data'], 'reference.png', { type: 'image/png' })] }, preventDefault: () => prevented++ });
  await settle();
  assert.equal(prevented, 1);
  const images = view.designImages();
  assert.equal(images.length, 1);
  assert.equal(view.instruction(), '');
  await view.generate();
  assert.equal(view.error(), '');
  assert.equal(requests.length, 2);
  assert.equal(requests[0].appId, requests[1].appId);
  for (const request of requests) {
    assert.deepEqual(request.images, images);
    assert.match(request.prompt, /请参考附带图片设计或修改这个应用/);
    assert.ok(!request.prompt.includes(images[0].data), 'images are multimodal attachments, not base64 prompt text');
  }
  assert.equal(view.designImages().length, 0);
  assert.match(view.persisted()[0].messages[0].text, /附带 1 张参考图片/);
  assert.ok(!view.storage.getItem(KEY).includes(images[0].data), 'image bytes must not exhaust localStorage');
  assert.match(source, /onPaste=\{pasteDesignImages\}/);
  assert.match(source, /type="file" accept="image\/png,image\/jpeg,image\/gif,image\/webp" multiple hidden/);
  assert.match(source, /ImageAttachmentStrip images=\{designImages\(\)\} onRemove=\{removeDesignImage\}/);
});

test('design images survive transport/save failures and are cleared when choosing another app', async t => {
  let fail = true;
  const items = [saved(), saved()];
  const view = mount(t, items, { invoke: async () => {
    if (fail) throw new Error('network failed');
    return JSON.stringify(model.DEMO_APP);
  } });
  await view.addDesignImages([new File(['data'], 'reference.webp', { type: 'image/webp' })]);
  view.setInstruction('参考这张图调整布局');
  await view.generate();
  assert.match(view.error(), /network failed/);
  assert.equal(view.instruction(), '参考这张图调整布局');
  assert.equal(view.designImages().length, 1);
  assert.deepEqual(view.persisted(), items);
  fail = false;
  const write = view.storage.setItem;
  view.storage.setItem = () => { throw new Error('quota exceeded'); };
  await view.generate();
  assert.match(view.error(), /quota exceeded/);
  assert.equal(view.designImages().length, 1, 'do not clear attachments before the result is saved');
  assert.equal(view.instruction(), '参考这张图调整布局');
  view.storage.setItem = write;
  view.choose(items[1].id);
  assert.equal(view.designImages().length, 0, 'never leak images to a different app');
});

test('image validation and read locks preserve existing attachments and block incomplete sends', async t => {
  const pending = Promise.withResolvers();
  const items = [saved(), saved()];
  const image = { name: 'reference.png', mimeType: 'image/png', data: 'YQ==', size: 1 };
  const view = mount(t, items, { fileToAttachment: () => pending.promise });
  const adding = view.addDesignImages([new File(['a'], 'reference.png', { type: 'image/png' })]);
  assert.equal(view.locked(), true);
  view.setInstruction('edit');
  await view.generate();
  assert.ok(!view.calls.includes('miniapp_generate'));
  view.choose(items[1].id);
  assert.equal(view.activeId(), items[0].id);
  pending.resolve(image);
  await adding;
  assert.equal(view.locked(), false);
  assert.deepEqual(view.designImages(), [image]);
  for (const [files, message] of [
    [[{ type: 'image/svg+xml', size: 1 }], /PNG/],
    [[{ type: 'image/png', size: 0 }], /5 MiB/],
    [[{ type: 'image/jpeg', size: 5 * 1024 * 1024 + 1 }], /5 MiB/],
    [Array(4).fill({ type: 'image/png', size: 1 }), /最多添加 4/],
  ]) {
    await view.addDesignImages(files);
    assert.match(view.error(), message);
    assert.deepEqual(view.designImages(), [image]);
  }
  view.publish();
  view.switchMode('run');
  await view.addDesignImages([new File(['a'], 'another.png', { type: 'image/png' })]);
  view.removeDesignImage(0);
  assert.deepEqual(view.designImages(), [image], 'published view must not edit pending images');
  view.switchMode('design');
  view.removeDesignImage(0);
  assert.deepEqual(view.designImages(), []);
});

test('image read failure leaves text and prior attachments intact', async t => {
  const view = mount(t, [saved()], { fileToAttachment: async () => { throw new Error('unreadable image'); } });
  view.setInstruction('keep my input');
  await view.addDesignImages([new File(['a'], 'reference.png', { type: 'image/png' })]);
  assert.match(view.error(), /unreadable image/);
  assert.equal(view.instruction(), 'keep my input');
  assert.deepEqual(view.designImages(), []);
  assert.equal(view.locked(), false);
});

test('preview maps validated layout styles without changing the bound action', t => {
  const view = mount(t, [saved()]);
  const block = { id: 'button', type: 'button', label: 'Run', actionId: 'draft', style: { width: 'half', align: 'right', textAlign: 'center', fontSize: 18, padding: 0, marginTop: 8 } };
  assert.deepEqual(view.previewStyle(block), { width: '50%', 'justify-self': 'end', '--mini-text-align': 'center', 'text-align': 'center', '--mini-font-size': '18px', padding: '0px', 'margin-top': '8px' });
  assert.equal(view.previewStyle({ ...block, style: { align: 'right' } }).width, 'fit-content');
  assert.equal(view.previewStyle({ ...block, style: { width: 'full' } }).width, '100%');
  assert.equal(view.previewStyle({ ...block, style: undefined }).width, undefined);
  assert.equal(block.actionId, 'draft');
});

test('poll reads metadata first, waits for terminal evidence and retains the final output', async t => {
  const pending = Promise.withResolvers();
  const calls = [];
  let items = [answer('partial')];
  const view = mount(t, [saved({ run: run() })], { api: {
    listThreads: () => { calls.push('list'); return pending.promise; },
    getThreadView: async () => { calls.push('view'); return { items }; },
  } });
  assert.deepEqual(calls, ['list']);
  pending.resolve([{ id: 'mock-thread', running: false }]);
  await settle();
  assert.deepEqual(calls, ['list', 'view']);
  assert.equal(view.apps()[0].run.status, 'running');
  assert.equal(view.locked(), true);
  items = [answer('final'), turn()];
  await view.pollRun();
  assert.equal(view.apps()[0].run.status, 'finished');
  assert.equal(view.persisted()[0].run.output, 'final');
  assert.equal(view.values().report, 'final');
  assert.equal(view.locked(), false);
});

test('a killed run without a terminal item unlocks after confirmed idle, keeping partial output without retry', async t => {
  let now = 10000;
  const other = saved();
  const view = mount(t, [saved({ run: run() }), other], { performance: { now: () => now }, api: {
    listThreads: async () => [{ id: 'mock-thread', running: false }],
    getThreadView: async () => ({ items: [{ type: 'user', text: 'latest request' }, answer('partial result'), { type: 'tool', title: 'wait_agent', status: 'in_progress' }] }),
  } });
  await settle();
  assert.equal(view.locked(), true);
  now += 2999;
  await view.pollRun();
  assert.equal(view.locked(), true);
  now++;
  await view.pollRun();
  assert.equal(view.locked(), false);
  assert.equal(view.persisted()[0].run.status, 'error');
  assert.match(view.persisted()[0].run.output, /运行已中断/);
  assert.match(view.persisted()[0].run.output, /partial result/);
  assert.equal(view.calls.includes('sendPromptTo'), false);
  assert.equal(view.calls.some(call => call.startsWith('miniapp_command_')), false);
  view.choose(other.id);
  assert.equal(view.activeId(), other.id);
});

test('startup, resumed work and temporary IPC errors reset the idle recovery grace', async t => {
  let now = 10000, running = false, fail = false;
  const view = mount(t, [saved({ run: run({ at: Date.now() + 86_400_000 }) })], { performance: { now: () => now }, api: {
    listThreads: async () => { if (fail) throw new Error('temporary'); return [{ id: 'mock-thread', running }]; },
    getThreadView: async () => ({ items: [] }),
  } });
  await settle();
  now = 12999;
  await view.pollRun();
  assert.equal(view.locked(), true, 'dispatch grace uses monotonic idle observations, not the saved wall clock');
  running = true;
  now = 15000;
  await view.pollRun();
  assert.equal(view.locked(), true);
  running = false;
  await view.pollRun();
  now = 18000;
  fail = true;
  await view.pollRun();
  fail = false;
  now = 19000;
  await view.pollRun();
  assert.equal(view.locked(), true, 'IPC errors break consecutive idle evidence');
  now = 22000;
  await view.pollRun();
  assert.equal(view.locked(), false);
});

test('rechecking a resumed backend prevents an interrupted result from unlocking active work', async t => {
  let now = 10000, reads = 0;
  const view = mount(t, [saved({ run: run() })], { performance: { now: () => now }, api: {
    listThreads: async () => [{ id: 'mock-thread', running: ++reads >= 3 }],
    getThreadView: async () => ({ items: [answer('still partial')] }),
  } });
  await settle();
  now += 3000;
  await view.pollRun();
  assert.equal(reads, 3);
  assert.equal(view.locked(), true);
  assert.equal(view.persisted()[0].run.status, 'running');
});

test('a previous turn marker cannot make a later killed turn look successful', async t => {
  let now = 10000;
  const view = mount(t, [saved({ run: run() })], { performance: { now: () => now }, api: {
    listThreads: async () => [{ id: 'mock-thread', running: false }],
    getThreadView: async () => ({ items: [answer('old success'), turn(), { type: 'user', text: 'new request' }] }),
  } });
  await settle();
  assert.equal(view.locked(), true);
  now += 3000;
  await view.pollRun();
  assert.equal(view.persisted()[0].run.status, 'error');
  assert.doesNotMatch(view.persisted()[0].run.output, /old success/);
  assert.equal(view.locked(), false);
});

test('terminal evidence only releases the lock after metadata says stopped', async t => {
  const cases = [
    ['turn failure', {}, [turn('error')], 'error'],
    ['turn cancellation', {}, [turn('cancelled')], 'cancelled'],
    ['runtime fault', {}, [{ type: 'system', level: 'error', text: 'runtime failed' }], 'error'],
    ['send failure', { sendError: 'dispatch failed' }, [], 'error'],
    ['cancel request without a turn', { cancelRequested: true }, [], 'cancelled'],
  ];
  for (const [name, patch, items, expected] of cases) await t.test(name, async t => {
    let running = true;
    const view = mount(t, [saved({ run: run(patch) })], { api: {
      listThreads: async () => [{ id: 'mock-thread', running }], getThreadView: async () => ({ items }),
    } });
    await settle();
    assert.equal(view.locked(), true);
    running = false;
    await view.pollRun();
    assert.equal(view.persisted()[0].run.status, expected);
    assert.equal(view.locked(), false);
  });
});

test('refresh failure after dispatch does not end tracking; send failure waits for polling', async t => {
  for (const sendFails of [false, true]) await t.test(sendFails ? 'send rejects' : 'refresh rejects', async t => {
    let sent = 0, refreshed = 0, running = true;
    const view = mount(t, [saved()], {
      sendPromptTo: async () => { sent++; if (sendFails) throw new Error('dispatch failed'); },
      refreshThreads: async () => { refreshed++; throw new Error('refresh failed'); },
      api: { listThreads: async () => [{ id: 'mock-thread', running }],
        getThreadView: async () => ({ items: sendFails ? [] : [answer('done'), turn()] }) },
    });
    await startAction(view);
    await settle();
    assert.equal(sent, 1);
    assert.equal(refreshed, sendFails ? 0 : 1);
    assert.deepEqual(view.calls, ['sceneModel', 'scratchDir', 'createThread', 'sendPromptTo', ...(sendFails ? [] : ['refreshThreads'])]);
    assert.equal(view.persisted()[0].run.sendError, sendFails ? 'dispatch failed' : undefined);
    assert.equal(view.locked(), true);
    await view.pollRun();
    assert.equal(view.apps()[0].run.status, 'running');
    running = false;
    await view.pollRun();
    assert.equal(view.persisted()[0].run.status, sendFails ? 'error' : 'finished');
    assert.equal(view.locked(), false);
  });
});

test('successful cancellation records the request but waits for the backend to stop', async t => {
  let running = true, cancelled = 0;
  const view = mount(t, [saved({ run: run() })], { api: {
    listThreads: async () => [{ id: 'mock-thread', running }], getThreadView: async () => ({ items: [] }),
    cancelTurn: async () => { cancelled++; },
  } });
  await settle();
  await view.cancelRun();
  await settle();
  assert.equal(cancelled, 1);
  assert.equal(view.persisted()[0].run.cancelRequested, true);
  assert.equal(view.locked(), true);
  running = false;
  await view.pollRun();
  assert.equal(view.persisted()[0].run.status, 'cancelled');
  assert.equal(view.locked(), false);
});

test('transient status errors retain the lock; confirmed missing thread releases it', async t => {
  let unavailable = true, transcriptReads = 0;
  const other = saved();
  const view = mount(t, [saved({ run: run() }), other], { api: {
    listThreads: async () => { if (unavailable) throw new Error('temporary IPC failure'); return []; },
    getThreadView: async () => { transcriptReads++; return { items: [] }; },
  } });
  await settle();
  assert.equal(view.locked(), true);
  assert.match(view.error(), /temporary IPC failure/);
  unavailable = false;
  await view.pollRun();
  assert.equal(transcriptReads, 0);
  assert.equal(view.persisted()[0].run.status, 'error');
  assert.equal(view.locked(), false);
  view.choose(other.id);
  assert.equal(view.activeId(), other.id);
});

test('cleared output stays cleared across app selection and remount; history stays linked', async t => {
  const app = saved({ run: run() }), other = saved();
  const view = mount(t, [app, other], { api: {
    listThreads: async () => [{ id: 'mock-thread', running: false }],
    getThreadView: async () => ({ items: [answer('old report'), turn()] }),
  } });
  await settle();
  assert.equal(view.values().report, 'old report');
  view.switchMode('try');
  view.activate('clearReport');
  assert.equal(view.values().report, '');
  view.choose(other.id);
  view.choose(app.id);
  assert.equal(view.values().report, '');
  const reopened = mount(t, view.persisted());
  assert.equal(reopened.values().report, '');
  assert.equal(reopened.lastThread(), 'mock-thread');
  assert.match(reopened.logs().join('\n'), /执行会话/);
});

test('published mode disables both publish button and handler; draft mode can publish', t => {
  const app = saved({ published: { ...clone(model.DEMO_APP), name: 'V1' }, draft: { ...clone(model.DEMO_APP), name: 'V2' } });
  const view = mount(t, [app]);
  view.choose(app.id, true);
  assert.equal(view.doc().name, 'V1');
  assert.equal(view.publishDisabled(), true);
  view.publish();
  assert.equal(view.persisted()[0].published.name, 'V1');
  view.switchMode('design');
  assert.equal(view.publishDisabled(), false);
  view.publish();
  assert.equal(view.persisted()[0].published.name, 'V2');
});

test('each publication is a version; the quick panel keeps exactly one, switchable without duplicates', t => {
  const legacy = saved({ published: { ...clone(model.DEMO_APP), name: 'V1' }, draft: { ...clone(model.DEMO_APP), name: 'V2' } });
  const view = mount(t, [legacy]);
  view.choose(legacy.id);
  assert.equal(view.publishLabel(), '发布 v2', 'a pre-version publication counts as v1');
  view.publish();
  let stored = view.persisted()[0];
  assert.deepEqual(stored.versions.map(v => [v.version, v.app.name]), [[1, 'V1'], [2, 'V2']]);
  assert.deepEqual([stored.publishedVersion, stored.published.name], [2, 'V2']);
  assert.equal(view.publishDisabled(), true, 'an unchanged draft has nothing to publish');
  view.pinVersion(stored.versions[0]);
  stored = view.persisted()[0];
  assert.deepEqual([stored.publishedVersion, stored.published.name, stored.versions.length, stored.draft.name], [1, 'V1', 2, 'V2']);
  view.pinVersion(stored.versions[1]);
  view.restoreVersion(stored.versions[0]);
  assert.deepEqual([view.persisted()[0].draft.name, view.persisted()[0].published.name], ['V1', 'V2']);
  assert.equal(view.publishLabel(), '切回 v1');
  view.publish();
  stored = view.persisted()[0];
  assert.deepEqual([stored.versions.length, stored.publishedVersion, stored.published.name], [2, 1, 'V1'], 're-publishing a known version re-pins it');
  view.unpublish();
  stored = view.persisted()[0];
  assert.deepEqual([stored.published, stored.publishedVersion, stored.versions.length], [undefined, undefined, 2]);
  assert.equal(view.error(), '');
  assert.equal(mount(t, view.persisted()).error(), '');
});

test('design writes adopt other windows\' run receipts but refuse a concurrent edit of the same design', t => {
  const entry = saved({ draft: { ...clone(model.DEMO_APP), name: 'Draft' } }), other = saved();
  const view = mount(t, [entry, other]);
  view.choose(entry.id);
  view.storage.setItem(KEY, JSON.stringify([{ ...entry, run: run({ status: 'finished', output: 'from runtime' }) }, other]));
  view.publish();
  assert.equal(view.error(), '');
  assert.deepEqual([view.persisted()[0].published.name, view.persisted()[0].run.output], ['Draft', 'from runtime']);
  const edited = { ...view.persisted()[0], draft: { ...clone(model.DEMO_APP), name: 'Edited elsewhere' } };
  view.storage.setItem(KEY, JSON.stringify([edited, other]));
  view.unpublish();
  assert.match(view.error(), /其他窗口修改/);
  assert.deepEqual(view.persisted(), [edited, other]);
});

test('runtimeAppId selects only its published app and ignores workbench open requests', t => {
  const other = saved({ published: clone(model.DEMO_APP) });
  const entry = saved({ published: { ...clone(model.DEMO_APP), name: 'Published runtime' } });
  entry.draft.name = 'Unpublished edits';
  entry.published.pages[1].blocks.find(block => block.id === 'tasks').value = 'Published default';
  let consumed = 0;
  const view = mount(t, [other, entry], { sessionStorage: {
    getItem: () => { consumed++; return JSON.stringify({ appId: other.id }); }, removeItem() {},
  } }, { runtimeAppId: entry.id });
  assert.equal(view.error(), '');
  assert.equal(view.activeId(), entry.id);
  assert.deepEqual(view.doc(), entry.published);
  assert.deepEqual(view.titles, ['Published runtime']);
  assert.equal(view.values().tasks, 'Published default');
  assert.equal(view.publishDisabled(), true);
  view.publish();
  assert.deepEqual(view.persisted(), [other, entry]);
  assert.equal(consumed, 0);
  assert.deepEqual(view.calls, []);
});

test('missing or unpublished runtime apps report an error without selecting or creating a fallback', async t => {
  for (const missing of [true, false]) await t.test(missing ? 'missing ID' : 'unpublished ID', t => {
    const entry = saved();
    const view = mount(t, [entry], {}, { runtimeAppId: missing ? randomUUID() : entry.id });
    assert.match(view.error(), /不存在|尚未发布/);
    assert.equal(view.activeId(), '');
    assert.equal(view.doc(), undefined);
    assert.deepEqual(view.persisted(), [entry]);
    assert.deepEqual(view.calls, []);
  });
});

test('page launches supply tabTag without field bindings, keep it for later actions and replace only on accepted launches', async t => {
  for (const automatic of [false, true]) await t.test(automatic ? 'automatic action' : 'manual action', async t => {
    const entry = saved({ published: clone(model.DEMO_APP) });
    entry.published.browser = { match: ['https://example.com/*'], ...(automatic ? { actionId: 'draft' } : {}) };
    entry.published.pages[1].blocks.find(block => block.id === 'tasks').value = 'Runtime input';
    const first = { url: 'https://example.com/work/1', title: 'Work "data"', tabTag: 'C1-source' };
    const second = { ...first, tabTag: 'C2-other' };
    let launch = first;
    const sent = [];
    const view = mount(t, [entry], {
      launch: () => { const context = launch; launch = null; return context; },
      invoke: async command => {
        assert.equal(command, 'miniapp_scene_model');
        return { agentKind: 'lyra', model: 'runtime-model' };
      },
      api: { sendPrompt: async (_id, prompt, images) => { sent.push({ prompt, images }); },
        listThreads: async () => [{ id: 'mock-thread', running: false }],
        getThreadView: async () => ({ items: [answer('done'), turn()] }) },
    }, { runtimeAppId: entry.id });
    await settle();
    if (!automatic) view.activate('draft');
    await settle();
    assert.equal(sent.length, 1, view.error());
    assert.ok(sent[0].prompt.includes(JSON.stringify(first)));
    assert.match(sent[0].prompt, /必须携带上面的 tabTag/);
    assert.equal(view.values().tasks, 'Runtime input', 'source context does not overwrite user fields');
    assert.deepEqual(sent[0].images, []);

    launch = second;
    view.emit('miniapp:launch');
    await settle();
    assert.match(view.error(), /正在执行/);
    assert.equal(sent.length, 1, 'a busy launch does not run or change the current source');
    await view.pollRun();
    view.activate('draft');
    await settle();
    assert.ok(sent.at(-1).prompt.includes(JSON.stringify(first)), 'later actions retain the original page');
    await view.pollRun();

    for (const context of [second, { url: first.url, title: first.title }, {}]) {
      launch = context;
      view.emit('miniapp:launch');
      await settle();
      if (!view.locked()) view.activate('draft');
      await settle();
      const prompt = sent.at(-1).prompt;
      assert.doesNotMatch(prompt, /C1-source/);
      if (context.tabTag) assert.ok(prompt.includes(JSON.stringify(second)));
      else if (context.url) {
        assert.match(prompt, /当前扩展未提供 tabTag/);
        assert.match(prompt, /不得猜测当前激活页/);
        assert.doesNotMatch(prompt, /C2-other/);
      } else assert.doesNotMatch(prompt, /启动网页上下文|C2-other/);
      await view.pollRun();
    }
    assert.doesNotMatch(view.storage.getItem(KEY), /C1-source|C2-other/, 'source identity is launch-local, never an app setting');
  });
});

test('runtime Nova activation dispatches published actions through api.sendPrompt, not the main-window store', async t => {
  const entry = saved({ published: clone(model.DEMO_APP) });
  entry.published.actions.find(action => action.id === 'draft').prompt = 'Published action: {{tasks}}';
  entry.draft.actions.find(action => action.id === 'draft').prompt = 'Draft action must not run';
  let sent = 0;
  const view = mount(t, [entry], {
    invoke: async command => {
      assert.equal(command, 'miniapp_scene_model');
      return { agentKind: 'lyra', model: 'runtime-model' };
    },
    api: {
      createThread: async (...args) => {
        assert.deepEqual(args, ['mock-dir', 'lyra', 'runtime-model', 'build', null, false, false, null, null, null, 'lightweight']);
        return { id: 'mock-thread' };
      },
      sendPrompt: async (threadId, prompt, attachments) => {
        sent++;
        assert.equal(threadId, 'mock-thread');
        assert.equal(view.persisted()[0].run.threadId, threadId, 'persist before dispatch');
        assert.match(prompt, /Published action: \{\{tasks\}\}/);
        assert.doesNotMatch(prompt, /Draft action must not run/);
        assert.ok(prompt.includes(JSON.stringify(view.values())));
        assert.deepEqual(attachments, []);
      },
    },
  }, { runtimeAppId: entry.id });
  view.setValues({ ...view.values(), tasks: 'Runtime input' });
  view.activate('draft');
  assert.equal(view.locked(), true);
  view.activate('draft');
  await settle();
  assert.equal(view.error(), '');
  assert.equal(sent, 1);
  assert.equal(view.lastThread(), 'mock-thread');
  assert.equal(view.persisted()[0].run.status, 'running');
  assert.deepEqual(view.calls, ['miniapp_scene_model', 'scratchDir', 'createThread', 'sendPrompt']);
});

test('run dispatch and final receipts merge newer cross-window designs and sibling app records', async t => {
  const entry = saved({ draft: commandApp(), published: commandApp() }), other = saved();
  const view = mount(t, [entry, other], { invoke: async (command, { runId }) => {
    assert.ok(['miniapp_command_start', 'miniapp_command_status'].includes(command));
    return { runId, status: 'finished', stdout: 'done', output: 'done', exitCode: 0, truncated: false };
  } }, { runtimeAppId: entry.id });
  const latest = clone(entry);
  latest.draft.name = 'Concurrent draft';
  latest.published.name = 'Concurrent publication';
  latest.messages.push({ role: 'user', text: 'Keep the other window edits' });
  latest.history.push(clone(entry.draft));
  const sibling = { ...other, run: run({ threadId: 'other-thread', status: 'finished', output: 'other result' }) };
  view.storage.setItem(KEY, JSON.stringify([sibling, latest]));
  view.setValues({ ...view.values(), tasks: 'sample' });
  view.activate('draft');
  await settle();
  assert.equal(view.error(), '');
  const dispatched = view.persisted().find(app => app.id === entry.id);
  assert.deepEqual(dispatched, { ...latest, updatedAt: dispatched.updatedAt, run: dispatched.run });
  assert.equal(dispatched.run.status, 'running');
  assert.deepEqual(view.persisted()[0], sibling);

  const newest = { ...dispatched, draft: { ...dispatched.draft, description: 'Edited again while running' } };
  const added = saved();
  view.storage.setItem(KEY, JSON.stringify([added, newest, sibling]));
  await view.pollRun();
  const finished = view.persisted()[1];
  assert.equal(view.error(), '');
  assert.deepEqual(view.persisted(), [added, { ...newest, updatedAt: finished.updatedAt,
    run: { ...dispatched.run, status: 'finished', output: 'done\n退出码：0' } }, sibling]);
  assert.equal(view.locked(), false);
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status']);
});

test('removing an app in another window prevents dispatch or receipt writes from recreating it', async t => {
  for (const stage of ['dispatch', 'receipt']) await t.test(stage, async t => {
    const entry = saved({ draft: commandApp(), published: commandApp() }), other = saved();
    const view = mount(t, [entry, other], { invoke: async (command, { runId }) => {
      assert.ok(['miniapp_command_start', 'miniapp_command_status'].includes(command));
      return { runId, status: 'finished', stdout: 'done', output: 'done', exitCode: 0, truncated: false };
    } }, { runtimeAppId: entry.id });
    view.setValues({ ...view.values(), tasks: 'sample' });
    if (stage === 'dispatch') view.storage.setItem(KEY, JSON.stringify([other]));
    view.activate('draft');
    await settle();
    if (stage === 'receipt') {
      view.storage.setItem(KEY, JSON.stringify([other]));
      await view.pollRun();
    }
    assert.match(view.error(), /移除|不存在/);
    assert.deepEqual(view.persisted(), [other]);
    assert.deepEqual(view.calls, stage === 'dispatch' ? [] : ['miniapp_command_start', 'miniapp_command_status']);
  });
});

test('a concurrently started run blocks duplicate commands without adding sendError to the other window run', async t => {
  const entry = saved({ draft: commandApp(), published: commandApp() });
  const view = mount(t, [entry], {}, { runtimeAppId: entry.id });
  const existing = { ...entry, run: run({ threadId: '', commandId: randomUUID() }) };
  view.storage.setItem(KEY, JSON.stringify([existing]));
  view.setValues({ ...view.values(), tasks: 'sample' });
  view.activate('draft');
  await settle();
  assert.match(view.error(), /另一窗口运行|重复执行/);
  assert.deepEqual(view.calls, [], 'do not send any native start or AI request');
  assert.deepEqual(view.persisted(), [existing]);
  assert.deepEqual(view.apps()[0].run, existing.run);
  assert.equal(view.persisted()[0].run.sendError, undefined);
  assert.equal(view.locked(), true);
});

test('active-run permissions allow_once only, skip questions and other threads, and never retry a request key', async t => {
  for (const runtime of [false, true]) for (const fails of [false, true]) {
    await t.test(`${runtime ? 'runtime' : 'workbench'} / ${fails ? 'response fails' : 'response succeeds'}`, async t => {
      const permission = patch => ({ threadId: 'mock-thread', requestKey: randomUUID(), toolCall: { title: 'Fixture tool' },
        options: [
          { kind: 'allow_always', optionId: 'always', name: 'Always' },
          { kind: 'reject_once', optionId: 'deny', name: 'Deny' },
          { kind: 'allow_once', optionId: 'once', name: 'Once' },
        ], ...patch });
      const request = permission(), question = permission({ questions: [{ header: 'Input', question: 'Choose a value', options: [] }] });
      const noOnce = permission({ options: [{ kind: 'allow_always', optionId: 'always', name: 'Always' }] });
      const unrelated = permission({ threadId: 'other-thread' });
      const pending = Promise.withResolvers(), responses = [];
      const state = { permissions: runtime ? [permission()] : [], settings: {} };
      let running = true;
      const entry = saved({ published: clone(model.DEMO_APP), run: run() });
      const view = mount(t, [entry], { state, invoke: async (command, args) => {
        assert.equal(runtime, true, 'only independent windows recover permissions through IPC');
        assert.equal(command, 'miniapp_pending_permissions');
        assert.deepEqual(args, { threadId: 'mock-thread' });
        return [];
      }, api: {
        listThreads: async () => [{ id: 'mock-thread', running }],
        getThreadView: async () => ({ items: running ? [] : [answer('done'), turn()] }),
        respondPermission: (...args) => { responses.push(args); return pending.promise; },
      } }, runtime ? { runtimeAppId: entry.id } : {});
      await settle();
      const requests = [unrelated, question, noOnce, request];
      if (runtime) requests.forEach(item => view.emit('acp:permission', item));
      else state.permissions = requests;
      for (let i = 0; i < 3; i++) view.flushEffects();
      assert.deepEqual(responses, [[request.requestKey, 'once']]);
      if (runtime) view.emit('acp:permission', request);
      view.flushEffects();
      assert.equal(responses.length, 1, 'duplicate events and rerenders cannot duplicate pending responses');
      if (fails) pending.reject(new Error('permission transport failed'));
      else pending.resolve();
      await settle();
      for (let i = 0; i < 3; i++) view.flushEffects();
      assert.equal(responses.length, 1, 'settling a response does not permit an automatic retry');
      if (fails) assert.match(view.error(), /permission transport failed/);
      else assert.equal(view.error(), '');
      assert.equal(view.permissions().some(item => item.threadId === 'other-thread'), false);
      if (runtime) {
        view.emit('acp:permission-resolved', { requestKey: request.requestKey });
        assert.equal(view.permissions().some(item => item.requestKey === request.requestKey), false);
      }
      running = false;
      await view.pollRun();
      assert.equal(view.persisted()[0].run.status, 'finished');
      if (runtime) view.emit('acp:permission', permission());
      else state.permissions = [permission()];
      view.flushEffects();
      assert.equal(responses.length, 1, 'completed runs cannot authorize new requests');
    });
  }
});

test('reopened runtime recovers pending permissions for only its running thread without replaying the action', async t => {
  const entry = saved({ published: clone(model.DEMO_APP), run: run() });
  const other = saved({ published: clone(model.DEMO_APP), run: run({ threadId: 'other-thread' }) });
  const request = { threadId: 'mock-thread', requestKey: randomUUID(), toolCall: { title: 'Restored fixture tool' }, options: [
    { kind: 'allow_always', optionId: 'always', name: 'Always' },
    { kind: 'allow_once', optionId: 'once', name: 'Once' },
  ] };
  const question = { ...request, requestKey: randomUUID(), questions: [{ header: 'Input', question: 'Choose a value', options: [] }] };
  const unrelated = { ...request, threadId: 'other-thread', requestKey: randomUUID() };
  const queried = [], responses = [];
  const view = mount(t, [other, entry], {
    invoke: async (command, args) => {
      assert.equal(command, 'miniapp_pending_permissions');
      assert.deepEqual(args, { threadId: entry.run.threadId });
      queried.push(args.threadId);
      return [unrelated, question, request];
    },
    api: {
      listThreads: async () => [other, entry].map(app => ({ id: app.run.threadId, running: true })),
      getThreadView: async threadId => { assert.equal(threadId, entry.run.threadId); return { items: [] }; },
      respondPermission: async (...args) => { responses.push(args); },
    },
  }, { runtimeAppId: entry.id });
  // No permission events are emitted: a reopened window must recover requests it missed.
  await settle();
  view.flushEffects();
  await settle();
  assert.equal(view.error(), '');
  assert.deepEqual(queried, [entry.run.threadId]);
  assert.deepEqual(view.permissions(), [question, request]);
  assert.deepEqual(responses, [[request.requestKey, 'once']], 'questions and other threads never auto-respond');
  await view.pollRun();
  view.flushEffects();
  await settle();
  assert.equal(view.error(), '');
  assert.deepEqual(queried, [entry.run.threadId, entry.run.threadId]);
  assert.deepEqual(responses, [[request.requestKey, 'once']], 'repeated snapshots do not respond twice');
  assert.equal(view.locked(), true);
  assert.deepEqual(view.persisted(), [other, entry]);
  assert.deepEqual(view.calls, [
    'listThreads', 'getThreadView', 'miniapp_pending_permissions', 'respondPermission',
    'listThreads', 'getThreadView', 'miniapp_pending_permissions',
  ], 'reopening only polls and responds; it never sends a prompt or starts a command');
});

test('showThread routes through native runtime IPC or main-window openThread and reports failures', async t => {
  for (const runtime of [false, true]) for (const fails of [false, true]) {
    await t.test(`${runtime ? 'runtime' : 'workbench'} / ${fails ? 'failure' : 'success'}`, async t => {
      const entry = saved({ published: clone(model.DEMO_APP), run: run({ status: 'finished' }) });
      const opened = [];
      const open = async threadId => { opened.push(threadId); if (fails) throw new Error('cannot open thread'); };
      const view = mount(t, [entry], {
        openThread: open,
        invoke: async (command, args) => { assert.equal(command, 'miniapp_show_thread'); await open(args.threadId); },
      }, runtime ? { runtimeAppId: entry.id } : {});
      await view.showThread();
      assert.deepEqual(opened, ['mock-thread']);
      assert.deepEqual(view.calls, [runtime ? 'miniapp_show_thread' : 'openThread']);
      if (fails) assert.match(view.error(), /cannot open thread/);
      else assert.equal(view.error(), '');
      if (!runtime) assert.deepEqual(view.titles, [], 'workbench does not replace the main window title');
    });
  }
});

test('unreadable libraries, failed writes and stale design windows never overwrite saved data', async t => {
  for (const failure of ['read', 'format', 'write', 'conflict']) await t.test(failure, t => {
    const entry = saved();
    let raw = failure === 'format' ? '{invalid JSON' : JSON.stringify([entry]);
    let writes = 0;
    const view = mount(t, [entry], { localStorage: {
      getItem: () => { if (failure === 'read') throw new Error('storage unavailable'); return raw; },
      setItem: (_key, value) => { writes++; if (failure === 'write') throw new Error('storage quota'); raw = value; },
    } });
    if (failure === 'conflict') raw = JSON.stringify([{ ...entry, draft: { ...entry.draft, name: 'Newer design' } }]);
    const before = raw;
    view.publish();
    assert.ok(view.error());
    assert.equal(raw, before);
    assert.equal(writes, failure === 'write' ? 1 : 0);
    assert.equal(view.apps()[0]?.published, undefined);
    assert.deepEqual(view.calls, []);
  });
});

test('Ctrl selection toggles across pages; plain selection replaces and mode switches clear it', t => {
  const view = mount(t, [saved()]);
  view.selectBlock('welcome');
  view.selectBlock('intro', true);
  assert.deepEqual(view.selectedIds(), ['welcome', 'intro']);
  view.selectBlock('welcome', true);
  assert.deepEqual(view.selectedIds(), ['intro']);
  view.selectBlock('intro', true);
  assert.deepEqual(view.selectedIds(), []);
  view.selectBlock('welcome');
  view.setPageId('editor');
  assert.deepEqual(view.selectedIds(), ['welcome']);
  view.selectBlock('tasks', true);
  assert.deepEqual(view.selectedIds(), ['welcome', 'tasks']);
  view.selectBlock('intro', true); // Off-page and nonexistent controls cannot be added.
  view.selectBlock('missing');
  assert.deepEqual(view.selectedIds(), ['welcome', 'tasks']);
  view.selectBlock('report');
  view.selectBlock('report');
  assert.deepEqual(view.selectedIds(), ['report']);
  view.switchMode('try');
  view.selectBlock('tasks');
  view.selectBlock('report', true);
  assert.deepEqual(view.selectedIds(), []);
  view.switchMode('design');
  view.selectBlock('tasks');
  view.switchMode('design');
  assert.deepEqual(view.selectedIds(), []);
});

test('generation preserves compatible dirty inputs, adopts new defaults and resets results', async t => {
  const draft = clone(model.DEMO_APP);
  const fields = [
    { id: 'clean', type: 'input', label: 'Clean', value: 'old default' },
    { id: 'changedType', type: 'input', label: 'Type', value: '' },
    { id: 'dirtyEmpty', type: 'input', label: 'Empty', value: 'seed' },
    { id: 'kept', type: 'select', label: 'Kept', options: ['A', 'B'], value: 'A' },
    { id: 'removedOption', type: 'select', label: 'Changed', options: ['A', 'B'], value: 'A' },
  ];
  draft.pages[1].blocks.push(...fields);
  const next = clone(draft);
  for (const block of next.pages[1].blocks) {
    if (block.id === 'clean') block.value = 'new default';
    if (block.id === 'changedType') { block.type = 'textarea'; block.value = 'new type default'; }
    if (block.id === 'kept') { block.options = ['B', 'C']; block.value = 'C'; }
    if (block.id === 'removedOption') { block.options = ['C']; block.value = 'C'; }
  }
  let generated = 0;
  const view = mount(t, [saved({ draft, published: clone(draft) })], { invoke: async (command, { prompt }) => {
    assert.equal(command, 'miniapp_generate');
    assert.deepEqual(designContext(prompt).conversation, view.persisted()[0].messages);
    generated++;
    return JSON.stringify(next);
  } });
  view.setValues({ ...view.values(), tasks: 'typed tasks', report: 'old result', changedType: 'typed', dirtyEmpty: '', kept: 'B', removedOption: 'B' });
  view.setInstruction('Update defaults and field types');
  await view.generate();
  assert.equal(generated, 1);
  assert.equal(view.error(), '');
  assert.deepEqual(view.values(), { ...model.initialValues(next), tasks: 'typed tasks', dirtyEmpty: '', kept: 'B' });
  assert.deepEqual(view.persisted()[0].published, draft);
  const messages = view.persisted()[0].messages;
  assert.equal(messages[1].text, model.makeDesignSummary(draft, next));
  assert.match(messages[1].text, /clean/);
  view.setInstruction('保留本轮修改，不作其它调整');
  await view.generate();
  assert.equal(generated, 2);
  assert.equal(view.error(), '');
  assert.deepEqual(view.persisted()[0].messages.slice(0, 2), messages);
  assert.equal(view.persisted()[0].messages.at(-1).text, '未产生改动');
});

const designContext = prompt => JSON.parse(prompt.slice(prompt.indexOf('\nJSON 上下文：\n') + '\nJSON 上下文：\n'.length));

function missingResult() {
  const app = clone(model.DEMO_APP);
  app.actions.find(action => action.kind === 'nova').target = 'result';
  return JSON.stringify(app);
}

test('generation repairs a dangling result reference once before applying any draft', async t => {
  for (const initial of [null, clone(model.DEMO_APP)]) await t.test(initial ? 'edit' : 'new app', async t => {
    const messages = [
      { role: 'user', text: '仅修改选中控件，保留其它页面', pageId: 'editor', selectedIds: ['tasks'] },
      { role: 'assistant', text: '已记录控件修改意图' },
    ];
    const entry = saved({ draft: initial, messages, ...(initial ? { published: clone(initial) } : {}) });
    const next = clone(model.DEMO_APP);
    next.pages[0].blocks = next.pages[0].blocks.filter(block => block.id !== 'intro');
    next.pages[1].blocks.find(block => block.id === 'tasks').label = '待整理的任务';
    const invalid = missingResult();
    let calls = 0;
    const view = mount(t, [entry], { makeDesignPrompt: (...args) => {
      assert.equal(args.length, 9);
      assert.deepEqual(args[3], initial ? ['welcome', 'intro', 'tasks'] : []);
      assert.equal(args[6], 'powershell');
      assert.deepEqual(args[7], messages);
      assert.equal(args[8], entry.id);
      return model.makeDesignPrompt(...args);
    }, invoke: async (command, { appId, prompt }) => {
      assert.equal(command, 'miniapp_generate');
      assert.equal(appId, entry.id, 'initial request and repair belong to the same design session');
      calls++;
      assert.deepEqual(view.persisted()[0], entry, 'invalid output must never be saved');
      const context = designContext(prompt);
      assert.equal(context.instruction, '做一个构建工具，选择环境，使用 chrome 进行构建');
      assert.deepEqual(context.currentApp, initial);
      assert.equal(context.pageId, initial ? 'editor' : '');
      assert.deepEqual(context.selectedIds, initial ? ['welcome', 'intro', 'tasks'] : []);
      assert.equal(context.runtimeShell, 'powershell');
      assert.deepEqual(context.conversation, messages, 'both initial and repair requests retain prior intent');
      assert.deepEqual(context.selectedControls.map(({ block, pageId }) => [block.id, pageId]),
        initial ? [['welcome', 'home'], ['intro', 'home'], ['tasks', 'editor']] : []);
      if (initial) {
        assert.deepEqual(context.selectedControls[2].block, initial.pages[1].blocks[0]);
        assert.deepEqual(context.selectedControls[2].actions, [initial.actions.find(action => action.id === 'draft')]);
      }
      if (calls === 1) {
        assert.equal(context.repair, undefined);
        view.selectBlock('report'); // Generation locks selection through the repair request.
        assert.deepEqual(view.selectedIds(), context.selectedIds);
        return invalid;
      }
      assert.equal(calls, 2);
      assert.equal(context.repair.response, invalid);
      assert.match(context.repair.error, /target.*result/);
      return JSON.stringify(next);
    } });
    if (initial) {
      view.selectBlock('welcome');
      view.selectBlock('intro', true);
      view.setPageId('editor');
      view.selectBlock('tasks', true);
    }
    view.setInstruction('做一个构建工具，选择环境，使用 chrome 进行构建');
    await view.generate();
    assert.equal(view.error(), '');
    assert.equal(calls, 2);
    assert.deepEqual(view.persisted()[0].draft, next);
    assert.deepEqual(view.persisted()[0].published, entry.published);
    assert.equal(view.persisted()[0].history.length, initial ? 1 : 0);
    assert.deepEqual(view.selectedIds(), initial ? ['welcome', 'tasks'] : []);
    assert.deepEqual(view.persisted()[0].messages, [...messages,
      { role: 'user', text: '做一个构建工具，选择环境，使用 chrome 进行构建', pageId: initial ? 'editor' : '', selectedIds: initial ? ['welcome', 'intro', 'tasks'] : [] },
      { role: 'assistant', text: model.makeDesignSummary(initial, next) },
    ]);
    assert.match(view.persisted()[0].messages.at(-1).text, /tasks/);
    assert.equal(view.instruction(), '');
    assert.equal(view.locked(), false);
  });
});

test('design sessions use distinct app IDs and retain all prior messages beyond twelve entries', async t => {
  const history = Array.from({ length: 14 }, (_, index) => ({
    role: index % 2 ? 'assistant' : 'user', text: `Fixture design turn ${index}`,
    ...(index % 2 ? {} : { pageId: 'editor', selectedIds: ['tasks'] }),
  }));
  const first = saved({ messages: history });
  const second = saved({ messages: [{ role: 'user', text: 'Independent fixture intent' }] });
  let current;
  const requests = [];
  const view = mount(t, [first, second], {
    makeDesignPrompt: (...args) => {
      assert.deepEqual(args[7], current.messages, 'pass complete app-local history, not a shared or sliced transcript');
      return model.makeDesignPrompt(...args);
    },
    invoke: async (command, { appId }) => {
      assert.equal(command, 'miniapp_generate');
      assert.equal(appId, current.id);
      requests.push(appId);
      return JSON.stringify(current.draft); // IPC still returns raw JSON, not a session envelope.
    },
  });
  for (current of [first, second]) {
    view.choose(current.id);
    view.setInstruction('保留当前设计');
    await view.generate();
    assert.equal(view.error(), '');
    assert.deepEqual(view.persisted().find(entry => entry.id === current.id).messages, [...current.messages,
      { role: 'user', text: '保留当前设计', pageId: 'home', selectedIds: [] },
      { role: 'assistant', text: '未产生改动' },
    ]);
  }
  assert.notEqual(first.id, second.id);
  assert.deepEqual(requests, [first.id, second.id]);
  assert.equal(view.persisted()[0].messages.length, 16);
});

test('clarification saves the real reply and feeds it back without resetting selection, preview or app bindings', async t => {
  const draft = optionsApp(), block = draft.pages[0].blocks[1];
  const entry = saved({ draft, published: clone(draft), history: [clone(draft)] });
  const replies = [
    { message: '请明确目录参数应来自哪个输入；当前控件和命令保持不变。', app: null },
    { message: '已确认，保留当前目录输入及命令绑定。', app: clone(draft) },
  ];
  let generated = 0;
  const view = mount(t, [entry], { invoke: async (command, args) => {
    if (command !== 'miniapp_generate') {
      assert.ok(['miniapp_command_start', 'miniapp_command_status'].includes(command));
      return { runId: args.runId, status: 'finished', stdout: '["alpha","beta"]', output: '["alpha","beta"]', exitCode: 0, truncated: false };
    }
    assert.equal(args.appId, entry.id);
    const context = designContext(args.prompt);
    assert.deepEqual(context.currentApp, draft);
    assert.deepEqual(context.conversation, view.persisted()[0].messages);
    assert.deepEqual(context.selectedControls[0].block, block);
    return JSON.stringify(replies[generated++]);
  } });
  view.switchMode('try');
  view.flushEffects();
  await settle();
  await view.pollRun();
  assert.equal(view.optionLoads().item.status, 'ready');
  view.switchMode('design');
  view.selectBlock('item');
  view.setValues({ ...view.values(), item: 'alpha', report: 'Prior preview result' });
  const values = clone(view.values()), loads = clone(view.optionLoads());
  for (const reply of replies) {
    const messages = view.persisted()[0].messages;
    view.setInstruction('保留选中控件，先确认参数');
    await view.generate();
    assert.equal(view.error(), '');
    assert.deepEqual(view.persisted()[0].draft, draft);
    assert.deepEqual(view.persisted()[0].published, entry.published);
    assert.deepEqual(view.persisted()[0].history, entry.history);
    assert.deepEqual(view.values(), values);
    assert.deepEqual(view.optionLoads(), loads);
    assert.deepEqual(view.selectedIds(), ['item']);
    assert.deepEqual(view.persisted()[0].messages, [...messages,
      { role: 'user', text: '保留选中控件，先确认参数', pageId: 'main', selectedIds: ['item'] },
      { role: 'assistant', text: reply.message + (reply.app ? '\n\n未产生改动' : '') },
    ]);
  }
  assert.equal(generated, 2);
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_generate', 'miniapp_generate']);
});

test('a failed repair is bounded and preserves the draft, publication, input and history', async t => {
  const entry = saved({ published: clone(model.DEMO_APP) });
  let calls = 0;
  const view = mount(t, [entry], { invoke: async () => { calls++; return missingResult(); } });
  view.setInstruction('保留现有页面，增加构建按钮');
  await view.generate();
  assert.equal(calls, 2);
  assert.match(view.error(), /自动修正后仍未通过/);
  assert.deepEqual(view.persisted()[0], entry);
  assert.equal(view.instruction(), '保留现有页面，增加构建按钮');
  assert.equal(view.locked(), false);
});

test('transport errors are never retried, including during a repair', async t => {
  for (const repair of [false, true]) await t.test(repair ? 'repair request' : 'initial request', async t => {
    const entry = saved();
    let calls = 0;
    const view = mount(t, [entry], { invoke: async () => {
      calls++;
      if (repair && calls === 1) return missingResult();
      throw new Error('模型连接失败');
    } });
    view.setInstruction('增加一个页面');
    await view.generate();
    assert.equal(calls, repair ? 2 : 1);
    assert.match(view.error(), /模型连接失败/);
    assert.deepEqual(view.persisted()[0], entry);
    assert.equal(view.instruction(), '增加一个页面');
    assert.equal(view.locked(), false);
  });
});

test('oversized invalid output is not truncated or submitted as an oversized repair', async t => {
  const entry = saved();
  let calls = 0;
  const view = mount(t, [entry], { invoke: async () => { calls++; return '中'.repeat(60_000); } });
  view.setInstruction('增加页面');
  await view.generate();
  assert.equal(calls, 1);
  assert.match(view.error(), /过长/);
  assert.deepEqual(view.persisted()[0], entry);
  assert.equal(view.locked(), false);
});

const commandAction = { id: 'draft', name: 'Echo input', kind: 'command', script: 'Write-Output $nova.tasks', shell: 'powershell', inputs: ['tasks'], timeoutSeconds: 30, target: 'report' };
function commandApp() {
  const app = clone(model.DEMO_APP);
  app.actions = app.actions.map(action => action.id === 'draft' ? clone(commandAction) : action);
  app.tests.push({ name: 'Command fixture is not execution evidence', actionId: 'draft', inputs: { tasks: 'sample' }, expect: { values: { report: 'not executed' } } });
  return app;
}

test('design, generation and checks never start commands or pretend their results passed', async t => {
  const draft = commandApp();
  const view = mount(t, [saved({ draft })], { invoke: async command => {
    assert.equal(command, 'miniapp_generate');
    return JSON.stringify(draft);
  } });
  view.setValues({ ...view.values(), tasks: 'sample' });
  view.activate('draft');
  assert.deepEqual(view.calls, []);
  view.setInstruction('保留脚本，只调整说明');
  await view.generate();
  view.test();
  assert.equal(view.error(), '');
  assert.equal(view.checks().some(check => check.status === 'fail'), false);
  const commandChecks = view.checks().filter(check => /Echo input/.test(check.name));
  assert.equal(commandChecks.length, 2, 'action and safety reminder stay explicitly unverified');
  assert.ok(commandChecks.every(check => check.status === 'unverified'));
  assert.equal(view.checks().some(check => check.name.includes('Command fixture')), false, 'sample expectations are not fabricated execution results');
  assert.equal(view.persisted()[0].run, undefined);
  assert.equal(view.values().report, '');
  assert.deepEqual(view.calls, ['miniapp_generate']);
});

test('advisory checks do not gate publication or direct command execution', async t => {
  for (const failedCheck of [false, true]) await t.test(failedCheck ? 'failed local check' : 'risk reminder', async t => {
    const draft = commandApp();
    draft.actions.find(action => action.id === 'draft').script = 'Remove-Item -LiteralPath $nova.tasks';
    const view = mount(t, [saved({ draft })], {
      ...(failedCheck ? { checkMiniApp: () => [{ name: 'Local example mismatch', status: 'fail', detail: 'Advisory only' }] } : {}),
      invoke: async (command, { runId }) => {
        assert.equal(command, 'miniapp_command_start');
        return { runId, status: 'running', output: '', truncated: false };
      },
    });
    view.test();
    assert.ok(view.checks().some(check => failedCheck ? check.status === 'fail' : /潜在风险/.test(check.detail)));
    assert.deepEqual(view.calls, [], 'checks never execute tools');
    assert.equal(view.publishDisabled(), false);
    view.publish();
    assert.deepEqual(view.persisted()[0].published, draft);
    assert.equal(view.error(), '');
    assert.deepEqual(view.calls, []);
    view.switchMode('try');
    view.setValues({ ...view.values(), tasks: 'synthetic-fixture-only' });
    view.activate('draft');
    await settle();
    assert.equal(view.error(), '');
    assert.equal(view.persisted()[0].run.status, 'running');
    assert.deepEqual(view.calls, ['miniapp_command_start'], 'IPC is stubbed; no real shell or deletion runs');
  });
});

test('command activation persists its ID before one native start; inputs stay literal and duplicates stay locked', async t => {
  const entry = saved({ draft: commandApp() });
  const pending = Promise.withResolvers();
  // This is an IPC contract test: no shell executes, even for injection-shaped input.
  const input = `"'; Write-Output 'INJECTION'; $env:PATH; $(Get-Date)\n{{report}}`;
  let snapshot = { status: 'running', output: '', truncated: false };
  const view = mount(t, [entry], { invoke: async (command, args) => {
    const durable = view.persisted()[0].run;
    assert.match(durable.commandId, /^[0-9a-f-]{36}$/i);
    if (command === 'miniapp_command_start') {
      assert.equal(durable.status, 'running');
      assert.equal(durable.threadId, '');
      assert.equal(durable.target, 'report');
      assert.deepEqual(args, { runId: durable.commandId, appId: entry.id, script: commandAction.script, shell: 'powershell', inputs: { tasks: input }, timeoutSeconds: 30 });
      return pending.promise;
    }
    assert.equal(command, 'miniapp_command_status');
    assert.deepEqual(args, { runId: durable.commandId });
    return { runId: durable.commandId, ...snapshot };
  } });
  view.switchMode('try');
  view.setValues({ ...view.values(), tasks: '   ' });
  view.activate('draft');
  assert.match(view.error(), /必填/);
  assert.deepEqual(view.persisted()[0], entry);
  assert.deepEqual(view.calls, []);
  view.setValues({ ...view.values(), tasks: input, report: 'unrelated data must not be sent' });
  view.activate('draft');
  assert.equal(view.locked(), true);
  assert.equal(view.persisted()[0].run.status, 'running');
  await view.startRun(commandAction);
  view.activate('draft');
  await view.pollRun();
  assert.deepEqual(view.calls, ['miniapp_command_start']);
  pending.resolve({ runId: view.apps()[0].run.commandId, ...snapshot });
  await settle();
  assert.equal(view.error(), '');
  assert.equal(view.lastThread(), '');
  assert.equal(view.locked(), true);
  view.activate('draft');
  await view.startRun(commandAction);
  snapshot = { status: 'running', output: 'partial command output', truncated: false };
  await view.pollRun();
  assert.equal(view.locked(), true);
  assert.equal(view.values().report, 'partial command output');
  assert.equal(view.persisted()[0].run.status, 'running');
  assert.equal(view.persisted()[0].run.output, '', 'partial output is not a saved final result');
  snapshot = { status: 'finished', output: 'literal input accepted', exitCode: 0, truncated: false };
  await view.pollRun();
  assert.equal(view.persisted()[0].run.status, 'finished');
  assert.equal(view.persisted()[0].run.output, 'literal input accepted\n退出码：0');
  assert.equal(view.values().report, view.persisted()[0].run.output);
  assert.equal(view.values().tasks, input);
  assert.equal(view.locked(), false);
  await view.pollRun();
  await view.startRun();
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_status']);
});

test('native terminal failures and missing status unlock, show results and never restart on remount', async t => {
  const cases = [
    ['nonzero exit', { status: 'error', output: 'stderr', exitCode: 7, truncated: true }, /stderr\n退出码：7\n输出已截断/],
    ['timeout', { status: 'error', output: '执行超时（30 秒）', exitCode: null, truncated: false }, /执行超时/],
    ['error without output', { status: 'error', output: '', truncated: false }, /无输出/],
    ['missing after restart', null, /记录已不存在[\s\S]*不会自动重新执行/],
  ];
  for (const [name, snapshot, output] of cases) await t.test(name, async t => {
    const record = run({ threadId: '', commandId: randomUUID() });
    const view = mount(t, [saved({ draft: commandApp(), run: record })], { invoke: async (command, args) => {
      assert.equal(command, 'miniapp_command_status');
      assert.deepEqual(args, { runId: record.commandId });
      return snapshot && { runId: record.commandId, ...snapshot };
    } });
    assert.equal(view.locked(), true);
    await settle();
    assert.equal(view.persisted()[0].run.status, 'error');
    assert.match(view.persisted()[0].run.output, output);
    assert.equal(view.values().report, view.persisted()[0].run.output);
    assert.equal(view.locked(), false);
    await view.pollRun();
    await view.startRun();
    assert.deepEqual(view.calls, ['miniapp_command_status']);
    const reopened = mount(t, view.persisted());
    await settle();
    assert.equal(reopened.locked(), false);
    assert.deepEqual(reopened.calls, []);
  });
});

test('command cancellation uses native IPC and waits for native terminal status, not cancelTurn', async t => {
  const record = run({ threadId: '', commandId: randomUUID() });
  let status = 'running';
  const view = mount(t, [saved({ draft: commandApp(), run: record })], { invoke: async (command, args) => {
    assert.deepEqual(args, { runId: record.commandId });
    if (command === 'miniapp_command_cancel') return;
    assert.equal(command, 'miniapp_command_status');
    return { runId: record.commandId, status, output: 'stopped output', truncated: false };
  } });
  await settle();
  await view.cancelRun();
  await settle();
  assert.equal(view.persisted()[0].run.cancelRequested, true);
  assert.equal(view.locked(), true);
  status = 'cancelled';
  await view.pollRun();
  assert.equal(view.persisted()[0].run.status, 'cancelled');
  assert.equal(view.values().report, 'stopped output');
  assert.equal(view.locked(), false);
  await view.cancelRun();
  assert.deepEqual(view.calls, ['miniapp_command_status', 'miniapp_command_cancel', 'miniapp_command_status', 'miniapp_command_status']);
});

test('native start/status/cancel errors never call AI or retry start; absent status resolves the durable failure', async t => {
  let unavailable = true;
  const view = mount(t, [saved({ draft: commandApp() })], { invoke: async command => {
    if (command === 'miniapp_command_start') throw new Error('native start failed');
    if (command === 'miniapp_command_cancel') throw new Error('native cancel failed');
    assert.equal(command, 'miniapp_command_status');
    if (unavailable) throw new Error('native status failed');
    return null;
  } });
  view.switchMode('try');
  view.setValues({ ...view.values(), tasks: 'sample' });
  view.activate('draft');
  await settle();
  assert.match(view.error(), /native start failed/);
  assert.equal(view.persisted()[0].run.sendError, 'native start failed');
  assert.equal(view.locked(), true);
  view.activate('draft');
  await view.startRun();
  await view.pollRun();
  assert.match(view.error(), /native status failed/);
  assert.equal(view.locked(), true);
  await view.cancelRun();
  assert.match(view.error(), /native cancel failed/);
  assert.equal(view.persisted()[0].run.cancelRequested, undefined);
  assert.equal(view.locked(), true);
  unavailable = false;
  await view.pollRun();
  assert.equal(view.persisted()[0].run.status, 'error');
  assert.equal(view.values().report, 'native start failed');
  assert.equal(view.locked(), false);
  await view.pollRun();
  await view.startRun();
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_cancel', 'miniapp_command_status']);
});

function optionsApp() {
  // Generic, synthetic command data source: no user app, repository or business paths.
  return {
    version: 1, name: 'Fixture catalog', description: '', startPage: 'main', accent: 'teal',
    pages: [{ id: 'main', title: 'Catalog', layout: 'stack', blocks: [
      { id: 'catalog', type: 'input', label: 'Catalog', value: 'alpha', required: true },
      { id: 'item', type: 'select', label: 'Catalog item', options: [], value: '', required: true, searchable: true, optionsActionId: 'loadItems', autoLoad: true },
      { id: 'report', type: 'result', label: 'Build output' },
    ] }],
    actions: [
      { id: 'loadItems', name: 'Load catalog', kind: 'command', target: 'item', shell: 'powershell', script: "@($nova.catalog, 'beta') | ConvertTo-Json -Compress", inputs: ['catalog'] },
      { id: 'build', name: 'Build selected item', kind: 'command', target: 'report', shell: 'powershell', script: 'Write-Output $nova.item', inputs: ['catalog', 'item'] },
    ], tests: [],
  };
}

test('dynamic select auto-load runs directly only at runtime, consumes stdout and rejects stale build inputs without AI', async t => {
  const draft = optionsApp(), block = draft.pages[0].blocks[1];
  let snapshot = { status: 'running', output: '["partial"]\nsource diagnostics', stdout: '["partial"]', truncated: false };
  const view = mount(t, [saved({ draft })], { invoke: async (command, args) => {
    const record = view.persisted()[0].run;
    assert.equal(args.runId, record.commandId);
    if (command === 'miniapp_command_start') {
      assert.equal(record.target, block.id);
      assert.equal(record.threadId, '');
      assert.equal(args.script, draft.actions[0].script);
      assert.deepEqual(args.inputs, { catalog: view.values().catalog });
    } else assert.equal(command, 'miniapp_command_status');
    return { runId: record.commandId, ...snapshot };
  } });
  assert.equal(view.error(), '');
  view.flushEffects();
  view.requestOptions(block);
  view.openOptions(block);
  assert.deepEqual(view.optionLoads(), {});
  assert.deepEqual(view.calls, []);

  view.switchMode('try');
  view.flushEffects();
  assert.equal(view.optionLoads().item.status, 'running');
  assert.equal(view.persisted()[0].run.status, 'running');
  assert.deepEqual(view.calls, ['miniapp_command_start'], 'auto-load starts directly without a confirmation step');
  view.flushEffects();
  view.openOptions(block);
  await settle();
  await view.pollRun();
  view.flushEffects();
  assert.equal(view.optionLoads().item.status, 'running');
  assert.equal(view.values().item, '', 'partial command output is never a selected value');
  assert.deepEqual(view.availableOptions(block), []);

  snapshot = { status: 'finished', stdout: '["alpha","beta"]', output: '["alpha","beta"]\nwarning: ["stderr-only"]', exitCode: 0, truncated: false };
  await view.pollRun();
  const key = view.optionsKey(block);
  assert.deepEqual(view.optionLoads().item, { key, status: 'ready', options: ['alpha', 'beta'] });
  assert.deepEqual(view.availableOptions(block), ['alpha', 'beta'], 'only stdout, never merged stderr, supplies options');
  assert.equal(view.values().item, '', 'loading does not select a default or write JSON into the value');
  assert.equal(view.persisted()[0].run.status, 'finished');
  view.setValues({ ...view.values(), item: 'alpha' });
  for (let i = 0; i < 3; i++) {
    view.openOptions(block);
    view.requestOptions(block);
    assert.deepEqual(view.availableOptions(block), ['alpha', 'beta']);
    view.flushEffects();
  }
  assert.equal(view.values().item, 'alpha');
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_status']);

  view.setValues({ ...view.values(), catalog: 'changed-catalog' });
  assert.notEqual(view.optionsKey(block), key);
  assert.deepEqual(view.availableOptions(block), []);
  view.activate('build');
  assert.match(view.error(), /选项.*(?:未加载|失效)/);
  await view.startRun(draft.actions[1]); // The shared entry point must revalidate too.
  assert.match(view.error(), /选项.*(?:未加载|失效)/);
  assert.equal(view.persisted()[0].run.target, 'item');
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_status']);
  view.flushEffects();
  assert.equal(view.optionLoads().item.status, 'running');
  assert.equal(view.values().item, '');
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_status', 'miniapp_command_start']);
  await settle();
});

test('cancelling first auto-load does not loop; manual retry runs directly and caches an empty array', async t => {
  const draft = optionsApp(), block = draft.pages[0].blocks[1];
  const entry = saved({ draft, published: clone(draft) });
  let starts = 0, status = 'running';
  const view = mount(t, [entry], { invoke: async (command, { runId }) => {
    assert.ok(['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_cancel'].includes(command));
    if (command === 'miniapp_command_start') status = ++starts === 1 ? 'running' : 'finished';
    if (command === 'miniapp_command_cancel') status = 'cancelled';
    return { runId, status, stdout: '[]', output: '[]\nsource diagnostics', exitCode: 0, truncated: false };
  } });
  assert.equal(view.error(), '');
  view.choose(entry.id, true);
  view.flushEffects();
  assert.deepEqual(view.calls, ['miniapp_command_start']);
  await settle();
  await view.cancelRun();
  await settle();
  assert.equal(view.persisted()[0].run.status, 'cancelled');
  assert.equal(view.optionLoads().item.status, 'error');
  assert.equal(view.locked(), false);
  for (let i = 0; i < 3; i++) view.flushEffects();
  view.requestOptions(block);
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_cancel', 'miniapp_command_status']);
  view.openOptions(block); // Opening an unloaded select is the UI's explicit retry path.
  assert.equal(view.optionLoads().item.status, 'running');
  assert.equal(starts, 2);
  await settle();
  await view.pollRun();
  assert.deepEqual(view.optionLoads().item, { key: view.optionsKey(block), status: 'ready', options: [] });
  assert.equal(view.persisted()[0].run.status, 'finished');
  assert.equal(view.values().item, '');
  assert.deepEqual(view.availableOptions(block), []);
  view.flushEffects();
  view.openOptions(block);
  view.requestOptions(block);
  assert.equal(starts, 2, 'ready empty results must not be mistaken for unloaded data');
  assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_cancel', 'miniapp_command_status', 'miniapp_command_start', 'miniapp_command_status']);
});

test('invalid, truncated and failed data sources show errors, offer no fallback options and never retry or call AI', async t => {
  const cases = [
    ['invalid JSON', { stdout: 'diagnostic\n["fake"]', output: '["not-a-fallback"]' }, /JSON|数组|解析/],
    ['truncated JSON', { stdout: '["partial"]', truncated: true }, /截断/],
    ['nonzero exit', { status: 'error', stdout: '["ignore-me"]', output: 'source failed', exitCode: 7 }, /source failed[\s\S]*退出码：7/],
  ];
  for (const [name, patch, message] of cases) await t.test(name, async t => {
    const draft = optionsApp(), block = draft.pages[0].blocks[1];
    const view = mount(t, [saved({ draft })], { invoke: async (command, { runId }) => {
      assert.ok(['miniapp_command_start', 'miniapp_command_status'].includes(command));
      return { runId, status: 'finished', stdout: '[]', output: '', exitCode: 0, truncated: false, ...patch };
    } });
    assert.equal(view.error(), '');
    view.switchMode('try');
    view.flushEffects();
    assert.equal(view.optionLoads().item.status, 'running');
    await settle();
    await view.pollRun();
    assert.equal(view.optionLoads().item.status, 'error');
    assert.match(view.optionLoads().item.error, message);
    assert.deepEqual(view.optionLoads().item.options, []);
    assert.deepEqual(view.availableOptions(block), []);
    assert.equal(view.values().item, '');
    assert.equal(view.persisted()[0].run.status, 'error');
    assert.equal(view.persisted()[0].run.output, view.optionLoads().item.error);
    assert.equal(view.locked(), false);
    view.flushEffects();
    view.flushEffects();
    view.requestOptions(block);
    assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status']);
    view.requestOptions(block, true);
    assert.equal(view.optionLoads().item.status, 'running', 'explicit retry starts directly');
    await settle();
    await view.pollRun();
    assert.equal(view.optionLoads().item.status, 'error');
    view.flushEffects();
    assert.deepEqual(view.calls, ['miniapp_command_start', 'miniapp_command_status', 'miniapp_command_start', 'miniapp_command_status']);
  });
});
