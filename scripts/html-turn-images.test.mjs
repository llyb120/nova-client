// Real Solid DOM + native layout; private headless Chromium, no app/IPC/GUI.
// Run just the bounded toolbar regression: node scripts/html-turn-images.test.mjs --user-actions-only
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
          get running() { return session().mode === 'running'; }, get readOnly() { return session().mode === 'readOnly'; },
          permissions: [], loading: false, preview: false, emptyHint: 'empty',
          ref: value => { handle = value; }, onReturnToCurrent() {} }), document.getElementById('root'));
        window.gate = {
          frames: [],
          prompt(text, mode) {
            setSession({ id: 'thread', mode, groups: [{ user: { id: 1, type: 'user', text, ts: 0 }, body: [] }] });
          },
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
  const frames = async (count = 5) => page.evaluate(async count => {
    for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame);
    return gate.frames;
  }, count);
  if (!process.argv.includes('--user-actions-only')) {
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
  } else await page.evaluate(() => test.mountTranscript());

  // Reuse the real transcript/turn and production reset/theme/gutters, not a toolbar replica.
  await page.addStyleTag({ content: await readFile('src/app.css', 'utf8') });
  page.setDefaultTimeout(5000);
  await page.evaluate(() => {
    document.getElementById('root').style.height = '560px';
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
      writeText: async text => { window.copiedText = text; },
    } });
  });
  const wrapped = 'This deliberately wrapped prompt keeps its full text and spacing while copy stays beside edit, outside the bubble. '.repeat(3) + '\n  中文 👋\tend  \n';
  const prompts = { short: '  Copy 👋  ', wrapped, collapsed: wrapped.repeat(15) + '\nfull-text tail 👋  ' };
  const user = page.locator('.ht-user');
  const snapshot = () => user.evaluate(user => {
    const rect = node => node.getBoundingClientRect().toJSON();
    const row = user.closest('.html-transcript-row'), host = row.closest('.html-transcript');
    const padding = getComputedStyle(row), text = user.querySelector('.ht-user-text');
    return {
      bubble: rect(user.querySelector('.ht-user-bubble')), text: text.textContent,
      textSize: [text.clientWidth, text.clientHeight], lineHeight: parseFloat(getComputedStyle(text).lineHeight),
      content: { left: rect(row).left + parseFloat(padding.paddingLeft), right: rect(row).right - parseFloat(padding.paddingRight) },
      host: { left: rect(host).left, right: rect(host).left + host.clientWidth },
      buttons: [...user.querySelectorAll('button')].filter(button => button.getBoundingClientRect().width > 0)
        .map(button => ({ ...rect(button), label: button.getAttribute('aria-label'), toolbar: !!button.closest('.ht-user-actions') })),
      opacity: getComputedStyle(user.querySelector('.ht-user-actions')).opacity,
      hovered: user.matches(':hover'), focused: !!user.querySelector(':focus-visible'),
    };
  });
  const assertLayout = (s, label) => {
    for (const b of s.buttons) {
      assert.ok(b.width > 0 && b.height > 0, `${label}: nonempty button`);
      assert.ok(b.left >= s.content.left - 1 && b.right <= s.content.right + 1, `${label}: button inside transcript content`);
      assert.ok(b.left >= s.host.left && b.right <= s.host.right, `${label}: button not clipped by transcript`);
      if (b.toolbar) assert.ok(b.right <= s.bubble.left - 3, `${label}: toolbar must be outside bubble, not over text`);
      else assert.ok(b.top >= s.bubble.bottom - 1, `${label}: long-prompt controls remain below bubble`);
    }
    for (let i = 0; i < s.buttons.length; i++) for (const b of s.buttons.slice(i + 1)) {
      const a = s.buttons[i];
      assert.ok(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top, `${label}: buttons must not overlap`);
    }
  };
  const artifact = process.env.HTML_USER_ACTIONS_SCREENSHOT || '/tmp/html-user-actions-320.png';
  for (const width of [800, 320]) for (const [kind, text] of Object.entries(prompts)) for (const mode of ['normal', 'readOnly', 'running']) {
    const label = `${width}px/${kind}/${mode}`, collapsed = kind === 'collapsed';
    await page.mouse.move(0, 0);
    // Narrow chat in a desktop viewport retains the real desktop transcript gutters.
    await page.evaluate(({ width, text, mode }) => {
      document.getElementById('root').style.width = width + 'px';
      document.activeElement?.blur(); window.copiedText = null; gate.prompt(text, mode);
    }, { width, text, mode });
    assert.equal(await page.locator('.html-transcript').evaluate(host => host.getBoundingClientRect().width), width);
    await frames(2);
    const before = await snapshot();
    const expectedText = collapsed ? text.slice(0, 1000).replace(/[\uD800-\uDBFF]$/, '') + '…' : text;
    assert.equal(before.text, expectedText, `${label}: rendered prompt is unchanged`);
    assert.equal(before.opacity, '0', `${label}: toolbar hidden until hover/focus`);
    assert.deepEqual(before.buttons.filter(b => b.toolbar).map(b => b.label), [
      ...(!collapsed ? ['复制完整提示词'] : []), ...(mode === 'normal' ? ['原样重发此消息', '编辑消息'] : []),
    ], `${label}: edit policy and separate long-prompt copy`);
    if (kind !== 'short') assert.ok(before.textSize[1] > before.lineHeight * 2, `${label}: fixture actually wraps`);
    assertLayout(before, label);

    await page.locator('.ht-user-bubble').hover();
    const hovered = await snapshot();
    assert.equal(hovered.opacity, '1', `${label}: hover reveals toolbar`);
    assertLayout(hovered, label);
    const copy = page.locator(collapsed ? '.ht-copy-prompt' : '.ht-user-actions > button').first();
    assert.equal(await page.locator('.ht-copy-prompt').count(), collapsed ? 1 : 0);
    await page.mouse.move(0, 0);
    await page.locator('.html-transcript').focus();
    // Tab through actual focus order, including resend/edit/fold for a collapsed prompt.
    for (let i = 0; i < (collapsed ? before.buttons.length : 1); i++) await page.keyboard.press('Tab');
    assert.ok(await copy.evaluate(button => button === document.activeElement), `${label}: copy is keyboard reachable`);
    const focused = await snapshot();
    assert.ok(!focused.hovered && focused.focused, `${label}: focus-visible without hover`);
    assert.equal(focused.opacity, '1', `${label}: keyboard focus reveals toolbar`);
    assertLayout(focused, label);
    if (width === 320 && kind === 'wrapped' && mode === 'normal') await page.locator('#root').screenshot({ path: artifact });

    await copy.click();
    assert.equal(await page.evaluate(() => window.copiedText), text, `${label}: clipboard receives exact full text, not preview`);
    const after = await snapshot();
    assert.equal(after.text, before.text, `${label}: copy never mutates prompt`);
    assert.deepEqual(after.textSize, before.textSize, `${label}: copy feedback does not reflow text`);
    assertLayout(after, label);
  }
  assert.deepEqual(errors, []);
  console.log(`HtmlTurn user action checks passed (18 cases: wide/320px, short/wrapped/collapsed, normal/readOnly/running, hover/keyboard/copy). Screenshot: ${artifact}`);
} finally {
  await browser.close();
}
