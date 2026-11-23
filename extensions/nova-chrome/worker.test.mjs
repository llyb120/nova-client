import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import { webcrypto } from 'node:crypto';

const timers = new Set();
const setTimeout = (fn, ms, ...args) => {
  const timer = globalThis.setTimeout(fn, ms, ...args);
  timers.add(timer);
  return timer;
};
const clearTimeout = timer => {
  timers.delete(timer);
  globalThis.clearTimeout(timer);
};
const setInterval = (fn, ms, ...args) => {
  const timer = globalThis.setInterval(fn, ms, ...args);
  timers.add(timer);
  return timer;
};
const clearInterval = timer => {
  timers.delete(timer);
  globalThis.clearInterval(timer);
};
afterEach(async () => {
  await new Promise(resolve => setImmediate(resolve));
  for (const t of timers) {
    globalThis.clearTimeout(t);
    globalThis.clearInterval(t);
  }
  timers.clear();
});

const event = () => ({
  listeners: [],
  addListener(fn) { this.listeners.push(fn); },
  emit(...args) { for (const fn of this.listeners) fn(...args); },
});

function createHarness(overrides = {}) {
  const cdpCalls = [];
  let tab = { id: 11, windowId: 5, url: 'https://example.com', title: 'Example', active: true, status: 'complete', groupId: -1, incognito: false };
  let win = { id: 5, state: 'normal', incognito: false };
  const chrome = {
    downloads: { search: async () => [] },
    extension: { isAllowedIncognitoAccess: async () => true },
    storage: { session: { get: async () => ({}), set: async () => {} }, local: { get: async () => ({}), set: async () => {} } },
    tabs: {
      query: async () => [tab],
      get: async () => ({ ...tab }),
      update: async (_id, props) => { Object.assign(tab, props); return { ...tab }; },
      create: async props => ({ ...tab, id: 12, url: props.url || 'about:blank' }),
      remove: async () => {},
      group: async () => 1,
      ungroup: async () => {},
      onRemoved: event(),
      onReplaced: event(),
      onUpdated: event(),
      ...overrides.tabs,
    },
    tabGroups: { query: async () => [], update: async () => {} },
    windows: {
      get: async () => ({ ...win }),
      getAll: async () => [{ ...win }],
      update: async (_id, props) => { win = { ...win, ...props }; return { ...win }; },
      create: async props => ({ id: 6, incognito: !!props.incognito, tabs: [{ ...tab, id: 13, url: props.url }] }),
      ...overrides.windows,
    },
    debugger: {
      attach: async () => {},
      detach: async () => {},
      sendCommand: async (_target, method, params) => {
        cdpCalls.push([method, params]);
        if (overrides.onCdp) {
          const out = await overrides.onCdp(method, params, chrome);
          if (out !== undefined) return out;
        }
        if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: 'main-frame' } } };
        if (method === 'Page.createIsolatedWorld') return { executionContextId: 7 };
        if (method === 'Page.captureScreenshot') return { data: 'jpegdata' };
        if (method === 'Runtime.evaluate') {
          const expr = String(params?.expression || '');
          if (expr.includes('__novaBrowser.resolve(')) return { result: { value: { x: 240, y: 120, rect: [200, 100, 80, 40], name: 'Search', editable: true } } };
          if (expr.includes('__novaBrowser.observe(')) return { result: { value: { url: tab.url, title: tab.title, readyState: 'complete', viewport: [1280, 800, 1], scroll: [0, 0, 1280, 800], cursor: [240, 120], items: [], text: 'hello', observeMs: 3 } } };
          if (expr.includes('document.readyState')) return { result: { value: 'complete' } };
          return { result: { value: true } };
        }
        return {};
      },
      onEvent: event(),
      onDetach: event(),
    },
    webNavigation: { onCreatedNavigationTarget: event() },
    runtime: {
      id: 'ext-id',
      getManifest: () => ({ version: '0.2.0' }),
      getPlatformInfo: async () => ({ os: 'win' }),
      reload: () => {},
      onMessage: event(),
      onInstalled: event(),
      onStartup: event(),
    },
    alarms: { onAlarm: event(), create: async () => {} },
  };
  class DummyWS {
    constructor() { this.readyState = 3; }
    close() {}
  }
  const scope = {
    chrome,
    crypto: webcrypto,
    URL,
    AbortController,
    WebSocket: overrides.WebSocket || DummyWS,
    ...(overrides.fetch ? { fetch: overrides.fetch } : {}),
    navigator: { userAgent: 'Mozilla/5.0 Chrome/130.0.0.0' },
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
  };
  return { scope, chrome, cdpCalls, setTab: p => Object.assign(tab, p), setWin: p => Object.assign(win, p) };
}

