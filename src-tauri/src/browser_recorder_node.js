// Nova 浏览器录制器：Playwright 驱动真实 Chrome/Edge 窗口。
const { chromium } = require('playwright-core');
const fs = require('fs');
const path = require('path');

const out = (obj) => process.stdout.write('__NOVA__' + JSON.stringify(obj) + '\n');
const collectorSource = fs.readFileSync(path.join(__dirname, 'browser_collector.js'), 'utf8');
let browser = null;
let page = null;
let recording = false;
let paused = false;
let collectorReady = false;
// 最近一次可能触发导航的页面操作；用于排除点击、提交、页面输入回车造成的导航。
let pendingNavigationAction = null;
let pendingNavigationUntil = 0;
// 主文档请求信息，用 redirectedFrom 排除后续 HTTP 重定向，只记录地址栏首次导航。
let mainDocumentRequest = null;
// 用户在地址栏输入网址后，站点可能继续用 location/SPA 路由跳转；短时间内均视为同一导航链，
// 只记录链首，避免把登录重定向误记为新的地址栏 goto。
let addressNavigationChainUntil = 0;
const storageStatePath = process.env.NOVA_BROWSER_STORAGE_STATE || '';
let context = null;
let saveStateTimer = null;

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
  if (!page) return;
  await page.evaluate(([on, isPaused]) => {
    window.__novaRecording = on;
    window.__novaRecordingPaused = isPaused;
  }, [recording, paused]);
}

async function ensureBrowser() {
  if (page && !page.isClosed()) return;
  let lastError = null;
  for (const channel of ['chrome', 'msedge']) {
    try {
      browser = await chromium.launch({
        channel,
        headless: false,
        args: ['--start-maximized'],
      });
      context = await browser.newContext({
        viewport: null,
        ...(storageStatePath && fs.existsSync(storageStatePath) ? { storageState: storageStatePath } : {}),
      });
      await context.exposeFunction('__novaPush', (event) => {
        const now = Date.now();
        if (
          event.kind === 'click' ||
          event.kind === 'submit' ||
          (event.kind === 'key' && event.data?.key === 'Enter')
        ) {
          pendingNavigationAction = {
            kind: event.kind,
            selector: event.target?.selector || '',
          };
          pendingNavigationUntil = now + 2500;
        }
        out({ type: 'event', ts: now, ...event });
        scheduleStorageStateSave();
      });
      await context.exposeFunction('__novaCollectorReady', ({ url }) => {
        collectorReady = true;
        out({ type: 'collectorReady', url });
      });
      await context.addInitScript({ content: collectorSource });
      page = await context.newPage();
      page.on('request', (request) => {
        if (request.frame() !== page.mainFrame() || request.resourceType() !== 'document') return;
        mainDocumentRequest = {
          url: request.url(),
          redirected: Boolean(request.redirectedFrom()),
        };
      });
      page.on('framenavigated', async (frame) => {
        if (frame !== page.mainFrame()) return;
        collectorReady = false;
        const url = page.url();
        // 只记录用户直接在浏览器地址栏输入网址产生的首次主文档导航：
        // - 页面点击/提交/输入框回车后的导航不记录；
        // - HTTP 重定向不记录；
        // - SPA/脚本导航没有地址栏输入证据，也不记录。
        const now = Date.now();
        const operationNavigation = now <= pendingNavigationUntil;
        const requestInfo = mainDocumentRequest?.url === url ? mainDocumentRequest : null;
        const inAddressNavigationChain = now <= addressNavigationChainUntil;
        const directAddressNavigation = !operationNavigation && !inAddressNavigationChain && requestInfo && !requestInfo.redirected;
        if (directAddressNavigation) addressNavigationChainUntil = now + 10000;
        pendingNavigationUntil = 0;
        mainDocumentRequest = null;
        out({ type: 'nav', url, navigationSource: directAddressNavigation ? 'address_bar' : 'operation' });
        if (recording && url && url !== 'about:blank' && directAddressNavigation) {
          out({
            type: 'event',
            ts: now,
            kind: 'navigate',
            url,
            target: null,
            data: { navigationSource: 'address_bar', trigger: null },
          });
        }
        await page.waitForLoadState('domcontentloaded').catch(() => {});
        await syncFlags().catch(() => {});
        scheduleStorageStateSave();
      });
      page.on('close', () => {
        out({ type: 'closed' });
        process.exit(0);
      });
      return;
    } catch (error) {
      lastError = error;
      if (browser) await browser.close().catch(() => {});
      browser = null;
      page = null;
    }
  }
  throw new Error('未找到 Chrome 或 Edge: ' + String(lastError?.message || lastError));
}

const commands = {
  async navigate(command) {
    await ensureBrowser();
    let url = String(command.url || '').trim();
    if (url && !/^https?:\/\//i.test(url)) url = 'https://' + url;
    try {
      await page.goto(url || 'about:blank', { waitUntil: 'domcontentloaded', timeout: 30000 });
      await syncFlags();
      out({ type: 'nav', url: page.url() });
    } catch (error) {
      out({ type: 'nav', url: page.url(), error: String(error?.message || error) });
    }
  },
  async startRecord() {
    await ensureBrowser();
    recording = true;
    paused = false;
    await syncFlags();
    out({ type: 'recording', on: true, collectorReady });
  },
  async stopRecord() {
    recording = false;
    paused = false;
    await syncFlags();
    await saveStorageState();
    out({ type: 'recording', on: false, collectorReady });
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
    await ensureBrowser();
    const clip = await page.evaluate(() => new Promise((resolve) => {
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
    const image = await page.screenshot({ clip });
    out({ type: 'shot', reqId: command.reqId, data: 'data:image/png;base64,' + image.toString('base64'), clip });
  },
  async close() {
    await saveStorageState().catch(() => {});
    if (browser) await browser.close().catch(() => {});
    process.exit(0);
  },
};

const readline = require('readline');
readline.createInterface({ input: process.stdin }).on('line', (line) => {
  let command;
  try {
    command = JSON.parse(line);
  } catch {
    return;
  }
  const handler = commands[command.cmd];
  if (handler) Promise.resolve(handler(command)).catch((error) => {
    out({ type: 'error', error: String(error?.message || error), cmd: command.cmd });
  });
});
out({ type: 'hello' });
