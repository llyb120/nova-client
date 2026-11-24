// Nova Chrome Extension v0.2.0 — Persistent WebSocket + Atomic Pointer-Driven CDP Executor.
// Receives page_runtime.js from Nova on WS hello (single source of truth).
let model, loading;
const sockets = new Map(); // origin -> { ws, sendReq, ready }
const reconnects = new Map();
const connecting = new Set();
let pageRuntime = '';

const attached = new Set();
const tabState = new Map(); // tabId -> { mainFrameId, contexts: Map, loading, loadWaiters: [], cursor, childFrames: Map, frameRoutes: Map, devtools }
const navigationOpeners = new Map();
const idleTimers = new Map();
let tabMutationSeq = 0;
const IDLE_DETACH_MS = 300000;
const DEVTOOLS_LIMIT = 500;

const ready = () => (loading ??= chrome.storage.session.get('model').then(({ model: saved }) => {
  model = saved ?? { clientId: crypto.randomUUID(), next: 1, tabs: {} };
  return model;
}));
const save = () => chrome.storage.session.set({ model });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const permittedUrl = url => /^https?:\/\//i.test(url || '') || url === 'about:blank';
const clip = (s, max = 2000) => { s = String(s ?? ''); return s.length > max ? s.slice(0, max) + `…(+${s.length - max})` : s; };

function getState(tabId) {
  let st = tabState.get(tabId);
  if (!st) {
    st = {
      mainFrameId: '',
      contexts: new Map(),
      loading: false,
      loadWaiters: [],
      cursor: null,
      cursorActive: false,
      childFrames: new Map(), // targetId -> { sessionId, targetInfo }
      frameRoutes: new Map(), // elementId -> { frameId, sessionId, chain }
      overlayFrame: null,
      scrollFrame: null,
      devtools: null,
    };
    tabState.set(tabId, st);
  }
  return st;
}

function notifyLoad(tabId) {
  const st = tabState.get(tabId);
  if (!st) return;
  const waiters = st.loadWaiters.splice(0);
  for (const fn of waiters) fn();
}

async function inventory() {
  await ready();
  const tabs = await chrome.tabs.query({});
  const ids = new Set(tabs.map(t => String(t.id)));
  let dirty = false;
  for (const id of Object.keys(model.tabs)) {
    if (!ids.has(id)) {
      delete model.tabs[id];
      dirty = true;
    }
  }
  for (const tab of tabs) {
    if (!model.tabs[tab.id]) {
      model.tabs[tab.id] = { tag: `C${model.next++}-${model.clientId.replaceAll('-', '').slice(0, 8)}` };
      dirty = true;
    }
  }
  if (dirty) await save();
  return tabs.map(tab => {
    const openerId = navigationOpeners.get(tab.id) ?? tab.openerTabId;
    const url = tab.url || tab.pendingUrl || '';
    return {
      tag: model.tabs[tab.id].tag,
      title: tab.title || '',
      url,
      active: Boolean(tab.active),
      incognito: Boolean(tab.incognito),
      loading: tab.status !== 'complete' || Boolean(tab.pendingUrl),
      controllable: permittedUrl(url),
      openerTag: model.tabs[openerId]?.tag ?? null,
    };
  });
}

async function resolveTag(tag) {
  await ready();
  if (typeof tag !== 'string' || !tag) throw Error('缺少 tabTag');
  const entry = Object.entries(model.tabs).find(([, v]) => v.tag === tag);
  if (!entry) throw Error(`标签 ${tag} 不存在或已关闭`);
  return chrome.tabs.get(Number(entry[0]));
}

function checkUrl(value) {
  const url = new URL(value);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
    throw Error('只允许不含凭据的 HTTP(S) 地址');
  }
  return url.href;
}