test('worker executes pointer-driven click + type(focus, clear, submit) + JPEG view in one atomic command', async () => {
  const { scope, cdpCalls } = createHarness();
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const tag = tabs[0].tag;

  const res = await scope.__exec('act', {
    tabTag: tag,
    steps: [
      { op: 'type', id: 1, text: 'nova', clear: true, submit: true },
    ],
    viewOpts: { screenshot: true },
  });

  assert.equal(res.ok, true);
  assert.equal(res.completed, 1);
  assert.equal(res.view.screenshotB64, 'jpegdata');

  // Verify pointer moved -> clicked to focus -> Ctrl+A + Backspace -> insertText -> Enter
  const methods = cdpCalls.map(([m, p]) => m === 'Input.dispatchMouseEvent' ? `${m}:${p.type}` : m === 'Input.dispatchKeyEvent' ? `${m}:${p.type}:${p.key}` : m);
  assert.ok(methods.includes('Input.dispatchMouseEvent:mouseMoved'), 'must move mouse pointer');
  assert.ok(methods.includes('Input.dispatchMouseEvent:mousePressed'), 'type with target must click to focus');
  assert.ok(methods.includes('Input.dispatchMouseEvent:mouseReleased'), 'must release click');
  assert.ok(methods.includes('Input.insertText'), 'must insert text');
  assert.ok(methods.includes('Input.dispatchKeyEvent:keyDown:Enter'), 'submit=true must press Enter');
  assert.ok(cdpCalls.some(([m, p]) => m === 'Page.captureScreenshot' && p.format === 'jpeg'), 'captures fast JPEG');
});

test('worker records DevTools console/network and filters errors cleanly', async () => {
  const { scope, chrome } = createHarness({
    onCdp: async (method) => {
      if (method === 'Network.getResponseBody') return { body: '{"ok":true}', base64Encoded: false };
      return undefined;
    },
  });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const tag = tabs[0].tag;

  await scope.__exec('debug', { tabTag: tag, kind: 'all' });
  chrome.debugger.onEvent.emit({ tabId: 11 }, 'Runtime.consoleAPICalled', { type: 'error', args: [{ type: 'string', value: 'boom' }], timestamp: 1 });
  chrome.debugger.onEvent.emit({ tabId: 11 }, 'Network.requestWillBeSent', { requestId: 'r1', request: { method: 'GET', url: 'https://example.com/api' }, type: 'Fetch', wallTime: 100 });
  chrome.debugger.onEvent.emit({ tabId: 11 }, 'Network.responseReceived', { requestId: 'r1', response: { status: 500, statusText: 'Err', mimeType: 'application/json' } });

  const filtered = await scope.__exec('debug', { tabTag: tag, kind: 'all', level: 'error' });
  assert.equal(filtered.console.total, 1);
  assert.equal(filtered.console.items[0].text, 'boom');
  assert.equal(filtered.network.total, 1);
  assert.equal(filtered.network.items[0].status, 500);

  const body = await scope.__exec('debug', { tabTag: tag, kind: 'network', requestId: 'r1' });
  assert.equal(body.body, '{"ok":true}');
});

test('worker reveals minimized windows before screenshot', async () => {
  const calls = [];
  const { scope, setTab, setWin } = createHarness({
    windows: { update: async (id, p) => { calls.push(['win', id, p]); return { id, ...p }; } },
    tabs: { update: async (id, p) => { calls.push(['tab', id, p]); return { id, ...p }; } },
  });
  setWin({ state: 'minimized' });
  setTab({ active: false });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const view = await scope.__exec('view', { tabTag: tabs[0].tag, viewOpts: { screenshot: true } });
  assert.equal(view.screenshotB64, 'jpegdata');
  assert.equal(JSON.stringify(calls), JSON.stringify([['win', 5, { state: 'normal' }], ['tab', 11, { active: true }]]));
});

