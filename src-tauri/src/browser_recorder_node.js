// Nova 浏览器录制器：Playwright 驱动真实 Chrome/Edge 窗口。
const { chromium } = require('playwright-core');
const fs = require('fs');
const path = require('path');

const out = (obj) => process.stdout.write('__NOVA__' + JSON.stringify(obj) + '\n');
const collectorSource = fs.readFileSync(path.join(__dirname, 'browser_collector.js'), 'utf8');
let browser = null;
let context = null;
let page = null;
let recording = false;
let paused = false;
let shuttingDown = false;
let closedEmitted = false;
let saveStateTimer = null;
let pendingAddressNavigations = 0;
const pendingAddressWaiters = [];
const attachedPages = new WeakSet();
const pageStates = new WeakMap();
const storageStatePath = process.env.NOVA_BROWSER_STORAGE_STATE || '';
let requestedHeadless = false;
let relaunching = false;
const runPages = new Map();
const runOutputDirs = new Map();

function browserLaunchOptions(headless) {
  return {
    headless,
    timeout: 20000,
    ...(headless ? {} : { args: ['--start-maximized'] }),
  };
}

function contextLaunchOptions(headless) {
  return {
    viewport: headless ? { width: 1280, height: 800 } : null,
    ...(storageStatePath && fs.existsSync(storageStatePath) ? { storageState: storageStatePath } : {}),
  };
}

function safeShotName(requestedPath) {
  const name = path.basename(requestedPath || 'current.png').replace(/[^a-zA-Z0-9._-]/g, '-');
  return name && name !== '.' && name !== '..' ? name : 'current.png';
}

