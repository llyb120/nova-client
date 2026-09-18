import { writeFile, rm, access } from 'node:fs/promises';
import { chromium } from 'playwright-core';
import { build, createServer } from 'vite';

/** Real application modules, only native IPC is supplied by the fixture. Offline
 * mode uses an in-memory page and bundled modules, without changing browser policy. */
export async function browserFixture(source, name = 'history', viewport = { width: 1280, height: 900 }) {
  const basename = `${name}-fixture-${process.pid}`;
  let server, browser;
  await writeFile(`${basename}.html`, `<div id="root"></div><script type="module" src="/${basename}.tsx"></script>`);
  await writeFile(`${basename}.tsx`, source);
  const close = async () => {
    await browser?.close(); await server?.close();
    await Promise.all([`${basename}.html`, `${basename}.tsx`].map(path => rm(path, { force: true })));
  };
  try {
    let executablePath;
    for (const path of [process.env.TEST_BROWSER, '/usr/bin/google-chrome', '/usr/bin/chromium', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)) {
      try { await access(path); executablePath = path; break; } catch { /* try next installed browser */ }
    }
    if (!executablePath) throw new Error('Set TEST_BROWSER to an installed Chrome/Edge executable');
    browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
    const page = await browser.newPage({ viewport });
    const errors = []; page.on('pageerror', error => errors.push(String(error.stack || error)));
    page.setDefaultTimeout(20000);
    // Native event subscribers can run at module initialization, before the
    // fixture installs its detailed IPC dispatcher.
    const nativeBootstrap = () => { window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }; window.__TAURI_INTERNALS__ = { transformCallback: () => 1, invoke: async () => 1,
      convertFileSrc: path => path, metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } } }; };
    await page.addInitScript(nativeBootstrap);

    if (process.env.TEST_OFFLINE === '1') {
      // An opaque offline page has no persistent storage; the test's storage is
      // intentionally isolated, just as a fresh temporary profile would be.
      await page.setContent('<div id="root"></div>');
      await page.evaluate(nativeBootstrap);
      await page.evaluate(() => {
        for (const name of ['localStorage', 'sessionStorage']) {
          const data = new Map();
          Object.defineProperty(window, name, { configurable: true, value: {
            getItem: key => data.get(String(key)) ?? null, setItem: (key, value) => data.set(String(key), String(value)),
            removeItem: key => data.delete(String(key)), clear: () => data.clear(),
          } });
        }
      });
      const results = await build({ logLevel: 'error', build: { write: false, minify: false, cssCodeSplit: false,
        rollupOptions: { input: `${basename}.html`, output: { inlineDynamicImports: true } } } });
      const outputs = (Array.isArray(results) ? results : [results]).flatMap(result => result.output);
      for (const asset of outputs.filter(o => o.type === 'asset' && o.fileName.endsWith('.css')))
        await page.addStyleTag({ content: String(asset.source) });
      for (const chunk of outputs.filter(o => o.type === 'chunk')) await page.addScriptTag({ type: 'module', content: chunk.code });
    } else {
      server = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } });
      await server.listen();
      const address = server.httpServer.address();
      await page.goto(`http://127.0.0.1:${address.port}/${basename}.html`, { waitUntil: 'domcontentloaded' });
    }
    return { page, errors, close };
  } catch (error) { await close(); throw error; }
}