test('worker dismiss clicks safe blank outsidePoint and hides cursor cleanly on cursor_hide & screenshots', async () => {
  let overlayOpen = true;
  const { scope, cdpCalls } = createHarness({
    onCdp: async (method, params) => {
      if (method === 'Runtime.evaluate' && String(params?.expression || '').includes('__novaBrowser.overlayState()')) {
        const res = overlayOpen ? { open: true, kind: 'popover', outsidePoint: [420, 90] } : { open: false };
        return { result: { value: res } };
      }
      if (method === 'Input.dispatchMouseEvent' && params?.type === 'mouseReleased' && params.x === 420 && params.y === 90) {
        overlayOpen = false;
      }
      return undefined;
    },
  });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const tag = tabs[0].tag;

  const res = await scope.__exec('act', { tabTag: tag, steps: [{ op: 'dismiss' }], viewOpts: { screenshot: true } });
  assert.equal(res.ok, true);
  assert.equal(res.completed, 1);
  assert.equal(overlayOpen, false, 'dismiss must close overlay by clicking outsidePoint');
  assert.ok(cdpCalls.some(([m, p]) => m === 'Input.dispatchMouseEvent' && p.type === 'mousePressed' && p.x === 420 && p.y === 90));
  assert.ok(cdpCalls.some(([m, p]) => m === 'Runtime.evaluate' && String(p?.expression || '').includes('cursor?.show?.(')), 'cursor is shown continuously while session operates');
  assert.ok(!cdpCalls.some(([m, p]) => m === 'Runtime.evaluate' && String(p?.expression || '').includes('setShotHidden?.(true)')), 'cursor never blinks off during screenshots');

  await scope.__exec('cursor_hide', { tabTag: tag });
  assert.ok(cdpCalls.some(([m, p]) => m === 'Runtime.evaluate' && String(p?.expression || '').includes('cursor?.dismiss?.(false)')));
});

test('worker scroll targets SPA primary scroll container and verifies/falls back scroll delta', async () => {
  const { scope, cdpCalls } = createHarness({
    onCdp: async (method, params) => {
      const expr = String(params?.expression || '');
      if (method === 'Runtime.evaluate' && expr.includes('__novaBrowser.scrollTarget(')) {
        return { result: { value: { x: 897, y: 408, top: 0, left: 0, maxY: 1850 } } };
      }
      if (method === 'Runtime.evaluate' && expr.includes('__novaBrowser.verifyOrFallbackScroll(')) {
        return { result: { value: { beforeTop: 0, afterTop: 600, beforeLeft: 0, afterLeft: 0, maxY: 1850 } } };
      }
      return undefined;
    },
  });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const res = await scope.__exec('act', {
    tabTag: tabs[0].tag,
    steps: [{ op: 'scroll', dy: 600 }],
    viewOpts: { screenshot: false },
  });
  assert.equal(res.ok, true);
  assert.equal(res.actions[0], 'scrolled y: 0 -> 600/1850');
  assert.ok(cdpCalls.some(([m, p]) => m === 'Input.dispatchMouseEvent' && p.type === 'mouseWheel' && p.x === 897 && p.y === 408 && p.deltaY === 600));
});