function execShotPath(command) {
  const requestedPath = String(command.path || '').replace(/\\/g, '/');
  const outputDir = runOutputDirs.get(command.runId);
  return outputDir ? path.join(outputDir, safeShotName(requestedPath)) : requestedPath;
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function pageViewport(target) {
  return target.evaluate(() => ({ width: window.innerWidth, height: window.innerHeight, deviceScaleFactor: window.devicePixelRatio }));
}

function mapViewportPoint(command, viewport) {
  let x = Number(command.x), y = Number(command.y);
  if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error('点击坐标必须是有效数字');
  if (command.normalized) {
    if (x < 0 || x > 1000 || y < 0 || y > 1000) throw new Error('归一化坐标必须在 0..1000 内');
    x = x * viewport.width / 1000;
    y = y * viewport.height / 1000;
  } else if (command.imageWidth || command.imageHeight) {
    const imageWidth = Number(command.imageWidth), imageHeight = Number(command.imageHeight);
    if (!(imageWidth > 0) || !(imageHeight > 0)) throw new Error('截图尺寸必须大于 0');
    x = x * viewport.width / imageWidth;
    y = y * viewport.height / imageHeight;
  }
  if (x < 0 || x > viewport.width || y < 0 || y > viewport.height) throw new Error('点击坐标超出当前视口');
  return { x: Math.min(x, viewport.width - 1), y: Math.min(y, viewport.height - 1) };
}

async function commandPoint(target, command) {
  return mapViewportPoint(command, await pageViewport(target));
}

async function prepareFullPageScreenshot(target) {
  await target.evaluate(async () => {
    const originalX = window.scrollX, originalY = window.scrollY;
    const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    let previousHeight = 0, stableBottom = 0;
    window.scrollTo(0, 0);
    await wait(120);
    // ponytail: 最多预滚动 100 次，超长或无限流页面仍由分段截图兜底。
    for (let step = 0; step < 100 && stableBottom < 2; step += 1) {
      const height = document.scrollingElement?.scrollHeight || document.documentElement.scrollHeight;
      const bottom = Math.max(0, height - window.innerHeight);
      const next = Math.min(bottom, window.scrollY + Math.max(1, Math.floor(window.innerHeight * 0.8)));
      window.scrollTo(0, next);
      window.dispatchEvent(new Event('scroll'));
      await wait(120);
      const updatedHeight = document.scrollingElement?.scrollHeight || document.documentElement.scrollHeight;
      stableBottom = window.scrollY >= bottom && updatedHeight === previousHeight ? stableBottom + 1 : 0;
      previousHeight = updatedHeight;
    }
    window.scrollTo(originalX, originalY);
    await wait(200);
  });
}

function semanticLocator(target, command) {
  if (command.role && command.name) return target.getByRole(String(command.role), { name: String(command.name), exact: command.exact !== false });
  if (command.label) return target.getByLabel(String(command.label), { exact: command.exact !== false });
  if (command.text) return target.getByText(String(command.text), { exact: command.exact !== false });
  return null;
}

function finishPendingAddressNavigation() {
  pendingAddressNavigations = Math.max(0, pendingAddressNavigations - 1);
  if (pendingAddressNavigations !== 0) return;
  for (const resolve of pendingAddressWaiters.splice(0)) resolve();
}

function waitForPendingAddressNavigations() {
  if (pendingAddressNavigations === 0) return Promise.resolve();
  return new Promise((resolve) => pendingAddressWaiters.push(resolve));
}

function stateOf(target) {
  let state = pageStates.get(target);
  if (!state) {
    state = {
      collectorReady: false,
      pendingNavigationUntil: 0,
      mainDocumentRequest: null,
      lastReportedUrl: '',
      lastRecordedNavigationUrl: '',
      lastRecordedNavigationAt: 0,
      lastOperationAt: 0,
    };
    pageStates.set(target, state);
  }
  return state;
}

function livePages() {
  return context ? context.pages().filter((candidate) => !candidate.isClosed()) : [];
}

function currentPage() {
  if (page && !page.isClosed()) return page;
  const pages = livePages();
  page = pages.at(-1) || null;
  return page;
}

function focusedPage(fallback = currentPage()) {
  const pages = livePages();
  if (pages.length < 2) return Promise.resolve(fallback);
  return Promise.all(pages.map(async (candidate) => ({
    candidate,
    focused: await candidate.evaluate(() => document.hasFocus()).catch(() => false),
  }))).then((results) => {
    const focused = results.find((result) => result.focused)?.candidate || fallback;
    if (focused) page = focused;
    return focused;
  });
}

function comparableUrl(value) {
  try {
    const url = new URL(value);
    url.hash = '';
    return url.href;
  } catch {
    return value;
  }
}

function reportNavigation(target, url, navigationSource = 'operation', error) {
  const state = stateOf(target);
  state.lastReportedUrl = url;
  out({ type: 'nav', url, navigationSource, ...(error ? { error } : {}) });
}

function emitClosedAndExit() {
  if (!closedEmitted) {
    closedEmitted = true;
    out({ type: 'closed' });
  }
  setImmediate(() => process.exit(0));
}

function scheduleStorageStateSave() {
  if (!context || !storageStatePath) return;
  clearTimeout(saveStateTimer);
  saveStateTimer = setTimeout(() => {
    context.storageState({ path: storageStatePath }).catch((error) => {
      out({ type: 'error', error: '保存浏览器登录态失败: ' + String(error?.message || error) });
    });
  }, 250);
}

async function saveStorageState() {
  if (!context || !storageStatePath) return;
  clearTimeout(saveStateTimer);
  await context.storageState({ path: storageStatePath });
}

async function syncFlags() {
  await Promise.all(livePages().map((target) => target.evaluate(([on, isPaused]) => {
    window.__novaRecording = on;
    window.__novaRecordingPaused = isPaused;
  }, [recording, paused]).catch(() => {})));
}

function shouldExitAfterLastPageCloses(pageCount, isShuttingDown, isRelaunching) {
  return pageCount === 0 && !isShuttingDown && !isRelaunching;
}

function attachPage(target) {
  if (attachedPages.has(target)) return;
  attachedPages.add(target);
  page = target;
  const state = stateOf(target);

  target.on('request', (request) => {
    if (request.frame() !== target.mainFrame() || request.resourceType() !== 'document') return;
    const requestAt = Date.now();
    const redirected = Boolean(request.redirectedFrom());
    state.mainDocumentRequest = { url: request.url(), redirected };
    if (redirected) return;

    // framenavigated 只会在重定向链最终提交时触发；若等到那里，最初在地址栏输入的 URL
    // 已被覆盖成 redirected 请求，整条跳转就会漏录。候选请求稍等一小段时间再落步骤，
    // 让同一时刻的点击/回车绑定先到 Node，避免把页面操作触发的跳转误判为地址栏输入。
    const commandNavigation = requestAt <= state.pendingNavigationUntil;
    const url = request.url();
    if (!commandNavigation && url && url !== 'about:blank') {
      pendingAddressNavigations += 1;
      setTimeout(() => {
        try {
          const operationNavigation = requestAt <= state.lastOperationAt + 2500;
          const now = Date.now();
          if (
            recording &&
            !paused &&
            !operationNavigation &&
            (state.lastRecordedNavigationUrl !== url || now - state.lastRecordedNavigationAt > 500)
          ) {
            state.lastRecordedNavigationUrl = url;
            state.lastRecordedNavigationAt = now;
            out({
              type: 'event',
              ts: requestAt,
              kind: 'navigate',
              url,
              target: null,
              data: { navigationSource: 'address_bar', trigger: null },
            });
          }
        } finally {
          finishPendingAddressNavigation();
        }
      }, 75);
    }
  });

  target.on('framenavigated', async (frame) => {
    if (frame !== target.mainFrame()) return;
    page = target;
    state.collectorReady = false;
    const url = target.url();
    const now = Date.now();
    const requestInfo = state.mainDocumentRequest;
    const requestMatches = requestInfo && comparableUrl(requestInfo.url) === comparableUrl(url);
    const operationNavigation = now <= state.pendingNavigationUntil;
    const directAddressNavigation = Boolean(!operationNavigation && requestMatches && !requestInfo.redirected);
    state.pendingNavigationUntil = 0;
    state.mainDocumentRequest = null;
    reportNavigation(target, url, directAddressNavigation ? 'address_bar' : 'operation');
    await target.waitForLoadState('domcontentloaded').catch(() => {});
    await syncFlags();
    scheduleStorageStateSave();
  });

  target.on('close', () => {
    if (page === target) page = null;
    if (!shouldExitAfterLastPageCloses(livePages().length, shuttingDown, relaunching)) return;
    shuttingDown = true;
    void saveStorageState()
      .catch(() => {})
      .then(() => browser?.close().catch(() => {}))
      .finally(emitClosedAndExit);
  });
}

async function ensureBrowser() {
  const existing = currentPage();
  if (browser?.isConnected() && context && existing) return existing;
  if (browser?.isConnected() && context) {
    const target = await context.newPage();
    attachPage(target);
    return target;
  }

  let lastError = null;
  for (const channel of ['chrome', 'msedge']) {
    try {
      const launchedBrowser = await chromium.launch({
        channel,
        ...browserLaunchOptions(requestedHeadless),
      });
      const launchedContext = await launchedBrowser.newContext(contextLaunchOptions(requestedHeadless));
      browser = launchedBrowser;
      context = launchedContext;
      shuttingDown = false;
      closedEmitted = false;

      await context.exposeBinding('__novaPush', ({ page: sourcePage }, event) => {
        if (sourcePage) {
          attachPage(sourcePage);
          page = sourcePage;
          const state = stateOf(sourcePage);
          if (
            event.kind === 'click' ||
            event.kind === 'submit' ||
            (event.kind === 'key' && event.data?.key === 'Enter')
          ) {
            const now = Date.now();
            state.lastOperationAt = now;
            state.pendingNavigationUntil = now + 2500;
          }
        }
        const now = Date.now();
        out({ type: 'event', ts: now, ...event });
        scheduleStorageStateSave();
      });
      await context.exposeBinding('__novaCollectorReady', ({ page: sourcePage }, { url }) => {
        if (sourcePage) {
          attachPage(sourcePage);
          stateOf(sourcePage).collectorReady = true;
        }
        out({ type: 'collectorReady', url });
      });
      await context.addInitScript({ content: collectorSource });
      context.on('page', attachPage);
      launchedBrowser.on('disconnected', () => {
        if (browser !== launchedBrowser) return;
        browser = null;
        context = null;
        page = null;
        if (!relaunching) emitClosedAndExit();
      });

      const target = await context.newPage();
      attachPage(target);
      return target;
    } catch (error) {
      lastError = error;
      const failedBrowser = browser;
      browser = null;
      context = null;
      page = null;
      if (failedBrowser) await failedBrowser.close().catch(() => {});
    }
  }
  throw new Error('未找到 Chrome 或 Edge: ' + String(lastError?.message || lastError));
}

const commands = {
  async ensureBrowser() {
    const target = await ensureBrowser();
    return { url: target.url(), headless: requestedHeadless };
  },
  async setHeadless(command) {
    const next = Boolean(command.headless);
    if (requestedHeadless === next) return { headless: next };
    requestedHeadless = next;
    if (browser?.isConnected()) {
      relaunching = true;
      runPages.clear();
      await saveStorageState().catch(() => {});
      await browser.close().catch(() => {});
      browser = null;
      context = null;
      page = null;
      relaunching = false;
    }
    return { headless: next };
  },
  async navigate(command) {
    const target = await ensureBrowser();
    let url = String(command.url || '').trim();
    if (url && !/^https?:\/\//i.test(url)) url = 'https://' + url;
    const state = stateOf(target);
    const previousPendingNavigationUntil = state.pendingNavigationUntil;
    const commandNavigationUntil = Date.now() + 35000;
    state.pendingNavigationUntil = commandNavigationUntil;
    try {
      await target.goto(url || 'about:blank', { waitUntil: 'domcontentloaded', timeout: 30000 });
      await syncFlags();
      const currentUrl = target.url();
      if (state.lastReportedUrl !== currentUrl) reportNavigation(target, currentUrl, 'operation');
      return { url: currentUrl };
    } catch (error) {
      const message = String(error?.message || error);
      reportNavigation(target, target.url(), 'operation', message);
      throw error;
    } finally {
      // 若命令执行期间页面内又发生了点击/回车，保留那个更晚的操作导航窗口；
      // 只清掉本次 goto 自己设置的标记。
      if (state.pendingNavigationUntil === commandNavigationUntil) {
        state.pendingNavigationUntil = previousPendingNavigationUntil;
      }
    }
  },
  async startRecord() {
    const fallback = await ensureBrowser();
    const target = await focusedPage(fallback) || fallback;
    for (const candidate of livePages()) {
      const state = stateOf(candidate);
      state.pendingNavigationUntil = 0;
      state.mainDocumentRequest = null;
      state.lastRecordedNavigationUrl = '';
      state.lastRecordedNavigationAt = 0;
      state.lastOperationAt = 0;
    }
    recording = true;
    paused = false;
    await syncFlags();
    const url = target.url();
    if (url && url !== 'about:blank') {
      stateOf(target).lastRecordedNavigationUrl = url;
      stateOf(target).lastRecordedNavigationAt = Date.now();
      out({
        type: 'event',
        ts: Date.now(),
        kind: 'navigate',
        url,
        target: null,
        data: { navigationSource: 'record_start', trigger: null },
      });
    }
    out({ type: 'recording', on: true, collectorReady: stateOf(target).collectorReady });
    return { url };
  },
  async stopRecord() {
    // 给刚发生的地址栏跳转/输入事件一个进入事件管道的机会，再发送完成确认。
    await delay(120);
    await waitForPendingAddressNavigations();
    recording = false;
    paused = false;
    await syncFlags();
    await saveStorageState();
    out({ type: 'recording', on: false, collectorReady: livePages().some((target) => stateOf(target).collectorReady) });
  },
  async pause() {
    paused = true;
    await syncFlags();
  },
  async resume() {
    paused = false;
    await syncFlags();
  },
  async regionScreenshot(command) {
    const fallback = await ensureBrowser();
    const target = await focusedPage(fallback) || fallback;
    const clip = await target.evaluate(() => new Promise((resolve) => {
      const overlay = document.createElement('div');
      overlay.style.cssText = 'position:fixed;inset:0;z-index:2147483647;cursor:crosshair;background:rgba(0,0,0,.08)';
      const tip = document.createElement('div');
      tip.textContent = '拖拽框选要截图的区域，Esc 取消';
      tip.style.cssText = 'position:fixed;top:12px;left:50%;transform:translateX(-50%);padding:6px 14px;background:#222;color:#fff;font:13px sans-serif;border-radius:6px';
      overlay.appendChild(tip);
      let box = null;
      let startX = 0;
      let startY = 0;
      const finish = (value) => {
        window.removeEventListener('keydown', onKey, true);
        overlay.remove();
        resolve(value);
      };
      const onKey = (event) => {
        if (event.key === 'Escape') finish(null);
      };
      window.addEventListener('keydown', onKey, true);
      overlay.onmousedown = (event) => {
        startX = event.clientX;
        startY = event.clientY;
        box = document.createElement('div');
        box.style.cssText = 'position:fixed;border:2px solid #4f8cff;background:rgba(79,140,255,.15)';
        overlay.appendChild(box);
      };
      overlay.onmousemove = (event) => {
        if (!box) return;
        const x = Math.min(startX, event.clientX);
        const y = Math.min(startY, event.clientY);
        box.style.left = x + 'px';
        box.style.top = y + 'px';
        box.style.width = Math.abs(event.clientX - startX) + 'px';
        box.style.height = Math.abs(event.clientY - startY) + 'px';
      };
      overlay.onmouseup = (event) => {
        if (!box) return;
        const value = {
          x: Math.min(startX, event.clientX),
          y: Math.min(startY, event.clientY),
          width: Math.abs(event.clientX - startX),
          height: Math.abs(event.clientY - startY),
        };
        finish(value.width > 8 && value.height > 8 ? value : null);
      };
      document.documentElement.appendChild(overlay);
    }));
    if (!clip) {
      out({ type: 'shot', reqId: command.reqId, data: null, cancelled: true });
      return;
    }
    const image = await target.screenshot({ clip, scale: 'css' });
    out({ type: 'shot', reqId: command.reqId, data: 'data:image/png;base64,' + image.toString('base64'), clip });
  },
  async close() {
    shuttingDown = true;
    await saveStorageState().catch(() => {});
    if (browser) await browser.close().catch(() => {});
    emitClosedAndExit();
  },

  // ---------- 双子座计划执行：每个 runId 绑定一个专用 tab，可并行多个 ----------
  async execConfigure(command) {
    const outputDir = String(command.outputDir || '');
    if (!command.runId || !outputDir) throw new Error('运行截图目录未配置');
    runOutputDirs.set(command.runId, outputDir);
    return null;
  },
  async execOpen(command) {
    await ensureBrowser();
    const target = await context.newPage();
    attachPage(target);
    runPages.set(command.runId, target);
    target.on('close', () => {
      if (runPages.get(command.runId) === target) runPages.delete(command.runId);
    });
    return { url: target.url() };
  },
  async execClose(command) {
    const target = runPages.get(command.runId);
    runPages.delete(command.runId);
    runOutputDirs.delete(command.runId);
    if (target && !target.isClosed()) await target.close().catch(() => {});
    return null;
  },
  async execGoto(command) {
    const target = runPageOf(command.runId);
    let url = String(command.url || '').trim();
    if (url && !/^https?:\/\//i.test(url)) url = 'https://' + url;
    if (command.sessionStorage && command.sessionStorage.key) {
      const { key, value } = command.sessionStorage;
      let origin = '';
      try { origin = new URL(url).origin; } catch {}
      await context.addInitScript(([k, v, o]) => {
        try { if (!o || location.origin === o) sessionStorage.setItem(k, v); } catch {}
      }, [key, value, origin]);
    }
    await target.goto(url, { waitUntil: 'domcontentloaded', timeout: 30000 });
    return { url: target.url() };
  },
  async execEval(command) {
    const target = runPageOf(command.runId);
    const result = await target.evaluate(command.expression);
    return { result: result === undefined ? null : result };
  },
  async execClick(command) {
    const target = runPageOf(command.runId);
    const semantic = semanticLocator(target, command);
    if (!semantic) {
      await target.click(command.selector, { timeout: command.timeout || 10000 });
      return null;
    }
    const count = await semantic.count();
    if (count !== 1) throw new Error(`语义点击目标匹配到 ${count} 个元素`);
    await semantic.click({ timeout: command.timeout || 10000 });
    return null;
  },
  async execFill(command) {
    const target = runPageOf(command.runId);
    await target.fill(command.selector, String(command.value ?? ''), { timeout: command.timeout || 10000 });
    return null;
  },
  // ---- 纯视觉坐标操作（不看 DOM）：截图强制按 CSS 像素输出，另支持 0..1000 归一化坐标 ----
  // viewport:null 时 Playwright viewportSize() 为 null，必须从页面读取真实窗口尺寸。
  async execViewport(command) {
    return pageViewport(runPageOf(command.runId));
  },
  async execMouseClick(command) {
    const target = runPageOf(command.runId);
    const { x, y } = await commandPoint(target, command);
    await target.mouse.move(x, y);
    await target.mouse.click(x, y, { button: command.button || 'left', clickCount: command.double ? 2 : 1 });
    return { x, y };
  },
  async execMouseMove(command) {
    const target = runPageOf(command.runId);
    const { x, y } = await commandPoint(target, command);
    await target.mouse.move(x, y);
    return { x, y };
  },
  // 滚动页面（deltaY>0 向下），用于把视野外目标滚进来再截图。
  async execScroll(command) {
    const target = runPageOf(command.runId);
    await target.mouse.wheel(Number(command.deltaX || 0), Number(command.deltaY ?? 600));
    return null;
  },
  // 拟人逐键输入（触发真实键盘事件，配合 execMouseClick 先点进输入框）。
  async execType(command) {
    const target = runPageOf(command.runId);
    await target.keyboard.type(String(command.text ?? ''), { delay: command.delay || 30 });
    return null;
  },
  async execKey(command) {
    const target = runPageOf(command.runId);
    await target.keyboard.press(String(command.key || 'Enter'));
    return null;
  },
  async execQuery(command) {
    const target = runPageOf(command.runId);
    const result = await target.evaluate((selector) => {
      const els = Array.from(document.querySelectorAll(selector)).slice(0, 20);
      return els.map((el) => ({
        tag: el.tagName.toLowerCase(),
        text: (el.textContent || '').trim().slice(0, 60),
        visible: !!(el.offsetWidth || el.offsetHeight),
      }));
    }, command.selector);
    return { count: result.length, items: result };
  },
  // 读取参考图（targetImagePaths）为 dataURL 返回，agent 无需本地读文件权限即可查看。
  async execReadImage(command) {
    const fs = require('fs');
    const rawPath = String(command.path || '').replace(/\\/g, '/');
    const buf = fs.readFileSync(rawPath);
    const ext = (rawPath.split('.').pop() || 'png').toLowerCase();
    const mime = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : ext === 'webp' ? 'image/webp' : 'image/png';
    return { dataUrl: `data:${mime};base64,${buf.toString('base64')}` };
  },
  // 等待 selector 出现/可见，或自定义条件成立；联动筛选依赖它等待下一个筛选项加载出来。
  async execWait(command) {
    const target = runPageOf(command.runId);
    const timeout = command.timeout || 10000;
    if (command.expression) {
      await target.waitForFunction(command.expression, { timeout });
      return { waited: 'expression' };
    }
    if (command.selector) {
      await target.waitForSelector(command.selector, { state: command.state || 'visible', timeout });
      return { waited: command.selector };
    }
    // 无参数时当作固定延时（等待异步刷新）
    await new Promise((resolve) => setTimeout(resolve, command.ms || 800));
    return { waited: 'ms' };
  },
  async execShot(command) {
    const target = runPageOf(command.runId);
    // 计划截图固定落在 Nova 数据目录；agent 只传 ASCII 文件名，避免 Windows PowerShell
    // 5.1 把命令中的非 ASCII 用户目录转成问号后再发进 JSON。
    const rawPath = execShotPath(command);
    fs.mkdirSync(path.dirname(rawPath), { recursive: true });
    const options = { path: rawPath, scale: 'css' };
    if (command.fullPage && !command.selector) {
      // 先逐屏触发 body 页面懒加载，再交给 Playwright 原生 fullPage 一次成图。
      await prepareFullPageScreenshot(target);
      await target.screenshot({ ...options, fullPage: true });
    } else if (command.selector) {
      // 等元素出现且可见，避免截到半渲染的空壳。
      await target.waitForSelector(command.selector, { state: 'visible', timeout: command.timeout || 10000 });
      if (command.full) {
        // 不改 DOM（overflow/flex/类样式撑不开）：先滚动整页触发懒加载，再用 fullPage+clip
        // 按元素的文档坐标截出完整内容，body 滚动、sticky 表头、内部容器均适用。
        await prepareFullPageScreenshot(target);
        const clip = await target.evaluate((sel) => {
          const el = document.querySelector(sel);
          if (!el) return null;
          const r = el.getBoundingClientRect();
          return { x: Math.max(0, r.left + window.scrollX), y: Math.max(0, r.top + window.scrollY), width: r.width, height: r.height };
        }, command.selector);
        if (clip && clip.width > 0 && clip.height > 0) {
          await target.screenshot({ ...options, fullPage: true, clip });
        } else {
          await target.screenshot({ ...options, fullPage: true });
        }
      } else {
        const locator = target.locator(command.selector).first();
        await locator.screenshot(options);
      }
    } else {
      await target.screenshot(options);
    }
    return { path: rawPath, viewport: await pageViewport(target) };
  },
};

function runPageOf(runId) {
  const target = runPages.get(runId);
  if (!target || target.isClosed()) throw new Error('运行页不存在或已关闭: ' + runId);
  return target;
}

// ---------- HTTP 控制端口：供 agent 闭环执行；与 stdin 录制通道并行 ----------
let execServer = null;
let execQueue = Promise.resolve();
function startExecServer() {
  const http = require('http');
  execServer = http.createServer((req, res) => {
    if (req.method !== 'POST') { res.writeHead(405).end(); return; }
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      let command;
      try { command = JSON.parse(body || '{}'); } catch { res.writeHead(400, { 'Content-Type': 'application/json' }).end(JSON.stringify({ ok: false, error: 'bad json' })); return; }
      const handler = commands[command.cmd];
      if (!handler) { res.writeHead(404, { 'Content-Type': 'application/json' }).end(JSON.stringify({ ok: false, error: 'unknown cmd: ' + command.cmd })); return; }
      execQueue = execQueue.then(async () => {
        try {
          const data = await handler(command);
          res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ ok: true, data: data ?? null }));
        } catch (error) {
          res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ ok: false, error: String(error?.message || error) }));
        }
      });
    });
  });
  execServer.listen(0, '127.0.0.1', () => {
    out({ type: 'execPort', port: execServer.address().port });
  });
}
if (process.env.NOVA_BROWSER_RECORDER_TEST === '1') {
  module.exports = {
    browserLaunchOptions,
    contextLaunchOptions,
    execShotPath,
    mapViewportPoint,
    pageViewport,
    prepareFullPageScreenshot,
    safeShotName,
    semanticLocator,
    shouldExitAfterLastPageCloses,
  };
} else {
  startExecServer();

  let commandQueue = Promise.resolve();
  let inputBuffer = '';
  const enqueueLine = (line) => {
    let command;
    try {
      command = JSON.parse(line);
    } catch {
      return;
    }
    const handler = commands[command.cmd];
    if (!handler) return;
    commandQueue = commandQueue.then(async () => {
      try {
        const data = await handler(command);
        if (command.commandId) out({ type: 'commandResult', commandId: command.commandId, ok: true, data: data ?? null });
      } catch (error) {
        const message = String(error?.message || error);
        out({ type: 'error', error: message, cmd: command.cmd });
        if (command.commandId) out({ type: 'commandResult', commandId: command.commandId, ok: false, error: message });
      }
    });
  };
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    inputBuffer += chunk;
    for (;;) {
      const newline = inputBuffer.indexOf('\n');
      if (newline < 0) break;
      const line = inputBuffer.slice(0, newline).trimEnd();
      inputBuffer = inputBuffer.slice(newline + 1);
      if (line) enqueueLine(line);
    }
  });
  process.stdin.on('end', () => {
    const line = inputBuffer.trim();
    if (line) enqueueLine(line);
    commandQueue.finally(async () => {
      shuttingDown = true;
      await saveStorageState().catch(() => {});
      if (browser) await browser.close().catch(() => {});
      process.exit(0);
    });
  });
  out({ type: 'hello' });
}