async function cdp(tabId, method, params = {}, sessionId = null, timeoutMs = 10000) {
  const target = sessionId ? { tabId, sessionId } : { tabId };
  let timer;
  try {
    return await Promise.race([
      chrome.debugger.sendCommand(target, method, params),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(Error(`CDP ${method} 超时`)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function forget(tabId) {
  attached.delete(tabId);
  tabState.delete(tabId);
  clearTimeout(idleTimers.get(tabId));
  idleTimers.delete(tabId);
}

async function dismissTabCursor(tabId, immediate = false) {
  const st = tabState.get(tabId);
  if (!st) return;
  st.cursorActive = false;
  if (!st.mainFrameId || !attached.has(tabId)) return;
  await evalFrame(tabId, null, st.mainFrameId, `__novaBrowser?.cursor?.dismiss?.(${immediate ? 'true' : 'false'})`).catch(() => {});
}

async function detach(tabId) {
  const was = attached.has(tabId);
  if (was) await dismissTabCursor(tabId, true);
  forget(tabId);
  if (was) await chrome.debugger.detach({ tabId }).catch(() => {});
}

async function attach(tab) {
  clearTimeout(idleTimers.get(tab.id));
  idleTimers.set(tab.id, setTimeout(() => {
    if (!tabState.get(tab.id)?.devtools) void detach(tab.id);
  }, IDLE_DETACH_MS));
  if (attached.has(tab.id)) return getState(tab.id);

  await chrome.debugger.attach({ tabId: tab.id }, '1.3');
  attached.add(tab.id);
  const st = getState(tab.id);

  await cdp(tab.id, 'Page.enable', {});
  if (pageRuntime) {
    await cdp(tab.id, 'Page.addScriptToEvaluateOnNewDocument', { source: pageRuntime, worldName: 'nova' }).catch(() => {});
  }
  const tree = await cdp(tab.id, 'Page.getFrameTree', {});
  st.mainFrameId = tree?.frameTree?.frame?.id || '';

  // Bounded fast settle for Chrome's debugging infobar resize (max 220ms instead of old 800ms!).
  const resizeExpr = `new Promise(done=>{let t;const fin=()=>{clearTimeout(t);clearTimeout(c);removeEventListener('resize',onR);done(1)};` +
    `const onR=()=>{clearTimeout(t);t=setTimeout(fin,80)};const c=setTimeout(fin,220);addEventListener('resize',onR);t=setTimeout(fin,140)})`;
  await cdp(tab.id, 'Runtime.evaluate', { expression: resizeExpr, awaitPromise: true, silent: true }, null, 500).catch(() => {});
  return st;
}

async function ensureWorld(tabId, cdpSessionId, frameId) {
  const st = getState(tabId);
  const key = `${cdpSessionId || ''}:${frameId}`;
  if (st.contexts.has(key)) return st.contexts.get(key);
  const created = await cdp(tabId, 'Page.createIsolatedWorld', { frameId, worldName: 'nova' }, cdpSessionId);
  const ctx = created?.executionContextId;
  if (!ctx) throw Error('无法创建页面隔离上下文');
  if (pageRuntime) {
    await cdp(tabId, 'Runtime.evaluate', { expression: pageRuntime, contextId: ctx, returnByValue: false }, cdpSessionId);
  }
  if (!cdpSessionId && frameId === st.mainFrameId && (st.cursorActive || st.cursor)) {
    const expr = st.cursorActive
      ? (st.cursor ? `__novaBrowser.cursor.show(${st.cursor[0]},${st.cursor[1]})` : '__novaBrowser.cursor.show()')
      : `__novaBrowser.cursor.sync(${st.cursor[0]},${st.cursor[1]})`;
    await cdp(tabId, 'Runtime.evaluate', { expression: expr, contextId: ctx }, null).catch(() => {});
  }
  st.contexts.set(key, ctx);
  return ctx;
}

async function evalFrame(tabId, cdpSessionId, frameId, expression) {
  const st = getState(tabId);
  const key = `${cdpSessionId || ''}:${frameId}`;
  for (let attempt = 0; attempt < 2; attempt++) {
    const ctx = await ensureWorld(tabId, cdpSessionId, frameId);
    try {
      const res = await cdp(tabId, 'Runtime.evaluate', {
        expression,
        contextId: ctx,
        returnByValue: true,
        awaitPromise: true,
      }, cdpSessionId);
      if (res?.exceptionDetails) {
        const desc = res.exceptionDetails.exception?.description || res.exceptionDetails.text || '页面脚本错误';
        if (attempt === 0 && (desc.includes('__novaBrowser') || desc.includes('Cannot find context'))) {
          st.contexts.delete(key);
          continue;
        }
        throw Error(desc);
      }
      return res?.result?.value;
    } catch (err) {
      const msg = String(err?.message || err);
      if (attempt === 0 && (msg.includes('Cannot find context') || msg.includes('Execution context was destroyed'))) {
        st.contexts.delete(key);
        continue;
      }
      throw err;
    }
  }
  throw Error('页面上下文重建失败');
}

async function waitForLoad(tabId, budgetMs = 3500) {
  const st = getState(tabId);
  const deadline = Date.now() + budgetMs;
  while (Date.now() < deadline) {
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab) return;
    const tabLoading = tab.status !== 'complete' || Boolean(tab.pendingUrl);
    if (!st.loading && !tabLoading) {
      const rs = await cdp(tabId, 'Runtime.evaluate', { expression: 'document.readyState', returnByValue: true, silent: true }, null, 1500)
        .then(r => r?.result?.value).catch(() => 'complete');
      if (rs === 'complete' || rs === 'interactive') break;
    }
    const left = Math.max(20, Math.min(120, deadline - Date.now()));
    await new Promise(resolve => {
      const timer = setTimeout(resolve, left);
      st.loadWaiters.push(() => { clearTimeout(timer); resolve(); });
    });
  }
  if (st.mainFrameId) {
    await evalFrame(tabId, null, st.mainFrameId, '__novaBrowser.waitQuiet(100, 260)').catch(() => {});
  }
}

async function iframeTransform(tabId, chain) {
  let ox = 0, oy = 0, sx = 1, sy = 1;
  for (const { childFrameId, parentSessionId } of chain) {
    const owner = await cdp(tabId, 'DOM.getFrameOwner', { frameId: childFrameId }, parentSessionId);
    const resolved = await cdp(tabId, 'DOM.resolveNode', { backendNodeId: owner.backendNodeId }, parentSessionId);
    const objectId = resolved?.object?.objectId;
    if (!objectId) throw Error('框架元素不可访问');
    try {
      const rectRes = await cdp(tabId, 'Runtime.callFunctionOn', {
        objectId,
        returnByValue: true,
        functionDeclaration: 'function(){const r=this.getBoundingClientRect();const sx=this.offsetWidth?r.width/this.offsetWidth:1;const sy=this.offsetHeight?r.height/this.offsetHeight:1;return [r.x+this.clientLeft*sx,r.y+this.clientTop*sy,sx,sy];}',
      }, parentSessionId);
      const [fx, fy, fsx, fsy] = rectRes?.result?.value || [0, 0, 1, 1];
      ox = fx + ox * fsx;
      oy = fy + oy * fsy;
      sx *= fsx;
      sy *= fsy;
    } finally {
      await cdp(tabId, 'Runtime.releaseObject', { objectId }, parentSessionId).catch(() => {});
    }
  }
  return [ox, oy, sx, sy];
}

function collectChildFrames(node, cdpSid, parentChain, out) {
  for (const child of node?.childFrames || []) {
    const fid = child?.frame?.id;
    if (!fid) continue;
    const chain = [{ childFrameId: fid, parentSessionId: cdpSid }, ...parentChain];
    out.push({ frameId: fid, sessionId: cdpSid, chain });
    collectChildFrames(child, cdpSid, chain, out);
  }
}

async function resolveElement(tabId, id, actionKind = '') {
  const st = getState(tabId);
  const route = st.frameRoutes.get(Number(id));
  const expr = `__novaBrowser.resolve(${Number(id)},${JSON.stringify(actionKind)})`;
  if (route) {
    const val = await evalFrame(tabId, route.sessionId, route.frameId, expr);
    const [ox, oy, sx, sy] = await iframeTransform(tabId, route.chain);
    val.x = ox + val.x * sx;
    val.y = oy + val.y * sy;
    if (Array.isArray(val.rect) && val.rect.length === 4) {
      val.rect = [ox + val.rect[0] * sx, oy + val.rect[1] * sy, val.rect[2] * sx, val.rect[3] * sy];
    }
    return val;
  }
  return evalFrame(tabId, null, st.mainFrameId, expr);
}

async function ensureVisibleForShot(tab) {
  const fresh = await chrome.tabs.get(tab.id);
  const win = await chrome.windows.get(fresh.windowId);
  if (win.state === 'minimized') await chrome.windows.update(fresh.windowId, { state: 'normal' });
  if (!fresh.active) await chrome.tabs.update(fresh.id, { active: true });
}

async function doView(tab, opts = {}) {
  const st = await attach(tab);
  st.cursorActive = true;
  if (!st.mainFrameId) {
    const tree = await cdp(tab.id, 'Page.getFrameTree', {});
    st.mainFrameId = tree?.frameTree?.frame?.id || '';
  }
  await evalFrame(tab.id, null, st.mainFrameId, st.cursor
    ? `__novaBrowser?.cursor?.show?.(${st.cursor[0]},${st.cursor[1]})`
    : '__novaBrowser?.cursor?.show?.()').catch(() => {});

  let zoomCss = opts.zoomRect || null;
  if (opts.zoomId !== undefined && opts.zoomId !== null) {
    try {
      const r = await resolveElement(tab.id, opts.zoomId);
      if (Array.isArray(r?.rect) && r.rect.length === 4) zoomCss = r.rect;
    } catch {}
  }

  const obsArg = JSON.stringify({
    scope: opts.scope || 'viewport',
    query: opts.query || '',
    maxItems: opts.maxItems || 100,
    maxTextChars: opts.maxTextChars ?? 3000,
    idOffset: 0,
  });
  const mainVal = await evalFrame(tab.id, null, st.mainFrameId, `__novaBrowser.observe(${obsArg})`);
  const items = Array.isArray(mainVal?.items) ? [...mainVal.items] : [];
  let text = mainVal?.text || '';
  let overlay = mainVal?.overlay ?? null;
  let scrollInfo = mainVal?.scrollInfo ?? null;
  st.frameRoutes.clear();
  st.overlayFrame = null;
  st.scrollFrame = null;

  if (mainVal?.hasIframes) {
    await cdp(tab.id, 'Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
    const tree = await cdp(tab.id, 'Page.getFrameTree', {}).catch(() => null);
    const children = [];
    collectChildFrames(tree?.frameTree, null, [], children);
    for (const child of st.childFrames.values()) {
      const ctree = await cdp(tab.id, 'Page.getFrameTree', {}, child.sessionId).catch(() => null);
      const fid = ctree?.frameTree?.frame?.id;
      if (fid && !children.some(c => c.frameId === fid)) {
        children.push({ frameId: fid, sessionId: child.sessionId, chain: [{ childFrameId: fid, parentSessionId: null }] });
      }
    }
    let offset = items.reduce((m, it) => Math.max(m, Number(it.id) || 0), 0);
    for (const child of children.slice(0, 6)) {
      try {
        const [ox, oy, sx, sy] = await iframeTransform(tab.id, child.chain);
        const fArg = JSON.stringify({
          scope: opts.scope || 'viewport',
          query: opts.query || '',
          maxItems: 40,
          maxTextChars: 1000,
          idOffset: offset,
        });
        const fval = await evalFrame(tab.id, child.sessionId, child.frameId, `__novaBrowser.observe(${fArg})`);
        for (const it of fval?.items || []) {
          offset = Math.max(offset, Number(it.id) || 0);
          it.x = Math.round(ox + it.x * sx);
          it.y = Math.round(oy + it.y * sy);
          if (Array.isArray(it.rect) && it.rect.length === 4) {
            it.rect = [
              Math.round(ox + it.rect[0] * sx),
              Math.round(oy + it.rect[1] * sy),
              Math.round(it.rect[2] * sx),
              Math.round(it.rect[3] * sy),
            ];
          }
          st.frameRoutes.set(Number(it.id), child);
          items.push(it);
        }
        // Promote active iframe overlay (e.g. dropdown/modal inside an embedded app iframe) to top-level view
        if (!overlay && fval?.overlay) {
          const fov = { ...fval.overlay };
          if (Array.isArray(fov.rect) && fov.rect.length === 4) {
            fov.rect = [Math.round(ox + fov.rect[0] * sx), Math.round(oy + fov.rect[1] * sy), Math.round(fov.rect[2] * sx), Math.round(fov.rect[3] * sy)];
          }
          if (Array.isArray(fov.outsidePoint) && fov.outsidePoint.length === 2) {
            fov.outsidePoint = [Math.round(ox + fov.outsidePoint[0] * sx), Math.round(oy + fov.outsidePoint[1] * sy)];
          }
          overlay = fov;
          st.overlayFrame = child;
        }
        // Promote large scrollable iframe as primary scrollInfo when top frame itself doesn't scroll
        const fvp = fval?.viewport || [0, 0];
        const fMore = Number(fval?.scrollInfo?.moreBelow || 0);
        const curMore = Number(scrollInfo?.moreBelow || 0);
        if (curMore <= 80 && fMore > 80 && fvp[0] * sx >= 300 && fvp[1] * sy >= 200) {
          if (!st.scrollFrame || fMore > curMore) {
            scrollInfo = fval.scrollInfo;
            st.scrollFrame = child;
          }
        }
        if (fval?.text) text = text ? `${text}\n${fval.text}` : fval.text;
      } catch {}
    }
  }

  let screenshotB64 = null, screenshotError = null;
  if (opts.screenshot !== false) {
    try {
      await ensureVisibleForShot(tab);
      const shot = await cdp(tab.id, 'Page.captureScreenshot', { format: 'jpeg', quality: 85, fromSurface: true });
      screenshotB64 = shot?.data || null;
    } catch (err) {
      screenshotError = String(err?.message || err);
    }
  }

  return {
    url: mainVal?.url || tab.url || '',
    title: mainVal?.title || tab.title || '',
    readyState: mainVal?.readyState || 'complete',
    viewport: mainVal?.viewport || [1280, 800, 1],
    scroll: mainVal?.scroll || [0, 0, 1280, 800],
    scrollInfo,
    cursor: mainVal?.cursor || st.cursor,
    focused: mainVal?.focused ?? null,
    overlay,
    items,
    text,
    observeMs: mainVal?.observeMs || 0,
    screenshotB64,
    screenshotError,
    zoomCss,
  };
}

function parseKeySpec(name) {
  const parts = String(name || '').split('+').map(s => s.trim());
  const base = parts.pop();
  if (!base) return null;
  let modifiers = 0;
  for (const m of parts) {
    if (m === 'Alt' || m === 'Option') modifiers |= 1;
    else if (m === 'Control' || m === 'Ctrl') modifiers |= 2;
    else if (m === 'Meta' || m === 'Cmd' || m === 'Command') modifiers |= 4;
    else if (m === 'Shift') modifiers |= 8;
    else return null;
  }
  const named = {
    Enter: ['Enter', 'Enter', 13], Return: ['Enter', 'Enter', 13],
    Tab: ['Tab', 'Tab', 9], Escape: ['Escape', 'Escape', 27], Esc: ['Escape', 'Escape', 27],
    Backspace: ['Backspace', 'Backspace', 8], Delete: ['Delete', 'Delete', 46], Del: ['Delete', 'Delete', 46],
    Home: ['Home', 'Home', 36], End: ['End', 'End', 35],
    PageUp: ['PageUp', 'PageUp', 33], PageDown: ['PageDown', 'PageDown', 34],
    ArrowLeft: ['ArrowLeft', 'ArrowLeft', 37], Left: ['ArrowLeft', 'ArrowLeft', 37],
    ArrowUp: ['ArrowUp', 'ArrowUp', 38], Up: ['ArrowUp', 'ArrowUp', 38],
    ArrowRight: ['ArrowRight', 'ArrowRight', 39], Right: ['ArrowRight', 'ArrowRight', 39],
    ArrowDown: ['ArrowDown', 'ArrowDown', 40], Down: ['ArrowDown', 'ArrowDown', 40],
    Space: [' ', 'Space', 32], ' ': [' ', 'Space', 32],
  };
  let spec = named[base];
  if (!spec && /^F([1-9]|1[0-2])$/.test(base)) {
    const n = Number(base.slice(1));
    spec = [base, base, 111 + n];
  }
  if (!spec && base.length === 1 && /^[a-zA-Z0-9]$/.test(base) && (modifiers & 7)) {
    const up = base.toUpperCase();
    const key = (modifiers & 8) ? up : base.toLowerCase();
    const code = /^\d$/.test(up) ? `Digit${up}` : `Key${up}`;
    spec = [key, code, up.charCodeAt(0)];
  }
  if (!spec) return null;
  const [key, code, vk] = spec;
  let command = null;
  if (modifiers === 2 || modifiers === 4) {
    if (vk === 65) command = 'selectAll';
    else if (vk === 67) command = 'copy';
    else if (vk === 88) command = 'cut';
    else if (vk === 86) command = 'paste';
  }
  return { key, code, vk, modifiers, command };
}

async function moveCursor(tabId, x, y, durationMs) {
  const st = getState(tabId);
  st.cursorActive = true;
  await evalFrame(tabId, null, st.mainFrameId, `__novaBrowser.cursor.moveTo(${x.toFixed(1)},${y.toFixed(1)},${durationMs})`).catch(() => {});
  st.cursor = [x, y];
  await cdp(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
}

async function clickPoint(tabId, x, y, button = 'left', clicks = 1) {
  const st = getState(tabId);
  await evalFrame(tabId, null, st.mainFrameId, `__novaBrowser.cursor.click(${x.toFixed(1)},${y.toFixed(1)})`).catch(() => {});
  const count = Math.max(1, Math.min(2, clicks));
  for (let n = 1; n <= count; n++) {
    await cdp(tabId, 'Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button, clickCount: n });
    await cdp(tabId, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button, clickCount: n });
  }
}

async function pressKey(tabId, keyName, showBadge = true) {
  const spec = parseKeySpec(keyName);
  if (!spec) throw Error(`不支持的按键：${keyName}`);
  const st = getState(tabId);
  if (st.mainFrameId) {
    const expr = showBadge
      ? `__novaBrowser.cursor.badge(${JSON.stringify(keyName)})`
      : `__novaBrowser.cursor.markInput(260)`;
    await evalFrame(tabId, null, st.mainFrameId, expr).catch(() => {});
  }
  const down = { type: 'keyDown', key: spec.key, code: spec.code, windowsVirtualKeyCode: spec.vk, modifiers: spec.modifiers };
  if (spec.command) down.commands = [spec.command];
  const up = { type: 'keyUp', key: spec.key, code: spec.code, windowsVirtualKeyCode: spec.vk, modifiers: spec.modifiers };
  await cdp(tabId, 'Input.dispatchKeyEvent', down);
  await cdp(tabId, 'Input.dispatchKeyEvent', up);
}

async function doAct(tab, args) {
  const started = Date.now();
  const st = await attach(tab);
  const steps = Array.isArray(args.steps) ? args.steps : [];
  const beforeTabs = await inventory();
  const beforeTags = new Set(beforeTabs.map(t => t.tag));
  let seenMutationSeq = tabMutationSeq;
  const sourceTag = args.tabTag;
  const isMac = navigator.userAgent.includes('Macintosh');
  const glideMs = steps.length > 2 ? 105 : 155;

  const actions = [];
  let completed = 0;
  let error = null;
  let mayNavigate = false;

  for (const step of steps) {
    try {
      if (step.op === 'click') {
        mayNavigate = true;
        let x = step.x, y = step.y, desc = 'target';
        if (step.id !== undefined && step.id !== null) {
          const r = await resolveElement(tab.id, step.id, 'click');
          x = r.x; y = r.y;
          desc = `[${step.id}] "${r.name || ''}"`;
        }
        await moveCursor(tab.id, x, y, glideMs);
        const btn = step.button || 'left';
        const cnt = step.clicks || 1;
        await clickPoint(tab.id, x, y, btn, cnt);
        actions.push(`clicked ${desc}`);
      } else if (step.op === 'type') {
        let targetDesc = 'focused';
        let isSelect = false;
        const route = (step.id !== undefined && step.id !== null) ? st.frameRoutes.get(Number(step.id)) : null;
        const targetSid = route ? route.sessionId : null;
        const targetFid = route ? route.frameId : st.mainFrameId;
        if (step.id !== undefined && step.id !== null) {
          const r = await resolveElement(tab.id, step.id, 'type');
          isSelect = r?.tag === 'select';
          await moveCursor(tab.id, r.x, r.y, glideMs);
          if (!isSelect) {
            await clickPoint(tab.id, r.x, r.y, 'left', 1);
            await sleep(22);
          }
          targetDesc = `[${step.id}]`;
        } else if (typeof step.x === 'number' && typeof step.y === 'number') {
          await moveCursor(tab.id, step.x, step.y, glideMs);
          await clickPoint(tab.id, step.x, step.y, 'left', 1);
          await sleep(22);
          targetDesc = 'target';
        }
        if (isSelect) {
          const sel = await evalFrame(tab.id, targetSid, targetFid, `__novaBrowser.selectOption(${Number(step.id)},${JSON.stringify(step.text || '')})`);
          actions.push(`selected "${sel?.selected || step.text || ''}" in ${targetDesc}`);
        } else {
          if (step.clear !== false) {
            await pressKey(tab.id, isMac ? 'Meta+A' : 'Ctrl+A', false);
            await pressKey(tab.id, 'Backspace', false);
            await evalFrame(tab.id, targetSid, targetFid, '__novaBrowser.ensureCleared?.()').catch(() => {});
          }
          if (step.text) {
            await cdp(tab.id, 'Input.insertText', { text: step.text });
            await evalFrame(tab.id, null, st.mainFrameId, '__novaBrowser.cursor.markInput()').catch(() => {});
          }
          const readVal = await evalFrame(tab.id, targetSid, targetFid, '__novaBrowser.readTypedValue?.()').catch(() => null);
          if (step.submit === true) {
            mayNavigate = true;
            await sleep(20);
            await pressKey(tab.id, 'Enter', true);
          }
          const valSuffix = (typeof readVal === 'string' && readVal && step.submit !== true) ? ` (val="${readVal}")` : '';
          actions.push(`typed "${String(step.text || '').slice(0, 40)}" into ${targetDesc}${valSuffix}${step.submit ? ' + Enter' : ''}`);
        }
      } else if (step.op === 'hover') {
        let x = step.x, y = step.y, desc = 'target';
        if (step.id !== undefined && step.id !== null) {
          const r = await resolveElement(tab.id, step.id);
          x = r.x; y = r.y; desc = `[${step.id}]`;
        }
        await moveCursor(tab.id, x, y, glideMs);
        actions.push(`hovered ${desc}`);
      } else if (step.op === 'drag') {
        const dur = Math.max(80, Math.min(1500, step.durationMs || 240));
        const segs = Math.max(6, Math.min(28, Math.round(dur / 16)));
        await moveCursor(tab.id, step.x1, step.y1, glideMs);
        await evalFrame(tab.id, null, st.mainFrameId, `__novaBrowser.cursor.drag('start',${step.x1.toFixed(1)},${step.y1.toFixed(1)})`).catch(() => {});
        await cdp(tab.id, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: step.x1, y: step.y1, button: 'left', buttons: 1, clickCount: 1 });
        for (let i = 1; i <= segs; i++) {
          const t = i / segs;
          const ease = t * t * (3 - 2 * t);
          const nx = step.x1 + (step.x2 - step.x1) * ease;
          const ny = step.y1 + (step.y2 - step.y1) * ease;
          await evalFrame(tab.id, null, st.mainFrameId, `__novaBrowser.cursor.drag('move',${nx.toFixed(1)},${ny.toFixed(1)})`).catch(() => {});
          await cdp(tab.id, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x: nx, y: ny, button: 'left', buttons: 1 });
          await sleep(Math.round(dur / segs));
        }
        await cdp(tab.id, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x: step.x2, y: step.y2, button: 'left', clickCount: 1 });
        await evalFrame(tab.id, null, st.mainFrameId, `__novaBrowser.cursor.drag('end',${step.x2.toFixed(1)},${step.y2.toFixed(1)})`).catch(() => {});
        st.cursor = [step.x2, step.y2];
        actions.push('dragged');
      } else if (step.op === 'scroll') {
        const dx = Number(step.dx) || 0;
        const dy = Number(step.dy) || 0;
        const targetId = step.id !== undefined && step.id !== null ? Number(step.id) : null;
        const route = targetId !== null
          ? st.frameRoutes.get(targetId) || null
          : (typeof step.x !== 'number' && typeof step.y !== 'number' ? st.scrollFrame : null);
        const targetSid = route ? route.sessionId : null;
        const targetFid = route ? route.frameId : st.mainFrameId;
        const info = await evalFrame(tab.id, targetSid, targetFid, `__novaBrowser.scrollTarget(${targetId === null ? 'null' : targetId})`).catch(() => null);
        let x = typeof step.x === 'number' ? step.x : (info?.x ?? 640);
        let y = typeof step.y === 'number' ? step.y : (info?.y ?? 400);
        if (route && info && typeof step.x !== 'number' && typeof step.y !== 'number') {
          const [ox, oy, sx, sy] = await iframeTransform(tab.id, route.chain).catch(() => [0, 0, 1, 1]);
          x = ox + x * sx;
          y = oy + y * sy;
        } else if (targetId !== null && !info) {
          const r = await resolveElement(tab.id, targetId);
          x = r.x; y = r.y;
        }
        const beforeTop = Number(info?.top ?? 0);
        const beforeLeft = Number(info?.left ?? 0);
        await moveCursor(tab.id, x, y, 80);
        await cdp(tab.id, 'Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: dx, deltaY: dy });
        await sleep(60);
        const after = await evalFrame(tab.id, targetSid, targetFid,
          `__novaBrowser.verifyOrFallbackScroll(${targetId === null ? 'null' : targetId},${dx},${dy},${beforeTop},${beforeLeft})`).catch(() => null);
        if (after && Number.isFinite(after.afterTop)) {
          actions.push(`scrolled y: ${after.beforeTop} -> ${after.afterTop}/${after.maxY}`);
        } else {
          actions.push(`scrolled dx=${dx} dy=${dy}`);
        }
      } else if (step.op === 'press') {
        if (String(step.key || '').toLowerCase() === 'enter') mayNavigate = true;
        await pressKey(tab.id, step.key, true);
        actions.push(`pressed ${step.key}`);
      } else if (step.op === 'dismiss') {
        mayNavigate = true;
        let ovRoute = null;
        let ov = await evalFrame(tab.id, null, st.mainFrameId, '__novaBrowser.overlayState()').catch(() => ({ open: false }));
        if (!ov?.open && st.overlayFrame) {
          const fov = await evalFrame(tab.id, st.overlayFrame.sessionId, st.overlayFrame.frameId, '__novaBrowser.overlayState()').catch(() => null);
          if (fov?.open) {
            ovRoute = st.overlayFrame;
            const [ox, oy, sx, sy] = await iframeTransform(tab.id, ovRoute.chain).catch(() => [0, 0, 1, 1]);
            ov = { ...fov };
            if (Array.isArray(ov.outsidePoint) && ov.outsidePoint.length === 2) {
              ov.outsidePoint = [ox + ov.outsidePoint[0] * sx, oy + ov.outsidePoint[1] * sy];
            }
          }
        }
        const pt = (ov?.open && Array.isArray(ov.outsidePoint) && ov.outsidePoint.length === 2)
          ? ov.outsidePoint
          : (Array.isArray(ov?.safeBlankPoint) && ov.safeBlankPoint.length === 2 ? ov.safeBlankPoint : null);
        if (pt) {
          const [ox, oy] = pt;
          await moveCursor(tab.id, ox, oy, glideMs);
          await clickPoint(tab.id, ox, oy, 'left', 1);
          await sleep(140);
          const checkSid = ovRoute ? ovRoute.sessionId : null;
          const checkFid = ovRoute ? ovRoute.frameId : st.mainFrameId;
          const after = await evalFrame(tab.id, checkSid, checkFid, '__novaBrowser.overlayState()').catch(() => ({ open: false }));
          if (ov?.open && after?.open) {
            await pressKey(tab.id, 'Escape', true);
            await sleep(80);
            actions.push('dismissed overlay (clicked blank space + Escape)');
          } else {
            actions.push('dismissed overlay by clicking blank space');
          }
        } else {
          await pressKey(tab.id, 'Escape', true);
          await sleep(80);
          actions.push('pressed Escape to dismiss overlay');
        }
      } else if (step.op === 'wait') {
        await evalFrame(tab.id, null, st.mainFrameId, '__novaBrowser?.cursor?.keepAlive?.()').catch(() => {});
        await sleep(Math.min(10000, Math.max(0, Number(step.ms) || 250)));
        actions.push(`waited ${step.ms || 250}ms`);
      } else if (step.op === 'upload') {
        const ctx = await ensureWorld(tab.id, null, st.mainFrameId);
        const expr = `(()=>{const ns=document.querySelectorAll(${JSON.stringify(step.selector)});if(ns.length!==1)throw Error('selector必须唯一匹配file input');const el=ns[0];if(!(el instanceof HTMLInputElement)||el.type!=='file'||el.disabled)throw Error('目标不是可用的file input');return el;})()`;
        const rep = await cdp(tab.id, 'Runtime.evaluate', { expression: expr, contextId: ctx, returnByValue: false });
        if (rep?.exceptionDetails) throw Error(rep.exceptionDetails.exception?.description || '上传校验失败');
        const objectId = rep?.result?.objectId;
        if (!objectId) throw Error('未取得 file input');
        try {
          await cdp(tab.id, 'DOM.setFileInputFiles', { objectId, files: step.files });
        } finally {
          await cdp(tab.id, 'Runtime.releaseObject', { objectId }).catch(() => {});
        }
        actions.push(`uploaded ${step.files.length} file(s)`);
      } else {
        throw Error(`未知动作：${step.op}`);
      }
      completed++;
    } catch (err) {
      error = String(err?.message || err);
      break;
    }
    if (completed < steps.length && tabMutationSeq !== seenMutationSeq) {
      seenMutationSeq = tabMutationSeq;
      const curTabs = await inventory();
      const openedNow = curTabs.filter(t => !beforeTags.has(t.tag) && t.openerTag === sourceTag);
      if (openedNow.length) break;
    }
  }

  // Smart event-driven settle
  if (!error) {
    if (mayNavigate) {
      await sleep(50);
      const freshTab = await chrome.tabs.get(tab.id).catch(() => null);
      if (st.loading || freshTab?.status !== 'complete' || freshTab?.pendingUrl) {
        await waitForLoad(tab.id, 3000);
      } else if (st.mainFrameId) {
        await evalFrame(tab.id, null, st.mainFrameId, '__novaBrowser.waitQuiet(110, 800)').catch(() => {});
      }
    } else {
      await sleep(25);
    }
  }

  const afterTabs = await inventory();
  const openedTabs = afterTabs.filter(t => !beforeTags.has(t.tag) && t.openerTag === sourceTag);
  let targetTab = tab;
  if (openedTabs.length === 1 && openedTabs[0].controllable) {
    try {
      targetTab = await resolveTag(openedTabs[0].tag);
      await waitForLoad(targetTab.id, 3000);
    } catch {}
  }

  let view = null;
  if (args.viewOpts) {
    view = await doView(targetTab, args.viewOpts).catch(() => null);
  }
  return { ok: !error, completed, error, actions, openedTabs, view, actionMs: Date.now() - started };
}

// -------------------- DevTools Ring Buffer --------------------
function recordDevtools(tabId, method, params) {
  const log = tabState.get(tabId)?.devtools;
  if (!log) return;
  const push = entry => { log.console.push(entry); if (log.console.length > DEVTOOLS_LIMIT) log.console.shift(); };
  if (method === 'Runtime.consoleAPICalled') {
    const text = (params.args || []).map(a => a.type === 'string' ? a.value : (a.description ?? JSON.stringify(a.value))).join(' ');
    push({ source: 'console', level: params.type, text: clip(text), time: params.timestamp });
  } else if (method === 'Runtime.exceptionThrown') {
    const d = params.exceptionDetails || {};
    push({ source: 'exception', level: 'error', text: clip(d.exception?.description || d.text), time: params.timestamp, url: clip(d.url, 2048) });
  } else if (method === 'Log.entryAdded') {
    const e = params.entry || {};
    push({ source: e.source, level: e.level, text: clip(e.text), time: e.timestamp, url: clip(e.url, 2048) });
  } else if (method === 'Network.requestWillBeSent') {
    log.network.delete(params.requestId);
    log.network.set(params.requestId, {
      requestId: params.requestId,
      method: params.request?.method,
      url: clip(params.request?.url, 2048),
      type: params.type,
      startedAt: Math.round((params.wallTime || 0) * 1000),
    });
    if (log.network.size > DEVTOOLS_LIMIT) log.network.delete(log.network.keys().next().value);
  } else {
    const req = log.network.get(params.requestId);
    if (!req) return;
    if (method === 'Network.responseReceived') Object.assign(req, { status: params.response?.status, statusText: params.response?.statusText, mimeType: params.response?.mimeType });
    else if (method === 'Network.loadingFinished') Object.assign(req, { finished: true, encodedBytes: params.encodedDataLength });
    else if (method === 'Network.loadingFailed') Object.assign(req, { failed: true, errorText: params.errorText });
  }
}

async function doDebug(tab, args) {
  const st = await attach(tab);
  if (!st.devtools) {
    st.devtools = { since: Date.now(), console: [], network: new Map() };
    for (const m of ['Runtime.enable', 'Log.enable', 'Network.enable']) {
      await cdp(tab.id, m, {}).catch(() => {});
    }
  }
  if (args.requestId) {
    const { body, base64Encoded } = await cdp(tab.id, 'Network.getResponseBody', { requestId: args.requestId });
    return { tab: args.tabTag, requestId: args.requestId, base64Encoded, bodyLength: body.length, body: clip(body, 65536) };
  }
  const q = args.query ? String(args.query).toLowerCase() : '';
  const lvl = args.level || '';
  const limit = Math.max(1, Math.min(DEVTOOLS_LIMIT, Number(args.limit) || 100));
  const keep = e => (!q || [e.text, e.url].some(v => v?.toLowerCase().includes(q)))
    && (!lvl || e.level === lvl || (lvl === 'error' && (e.failed || e.status >= 400)));
  const pick = list => { const matched = list.filter(keep); return { total: matched.length, items: matched.slice(-limit) }; };
  const res = { tab: args.tabTag, recordingSince: st.devtools.since };
  if (args.kind !== 'network') res.console = pick(st.devtools.console);
  if (args.kind !== 'console') {
    const allNet = [...st.devtools.network.values()];
    res.network = pick(allNet);
    if (q && res.network.total === 0 && allNet.length > 0) {
      res.network.unfilteredTotal = allNet.length;
      res.network.sampleUrls = allNet
        .filter(e => e.type === 'Fetch' || e.type === 'XHR')
        .slice(-6)
        .map(e => e.url);
    }
  }
  if (args.clear === true) { st.devtools.console = []; st.devtools.network.clear(); }
  return res;
}

// -------------------- Command Router --------------------
async function executeCommand(op, args = {}) {
  if (op === 'status') {
    return {
      version: chrome.runtime.getManifest?.().version || '0.2.0',
      incognitoAllowed: await chrome.extension.isAllowedIncognitoAccess(),
    };
  }
  if (op === 'reload_ext') {
    setTimeout(() => chrome.runtime.reload(), 200);
    return { reloading: true };
  }
  if (op === 'cursor_hide') {
    if (args.tabTag) {
      try {
        const t = await resolveTag(args.tabTag);
        await dismissTabCursor(t.id, Boolean(args.immediate));
      } catch {}
    } else {
      for (const tabId of [...attached]) {
        await dismissTabCursor(tabId, Boolean(args.immediate));
      }
    }
    return { hidden: true };
  }
  if (op === 'tabs') {
    const tabs = await inventory();
    const filtered = args.incognito == null ? tabs : tabs.filter(t => t.incognito === args.incognito);
    return { tabs: filtered, incognitoAllowed: await chrome.extension.isAllowedIncognitoAccess() };
  }
  if (op === 'downloads') {
    const query = { orderBy: ['-startTime'], limit: 100 };
    if (args.downloadId) query.id = Number(args.downloadId);
    else query.startedAfter = new Date(args.since ?? Date.now() - 600000).toISOString();
    const items = await chrome.downloads.search(query);
    return {
      downloads: items.map(d => ({
        id: String(d.id), url: clip(d.url, 4096), finalUrl: clip(d.finalUrl, 4096),
        path: d.filename, state: d.state, bytesReceived: d.bytesReceived, totalBytes: d.totalBytes,
        startTime: d.startTime, endTime: d.endTime, error: d.error || null, exists: d.exists, mime: d.mime,
      })),
    };
  }
  if (op === 'open') {
    const url = args.url ? checkUrl(args.url) : 'about:blank';
    let tab;
    if (args.incognito === true) {
      if (!await chrome.extension.isAllowedIncognitoAccess()) {
        throw Error('请在 chrome://extensions 中开启 Nova Chrome 的“允许在无痕模式下运行”');
      }
      const win = await chrome.windows.create({ url, incognito: true });
      [tab] = win.tabs || await chrome.tabs.query({ windowId: win.id });
    } else {
      const normal = (await chrome.windows.getAll({ windowTypes: ['normal'] })).find(w => !w.incognito);
      if (normal) tab = await chrome.tabs.create({ url, windowId: normal.id });
      else { const win = await chrome.windows.create({ url, incognito: false }); [tab] = win.tabs || await chrome.tabs.query({ windowId: win.id }); }
    }
    if (!tab) throw Error('未能创建标签页');
    if (args.focus === true) {
      await chrome.tabs.update(tab.id, { active: true });
      await chrome.windows.update(tab.windowId, { focused: true });
    }
    if (url !== 'about:blank') await waitForLoad(tab.id, 3500);
    const tabs = await inventory();
    const tabTag = model.tabs[tab.id]?.tag;
    const view = args.viewOpts ? await doView(tab, args.viewOpts).catch(() => null) : null;
    return { tabTag, tabs, view };
  }
  if (op === 'cookies_export') {
    const tabs = await chrome.tabs.query({});
    let tab = tabs.find(t => permittedUrl(t.url) && t.active) || tabs.find(t => permittedUrl(t.url));
    if (!tab) tab = await chrome.tabs.create({ url: 'about:blank', active: false });
    await attach(tab);
    await cdp(tab.id, 'Network.enable', {}).catch(() => {});
    const res = await cdp(tab.id, 'Network.getAllCookies', {});
    return { cookies: res?.cookies || [] };
  }
  if (op === 'cookies_import') {
    const tabs = await chrome.tabs.query({});
    let tab = tabs.find(t => permittedUrl(t.url) && t.active) || tabs.find(t => permittedUrl(t.url));
    if (!tab) tab = await chrome.tabs.create({ url: 'about:blank', active: false });
    await attach(tab);
    await cdp(tab.id, 'Network.enable', {}).catch(() => {});
    const cookies = Array.isArray(args.cookies) ? args.cookies : [];
    for (let i = 0; i < cookies.length; i += 500) {
      await cdp(tab.id, 'Network.setCookies', { cookies: cookies.slice(i, i + 500) });
    }
    return { count: cookies.length };
  }

  const tab = await resolveTag(args.tabTag);
  if (op === 'select_tab') {
    await chrome.tabs.update(tab.id, { active: true });
    await chrome.windows.update(tab.windowId, { focused: true });
    const view = args.viewOpts ? await doView(tab, args.viewOpts).catch(() => null) : null;
    return { tabTag: args.tabTag, tabs: await inventory(), view };
  }
  if (op === 'close_tab') {
    await detach(tab.id);
    await chrome.tabs.remove(tab.id);
    delete model.tabs[tab.id];
    await save();
    return { tabTag: args.tabTag, tabs: await inventory() };
  }
  if (op === 'goto') {
    const st = getState(tab.id);
    st.loading = true; st.contexts.clear();
    await chrome.tabs.update(tab.id, { url: checkUrl(args.url) });
    await waitForLoad(tab.id, 3500);
    const view = args.viewOpts ? await doView(tab, args.viewOpts).catch(() => null) : null;
    return { tabTag: args.tabTag, tabs: await inventory(), view };
  }
  if (op === 'back' || op === 'forward' || op === 'reload') {
    const st = getState(tab.id);
    st.loading = true; st.contexts.clear();
    if (op === 'back') await chrome.tabs.goBack(tab.id);
    else if (op === 'forward') await chrome.tabs.goForward(tab.id);
    else await chrome.tabs.reload(tab.id);
    await waitForLoad(tab.id, 3500);
    const view = args.viewOpts ? await doView(tab, args.viewOpts).catch(() => null) : null;
    return { tabTag: args.tabTag, tabs: await inventory(), view };
  }
  if (op === 'stop') {
    await detach(tab.id);
    return { stopped: true };
  }
  if (op === 'view') return doView(tab, args.viewOpts || {});
  if (op === 'act') return doAct(tab, args);
  if (op === 'eval') {
    const st = await attach(tab);
    const res = await evalFrame(tab.id, null, st.mainFrameId, `__novaBrowser.evalCode(${JSON.stringify(args.code)})`);
    const out = res?.ok ? { ok: true, value: res.value } : { ok: false, error: res?.error || '脚本执行失败' };
    if (args.withView) out.view = await doView(tab, args.viewOpts || {}).catch(() => null);
    return out;
  }
  if (op === 'debug') return doDebug(tab, args);
  if (op === 'apply_storage') {
    await waitForLoad(tab.id, 6000);
    await attach(tab);
    const items = JSON.stringify(args.pairs || []);
    const expr = `(()=>{const items=${items};for(const [k,v] of items){try{localStorage.setItem(k,v)}catch(e){}}return items.length})()`;
    const r = await cdp(tab.id, 'Runtime.evaluate', { expression: expr, returnByValue: true });
    await chrome.tabs.reload(tab.id);
    return { count: r?.result?.value || 0 };
  }
  throw Error(`未知浏览器指令：${op}`);
}

// -------------------- Persistent WebSocket Loop (Ports 47653-47662) --------------------
const newer = (a, b) => {
  const [x, y] = [a, b].map(v => String(v || '0').split('.').map(Number));
  for (let i = 0; i < 4; i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
  return false;
};

let cmdQueue = Promise.resolve();

function discover() {
  for (let port = 47653; port <= 47662; port++) void connectPort(port);
}

async function probePort(origin) {
  if (typeof fetch !== 'function') return true;
  const controller = typeof AbortController === 'function' ? new AbortController() : null;
  const timer = controller ? setTimeout(() => controller.abort(), 1200) : null;
  try {
    const res = await fetch(`${origin}/health`, {
      method: 'GET',
      cache: 'no-store',
      signal: controller?.signal,
    });
    if (!res.ok) return false;
    const data = await res.json().catch(() => null);
    return data?.ok === true && data?.app === 'nova';
  } catch {
    return false;
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function connectPort(port, attempt = 0) {
  const origin = `http://127.0.0.1:${port}`;
  if (sockets.has(origin) || connecting.has(origin)) return;
  connecting.add(origin);
  clearTimeout(reconnects.get(origin));
  reconnects.delete(origin);

  let alive = false;
  try {
    await ready();
    alive = await probePort(origin);
  } finally {
    connecting.delete(origin);
  }
  if (sockets.has(origin)) return;
  if (!alive) {
    scheduleReconnect(port, attempt);
    return;
  }

  const wsUrl = `ws://127.0.0.1:${port}/ws?clientId=${encodeURIComponent(model.clientId)}`;
  let ws;
  try {
    ws = new WebSocket(wsUrl);
  } catch {
    scheduleReconnect(port, attempt);
    return;
  }
  const pendingOut = new Map();
  let nextOut = 1;
  let pingTimer;
  const conn = {
    origin,
    ws,
    open: false,
    callHost(type, payload = {}) {
      return new Promise((resolve, reject) => {
        if (!conn.open) { reject(Error('Nova 未连接')); return; }
        const id = `ext-${nextOut++}`;
        const t = setTimeout(() => { pendingOut.delete(id); reject(Error('Nova 响应超时')); }, 5000);
        pendingOut.set(id, { resolve: v => { clearTimeout(t); resolve(v); }, reject: e => { clearTimeout(t); reject(e); } });
        ws.send(JSON.stringify({ type, id, ...payload }));
      });
    },
  };
  sockets.set(origin, conn);

  ws.onopen = () => {
    conn.open = true;
    attempt = 0;
    pingTimer = setInterval(() => {
      if (ws.readyState === WebSocket.OPEN) {
        void chrome.runtime.getPlatformInfo?.().catch(() => {});
        ws.send('{"type":"ping"}');
      }
    }, 20000);
  };

  ws.onmessage = async event => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }
    if (msg.type === 'pong') return;
    if (msg.type === 'hello') {
      if (typeof msg.runtime === 'string' && msg.runtime) {
        if (pageRuntime !== msg.runtime) {
          pageRuntime = msg.runtime;
          // Force attached tabs to upgrade their isolated world runtime to the latest version.
          for (const st of tabState.values()) st.contexts.clear();
        }
      }
      const myVer = chrome.runtime.getManifest?.().version;
      if (msg.version && myVer && newer(msg.version, myVer)) {
        const stored = await chrome.storage.local.get('reloadedFor').catch(() => ({}));
        if (stored.reloadedFor !== msg.version) {
          await chrome.storage.local.set({ reloadedFor: msg.version }).catch(() => {});
          chrome.runtime.reload();
        }
      }
      return;
    }
    if (msg.type === 'host_reply' && msg.id && pendingOut.has(msg.id)) {
      const p = pendingOut.get(msg.id);
      pendingOut.delete(msg.id);
      if (msg.error) p.reject(Error(msg.error)); else p.resolve(msg.result);
      return;
    }
    if (msg.type === 'cmd' && msg.id) {
      const task = cmdQueue.then(() => executeCommand(msg.op, msg.args || {}));
      cmdQueue = task.catch(() => {});
      let reply;
      try { reply = { type: 'reply', id: msg.id, result: await task }; }
      catch (err) { reply = { type: 'reply', id: msg.id, error: String(err?.message || err) }; }
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(reply));
    }
  };

  const cleanup = () => {
    clearInterval(pingTimer);
    conn.open = false;
    for (const p of pendingOut.values()) p.reject(Error('连接已断开'));
    pendingOut.clear();
    if (sockets.get(origin) === conn) sockets.delete(origin);
    if (![...sockets.values()].some(c => c.open)) {
      for (const tabId of [...attached]) void dismissTabCursor(tabId, false);
    }
    scheduleReconnect(port, attempt);
  };
  ws.onclose = cleanup;
  ws.onerror = () => { try { ws.close(); } catch {} };
}

function scheduleReconnect(port, attempt) {
  const origin = `http://127.0.0.1:${port}`;
  clearTimeout(reconnects.get(origin));
  const delay = Math.min(500 * Math.pow(2, Math.min(attempt, 4)), 6000);
  reconnects.set(origin, setTimeout(() => void connectPort(port, attempt + 1), delay));
}

// -------------------- Miniapp Launchers & Popup Support --------------------
const escapeRegExp = s => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
const globMatch = (pattern, url) => new RegExp('^' + pattern.split('*').map(escapeRegExp).join('.*') + '$').test(url);

async function miniappMatches(url) {
  if (!/^https?:\/\//i.test(url || '')) return [];
  const openConns = [...sockets.values()].filter(c => c.open);
  const found = [];
  await Promise.all(openConns.map(async conn => {
    try {
      const res = await conn.callHost('miniapp_triggers');
      for (const trigger of Array.isArray(res?.triggers) ? res.triggers : []) {
        if (typeof trigger?.appId === 'string' && Array.isArray(trigger.match) && trigger.match.some(p => typeof p === 'string' && globMatch(p, url))) {
          found.push({ origin: conn.origin, appId: trigger.appId, name: String(trigger.name || '') });
        }
      }
    } catch {}
  }));
  return found;
}

async function launchMiniapp(message, tab) {
  const conn = sockets.get(message.origin);
  if (!conn?.open || !tab) throw Error('Nova 未连接');
  await inventory();
  const tabTag = model.tabs[tab.id]?.tag;
  if (!tabTag) throw Error('启动应用的标签页已关闭');
  await conn.callHost('miniapp_launch', { appId: message.appId, url: tab.url, title: tab.title || '', tabTag });
  return { ok: true };
}

// -------------------- Chrome Event Listeners --------------------
chrome.debugger.onEvent.addListener((source, method, params) => {
  const st = tabState.get(source.tabId);
  if (!source.sessionId) recordDevtools(source.tabId, method, params);
  if (!st) return;
  if (method === 'Target.attachedToTarget' && params.targetInfo?.type === 'iframe') {
    st.childFrames.set(params.targetInfo.targetId, { targetInfo: params.targetInfo, sessionId: params.sessionId });
  } else if (method === 'Target.detachedFromTarget') {
    for (const [id, f] of st.childFrames) if (f.sessionId === params.sessionId) st.childFrames.delete(id);
  } else if (method === 'Page.frameStartedLoading' && params.frameId === st.mainFrameId) {
    st.loading = true;
  } else if (method === 'Page.frameNavigated') {
    if (params.frame?.id === st.mainFrameId) st.contexts.clear();
    else for (const k of [...st.contexts.keys()]) if (k.endsWith(`:${params.frame?.id}`)) st.contexts.delete(k);
  } else if (method === 'Page.domContentEventFired' || method === 'Page.loadEventFired' || (method === 'Page.frameStoppedLoading' && params.frameId === st.mainFrameId)) {
    st.loading = false;
    notifyLoad(source.tabId);
  }
});

chrome.debugger.onDetach.addListener(source => forget(source.tabId));
chrome.webNavigation?.onCreatedNavigationTarget.addListener(({ tabId, sourceTabId }) => {
  tabMutationSeq++;
  navigationOpeners.set(tabId, sourceTabId);
});
chrome.tabs.onCreated?.addListener(() => { tabMutationSeq++; });
chrome.tabs.onRemoved.addListener(id => {
  tabMutationSeq++;
  navigationOpeners.delete(id);
  forget(id);
  void ready().then(async () => { delete model.tabs[id]; await save(); });
});
chrome.tabs.onReplaced.addListener((added, removed) => {
  tabMutationSeq++;
  void ready().then(async () => {
    const opener = navigationOpeners.get(removed);
    if (opener !== undefined) { navigationOpeners.set(added, opener); navigationOpeners.delete(removed); }
    if (model.tabs[removed]) { model.tabs[added] = model.tabs[removed]; delete model.tabs[removed]; await save(); }
    forget(removed);
  });
});
chrome.tabs.onUpdated.addListener((id, change) => {
  if (change.url) void chrome.tabs.sendMessage(id, { type: 'miniapp-refresh' }).catch(() => {});
  if (change.status === 'complete') notifyLoad(id);
});

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (sender.id !== chrome.runtime.id) return;
  const task = async () => {
    await ready();
    if (message.type === 'miniapp-match') return { apps: await miniappMatches(sender.tab?.url) };
    if (message.type === 'miniapp-launch') return launchMiniapp(message, sender.tab);
    if (message.type !== 'status' && message.type !== 'reconnect') throw Error('未知请求');
    discover();
    const tabs = await inventory();
    const [active] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    const count = [...sockets.values()].filter(c => c.open).length;
    return { tab: active ? tabs.find(t => t.tag === model.tabs[active.id]?.tag) : null, connection: { connected: count > 0, count } };
  };
  task().then(reply, err => reply({ error: String(err?.message || err) }));
  return true;
});

chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === 'nova-reconnect') discover(); });
chrome.runtime.onInstalled.addListener(() => { void chrome.alarms.create('nova-reconnect', { periodInMinutes: 0.5 }); discover(); });
chrome.runtime.onStartup.addListener(() => discover());
discover();