test('worker promotes iframe overlay & scrollInfo and routes dismiss/scroll into child iframe', async () => {
  let iframeOverlayOpen = true;
  const { scope, cdpCalls } = createHarness({
    onCdp: async (method, params) => {
      if (method === 'Page.getFrameTree') {
        return { frameTree: { frame: { id: 'main-frame' }, childFrames: [{ frame: { id: 'child-frame' } }] } };
      }
      if (method === 'Page.createIsolatedWorld') {
        return { executionContextId: params.frameId === 'child-frame' ? 9 : 7 };
      }
      if (method === 'DOM.getFrameOwner') return { backendNodeId: 55 };
      if (method === 'DOM.resolveNode') return { object: { objectId: 'iframe-obj' } };
      if (method === 'Runtime.callFunctionOn') return { result: { value: [200, 100, 1, 1] } };
      if (method === 'Runtime.evaluate') {
        const expr = String(params?.expression || '');
        const isChild = params?.contextId === 9;
        if (expr.includes('__novaBrowser.observe(')) {
          if (!isChild) {
            return { result: { value: { url: 'https://example.com', title: 'Host', readyState: 'complete', viewport: [1280, 800, 1], scroll: [0, 0, 1280, 800], scrollInfo: { y: 0, maxY: 0, viewH: 800, totalH: 800, moreBelow: 0 }, hasIframes: true, items: [], text: '' } } };
          }
          return { result: { value: { url: 'https://app.example.com', title: 'App', readyState: 'complete', viewport: [900, 600, 1], scroll: [0, 0, 900, 1800], scrollInfo: { y: 0, maxY: 1200, viewH: 600, totalH: 1800, moreBelow: 1200, containerId: 2 }, overlay: { kind: 'popover', rect: [100, 80, 300, 200], hasConfirm: false, outsidePoint: [50, 40] }, items: [{ id: 1, role: 'checkbox', name: 'US', x: 150, y: 120, rect: [140, 110, 20, 20], inView: true, inOverlay: true }], text: 'inside' } } };
        }
        if (expr.includes('__novaBrowser.overlayState()')) {
          if (!isChild) return { result: { value: { open: false } } };
          return { result: { value: iframeOverlayOpen ? { open: true, outsidePoint: [50, 40] } : { open: false } } };
        }
        if (expr.includes('__novaBrowser.scrollTarget(') && isChild) {
          return { result: { value: { x: 400, y: 300, top: 0, left: 0, maxY: 1200 } } };
        }
        if (expr.includes('__novaBrowser.verifyOrFallbackScroll(') && isChild) {
          return { result: { value: { beforeTop: 0, afterTop: 500, beforeLeft: 0, afterLeft: 0, maxY: 1200 } } };
        }
      }
      if (method === 'Input.dispatchMouseEvent' && params?.type === 'mouseReleased' && params.x === 250 && params.y === 140) {
        iframeOverlayOpen = false;
      }
      return undefined;
    },
  });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__exec = executeCommand;', scope);
  const { tabs } = await scope.__exec('tabs', {});
  const tag = tabs[0].tag;

  const view = await scope.__exec('view', { tabTag: tag, viewOpts: { screenshot: false } });
  assert.equal(JSON.stringify(view.overlay.outsidePoint), JSON.stringify([250, 140]), 'iframe overlay outsidePoint transformed to top-level');
  assert.equal(view.scrollInfo.moreBelow, 1200, 'iframe scrollInfo promoted when top frame does not scroll');

  const act = await scope.__exec('act', { tabTag: tag, steps: [{ op: 'dismiss' }, { op: 'scroll', dy: 500 }], viewOpts: { screenshot: false } });
  assert.equal(act.ok, true);
  assert.equal(iframeOverlayOpen, false, 'dismiss clicked top-level transformed point (250,140) to close iframe overlay');
  assert.equal(act.actions[1], 'scrolled y: 0 -> 500/1200', 'scroll routed into child iframe');
  assert.ok(cdpCalls.some(([m, p]) => m === 'Input.dispatchMouseEvent' && p.type === 'mouseWheel' && p.x === 600 && p.y === 400 && p.deltaY === 500));
});

test('worker probes /health via fetch and only opens WebSocket to live Nova ports without duplicate probes', async () => {
  const probed = [];
  const wsUrls = [];
  class RecordingWS {
    constructor(url) {
      wsUrls.push(url);
      this.readyState = 1;
    }
    close() {}
  }
  const { scope } = createHarness({
    WebSocket: RecordingWS,
    fetch: async (url) => {
      probed.push(url);
      if (url === 'http://127.0.0.1:47653/health') {
        return { ok: true, json: async () => ({ ok: true, app: 'nova', version: '0.2.5' }) };
      }
      if (url === 'http://127.0.0.1:47654/health') {
        return { ok: true, json: async () => ({ ok: true, app: 'other-service' }) };
      }
      throw new TypeError('Failed to fetch');
    },
  });
  const src = await readFile(new URL('./worker.js', import.meta.url), 'utf8');
  runInNewContext(src + '\nglobalThis.__discover = discover;', scope);
  // Trigger a second concurrent discover while probes are in flight.
  scope.__discover();
  await new Promise(resolve => globalThis.setTimeout(resolve, 30));

  assert.equal(probed.length, 10, 'each port in 47653-47662 probed once while in flight');
  assert.equal(wsUrls.length, 1, 'only the live Nova port opens a WebSocket');
  assert.ok(wsUrls[0].startsWith('ws://127.0.0.1:47653/ws?clientId='));
});
