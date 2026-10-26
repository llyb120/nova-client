// Real Solid DOM + native image layout; private headless Chromium, no app/IPC/GUI.
import assert from 'node:assert/strict';
import { access, readFile, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';
import { build, transform } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from 'babel-preset-solid';
import { chromium } from 'playwright-core';

const origin = 'https://image-fixture.test';
const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600"><rect width="800" height="600" fill="green"/></svg>';
const bundle = await build({
  stdin: { resolveDir: process.cwd(), loader: 'tsx', contents: `
    import { createSignal } from 'solid-js';
    import { render } from 'solid-js/web';
    import { HtmlTurn } from './src/components/HtmlTurn';
    import { HtmlTranscript } from './src/components/HtmlTranscript';
    import { warmImages } from './src/components/ImageAttachmentStrip';
    const data = ${JSON.stringify(Buffer.from(svg).toString('base64'))};
    const inline = { name: 'image.svg', mimeType: 'image/svg+xml', data, size: 100 };
    const uri = name => ({ ...inline, data: undefined, uri: 'file:///' + name + '.svg' });
    const requests = [];
    const NativeImage = Image;
    window.Image = class extends NativeImage {
      decode() {
        const decoded = super.decode();
        decoded.catch(() => {});
        return new Promise((resolve, reject) => {
          requests.push({ src: this.src, ready: () => resolve(decoded), fail: () => reject(new Error('delayed failure')) });
        });
      }
    };
    const warm = new NativeImage();
    warm.src = 'data:image/svg+xml;base64,' + data;
    await warm.decode();
    warmImages.set(warm.src, warm);
    const [group, setGroup] = createSignal({ user: { id: 1, type: 'user', text: '', ts: 0, images: [inline] }, body: [] });
    const disposeTurn = render(() => HtmlTurn({ get group() { return group(); }, threadId: 'thread', running: false, preview: false, onReturnToCurrent() {} }), document.getElementById('root'));
    window.test = { inline, uri, requests,
      replace(image, id = 1) { setGroup(g => ({ ...g, user: { ...g.user, id, images: [image] } })); },
      image: () => document.querySelector('.ht-bubble-images img'),
      mountTranscript() {
        disposeTurn(); window.Image = NativeImage;
        document.getElementById('root').style.height = '260px';
        const [session, setSession] = createSignal({ id: 'empty', groups: [] });
        let handle;
        render(() => HtmlTranscript({ get groups() { return session().groups; }, get threadId() { return session().id; },
          permissions: [], running: false, loading: false, preview: false, emptyHint: 'empty',
          ref: value => { handle = value; }, onReturnToCurrent() {} }), document.getElementById('root'));
        window.gate = {
          frames: [],
          switch(id, image) {
            this.frames = [];
            setSession({ id, groups: Array.from({ length: 8 }, (_,i) => ({ user: {
              id: i + 1, type: 'user', text: id + '-text-' + i, ts: 0,
              images: i === 7 && image ? [uri(image)] : [],
            }, body: [] })) });
          },
          sample() {
            const host = document.querySelector('.html-transcript');
            const space = host.querySelector('.html-transcript-space');
            const img = space.querySelector('img');
            const row = img?.closest('.html-transcript-row');
            return { id: session().id, visible: getComputedStyle(space).visibility !== 'hidden',
              loading: !!host.querySelector('[role=status]'), imageHeight: img?.getBoundingClientRect().height,
              textTop: row?.querySelector('.ht-user-text').getBoundingClientRect().top,
              spaceHeight: space.getBoundingClientRect().height,
              rowBottom: row ? row.getBoundingClientRect().bottom - space.getBoundingClientRect().top : null,
              top: handle.scrollTop(), max: handle.maxScrollTop() };
          },
        };
        const frame = () => { if (gate.frames.length < 300) gate.frames.push(gate.sample()); requestAnimationFrame(frame); };
        requestAnimationFrame(frame);
      },
    };`, },
  bundle: true, write: false, format: 'esm', platform: 'browser', outdir: '/tmp/html-turn-image-bundle',
  plugins: [{ name: 'isolated-turn', setup(build) {
    build.onResolve({ filter: /(?:^|\/)(store|ipc|Markdown|FileContextMenu|ImageAttachmentStrip|workspaceLinks)$/ }, args => ({ path: args.path.split('/').at(-1), namespace: 'mock' }));
    build.onLoad({ filter: /.*/, namespace: 'mock' }, args => ({ loader: 'js', contents: {
      store: `export const state = { expanded: {}, currentId: 'thread', cwd: '/project' };
        export const isExpanded = (key, fallback = false) => fallback;
        export const toggleExpanded = () => {}; export const editUserMessage = async () => {};
        export const optimisticHandoffs = new Map(); export const traceThreadSwitchLayoutDone = () => {}; `,
      ipc: `export const fileUriPath = uri => uri.replace('file://', '');`,
      Markdown: `export const Markdown = () => '';`,
      FileContextMenu: `export const createFileContextMenu = () => ({ open(e, path) { e.preventDefault(); window.menuPath = path; }, Menu: () => '' });`,
      workspaceLinks: `export const openWorkspaceFile = path => { window.openedPath = path; };`,
      ImageAttachmentStrip: `export const warmImages = new Map();
        export const attachmentPreviewSrc = image => image.data ? 'data:' + image.mimeType + ';base64,' + image.data : image.uri.replace('file://', '${origin}');
        export const takeWarmImage = src => { const value = warmImages.get(src); warmImages.delete(src); return value; };
        export const ImageAttachmentStrip = () => '';
        export const createImageAttachments = () => ({ images: () => [], clear() {}, set() {}, remove() {}, onPaste() {}, pickFiles: async () => {} });`,
    }[args.path] }));
    build.onLoad({ filter: /\.tsx$/ }, async args => {
      const stripped = await transform(await readFile(args.path, 'utf8'), { loader: 'tsx', jsx: 'preserve' });
      const output = await transformAsync(stripped.code, { filename: args.path, babelrc: false, configFile: false, presets: [[solid, { generate: 'dom', hydratable: false }]] });
      return { contents: output.code, loader: 'js' };
    });
  } }],
});
const cache = process.env.PLAYWRIGHT_BROWSERS_PATH || path.join(homedir(), '.cache/ms-playwright');
const shells = (await readdir(cache).catch(() => [])).filter(name => /^chromium_headless_shell-\d+$/.test(name)).sort().reverse();
const candidates = [process.env.TEST_BROWSER, chromium.executablePath(), '/usr/bin/chromium', '/usr/bin/chromium-browser',
  ...shells.map(name => path.join(cache, name, 'chrome-headless-shell-linux64/chrome-headless-shell'))].filter(Boolean);
let executablePath;
for (const candidate of candidates) { try { await access(candidate); executablePath = candidate; break; } catch {} }
assert.ok(executablePath, 'No Chromium installed: set TEST_BROWSER');
const browser = await chromium.launch({ headless: true, executablePath, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  let releaseCold;
  const coldResponse = new Promise(resolve => { releaseCold = resolve; });
  const held = new Map();
  const hold = name => {
    let release;
    held.set('/' + name + '.svg', new Promise(resolve => { release = resolve; }));
    return release;
  };
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) return route.abort();
    if (url.pathname === '/bundle.js') return route.fulfill({ contentType: 'text/javascript', body: bundle.outputFiles.find(f => f.path.endsWith('.js')).text });
    if (url.pathname === '/cold.svg') await coldResponse;
    if (held.has(url.pathname)) await held.get(url.pathname);
    if (url.pathname.endsWith('.svg')) return route.fulfill({ status: url.pathname.includes('broken') ? 404 : 200, contentType: 'image/svg+xml', body: url.pathname.includes('broken') ? 'not an image' : svg });
    return route.fulfill({ contentType: 'text/html', body: `<style>${bundle.outputFiles.find(f => f.path.endsWith('.css')).text}</style><div id="root"></div><script type="module" src="/bundle.js"></script>` });
  });
  await page.goto(origin);
  await page.waitForFunction(() => window.test?.image()?.naturalWidth > 0);
  const size = () => page.evaluate(() => {
    const img = test.image(), rect = img.getBoundingClientRect();
    return { width: rect.width, height: rect.height, widthAttr: img.width, heightAttr: img.height };
  });
  const initial = await size();
  assert.equal(initial.width, 240);
  assert.equal(initial.height, 180);
  await page.evaluate(() => { window.firstImage = test.image(); test.replace(test.uri('persisted')); });
  assert.ok(await page.evaluate(() => test.image() === firstImage && test.image().src.startsWith('data:')));
  assert.deepEqual(await size(), initial, 'pending persistence must not collapse the image');
  assert.equal(await page.evaluate(() => {
    test.image().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    return window.openedPath;
  }), '/persisted.svg', 'actions use the new attachment while pixels still use the old source');
  await page.evaluate(() => { test.replace({ ...test.uri('persisted') }); test.requests.at(-1).ready(); });
  await page.waitForFunction(() => test.image().src.endsWith('/persisted.svg'));
  assert.deepEqual(await size(), initial);
  assert.ok(await page.evaluate(() => test.image() === firstImage), 'object replacement must retain the actual DOM node');

  await page.evaluate(() => { test.replace(test.inline); test.requests.at(-1).ready(); });
  await page.waitForFunction(() => test.image().getAttribute('width') === '800');
  await page.evaluate(() => { test.replace(test.uri('broken-projection')); test.requests.at(-1).fail(); });
  assert.ok(await page.evaluate(() => test.image().src.startsWith('data:') && !test.image().classList.contains('ht-image-broken')));
  assert.deepEqual(await size(), initial, 'failed projection retains usable pixels and layout');

  await page.evaluate(() => {
    test.replace(test.uri('slow')); window.stale = test.requests.at(-1);
    test.replace({ ...test.uri('cold'), name: 'new.svg' }, 2);
  });
  assert.equal(await page.evaluate(() => test.image().getAttribute('src')), origin + '/cold.svg');
  assert.equal(await page.evaluate(() => test.image().getAttribute('width')), null, 'wrong identity must not borrow old dimensions');
  assert.equal(await page.evaluate(() => test.image().alt), 'new.svg');
  await page.waitForFunction(() => test.image().naturalWidth === 0);
  assert.equal(await page.evaluate(() => test.image().currentSrc), '', 'a cold replacement must not paint the previous attachment while loading');
  releaseCold();
  await page.evaluate(() => stale.fail());
  assert.ok(await page.evaluate(() => !test.image().classList.contains('ht-image-broken')));
  await page.evaluate(() => test.requests.at(-1).ready());
  await page.waitForFunction(() => test.image().getAttribute('width') === '800');

  await page.evaluate(() => test.replace({ ...test.uri('broken-new'), name: 'broken.svg' }, 3));
  await page.waitForFunction(() => test.image().classList.contains('ht-image-broken'));
  await page.evaluate(() => {
    test.requests.at(-1).fail();
    test.replace({ ...test.uri('recovered'), name: 'recovered.svg' }, 4);
    const img = test.image();
    // A queued old resource event must not mark the newly selected attachment broken.
    Object.defineProperties(img, { currentSrc: { configurable: true, value: 'https://image-fixture.test/broken-new.svg' }, complete: { configurable: true, value: true }, naturalWidth: { configurable: true, value: 0 } });
    img.dispatchEvent(new Event('error'));
    delete img.currentSrc; delete img.complete; delete img.naturalWidth;
  });
  assert.ok(await page.evaluate(() => !test.image().classList.contains('ht-image-broken') && test.image().alt === 'recovered.svg'));
  await page.evaluate(() => test.requests.at(-1).ready());
  await page.waitForFunction(() => test.image().naturalWidth === 800 && test.image().getAttribute('width') === '800');
  console.log('HtmlTurn image DOM checks passed (stable slot/size, latest actions, decode failure, identity reset, cold load, stale errors).');

  const frames = async (count = 5) => page.evaluate(async count => {
    for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame);
    return gate.frames;
  }, count);
  const assertHidden = samples => {
    assert.ok(samples.length >= 3, 'sample multiple pending paint frames');
    assert.ok(samples.every(s => !s.visible && s.loading), JSON.stringify(samples));
  };
  const assertReady = samples => {
    const visible = samples.filter(s => s.visible);
    assert.ok(visible.length >= 3, 'sample first visible frame and subsequent frames');
    assert.ok(visible.every(s => s.imageHeight === 180 && !s.loading), JSON.stringify(visible));
    assert.ok(visible.every(s => Math.abs(s.spaceHeight - s.rowBottom - 16) <= 1), 'first visible space already includes decoded row height');
    assert.ok(visible.every(s => Math.abs(s.top - s.max) <= 1), 'initial bottom anchor is correct before reveal');
    assert.ok(visible.every(s => Math.abs(s.textTop - visible[0].textTop) <= 1), 'no text jump after first visible frame');
  };
  const releaseInitial = hold('gate-initial');
  await page.evaluate(() => { test.mountTranscript(); gate.switch('initial', 'gate-initial'); });
  assertHidden(await frames());
  releaseInitial();
  await page.waitForFunction(() => gate.sample().visible);
  assertReady(await frames());

  const releaseOld = hold('gate-old'), releaseNew = hold('gate-new');
  await page.evaluate(() => gate.switch('old', 'gate-old'));
  assertHidden(await frames());
  await page.evaluate(() => gate.switch('new', 'gate-new'));
  assertHidden(await frames());
  releaseOld();
  await page.waitForTimeout(100);
  assertHidden(await frames());
  releaseNew();
  await page.waitForFunction(() => gate.sample().visible);
  assertReady(await frames());

  const releaseBroken = hold('gate-broken');
  await page.evaluate(() => gate.switch('broken', 'gate-broken'));
  assertHidden(await frames());
  releaseBroken();
  await page.waitForFunction(() => gate.sample().visible);
  const failed = await frames();
  assert.ok(failed.filter(s => s.visible).every(s => !s.loading));
  assert.ok(await page.locator('.ht-image-broken').count(), 'a failed image releases the gate and displays its fallback');
  assert.deepEqual(errors, []);
  console.log('HtmlTranscript cold image gate checks passed (hidden pending frames, correct first-visible geometry/anchor, stale session completion, broken image release).');
} finally {
  await browser.close();
}
