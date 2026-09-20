import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { browserFixture } from './lib/browser-fixture.mjs';
const source = (await readFile('scripts/fixtures/history-ui-fixture.tsx', 'utf8')).replaceAll("'../../src/", "'./src/");
const { page, errors, close } = await browserFixture(source, 'seamless-history');
const directory = process.env.TEST_REPORT_DIR || 'history-ui-results';
await mkdir(directory, { recursive: true });
const results = [];
async function check(name, action) {
  const start = Date.now();
  await action();
  results.push({ name, result: 'passed', elapsedMs: Date.now() - start });
  console.log('PASS ' + name);
}
try {
  await page.waitForFunction(() => !!window.perfTest);
  await check('normal wheel scrolling prefetches history without a pager, loading row or second gesture', async () => {
    await page.evaluate(() => window.perfTest.open('big'));
    await page.waitForFunction(() => window.perfTest.metrics().paints > 10);
    await page.waitForTimeout(300);
    assert.equal(await page.locator('.history-pager').count(), 0);
    const initial = await page.evaluate(() => window.perfTest.metadata().start);
    const rect = await page.locator('canvas.transcript-canvas-only').boundingBox();
    await page.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2);
    await page.evaluate(() => window.perfTest.gate());
    for (let i = 0; i < 20; i++) {
      await page.mouse.wheel(0, -300);
      await page.waitForTimeout(80);
      if (await page.evaluate(() => window.perfTest.state.historyLoading)) break;
    }
    await page.waitForFunction(() => window.perfTest.state.historyLoading);
    assert.equal(await page.getByText('正在读取历史片段…', { exact: true }).count(), 0);
    assert.equal(await page.getByRole('button', { name: '加载更早记录', exact: true }).count(), 0);
    assert.equal(await page.locator('textarea.composer-input').count(), 1);
    // Keep scrolling while the IPC is held, then reverse. The position saved at
    // request START must not be restored when the page finally arrives.
    await page.mouse.wheel(0, -300);
    await page.waitForTimeout(120);
    await page.mouse.wheel(0, 120);
    await page.waitForTimeout(120);
    const before = await page.evaluate(() => window.perfTest.visibleText().find(value => /prompt-|reply-/.test(value.text) && value.y > 20 && value.y < 600));
    assert.ok(before, 'A real message must be painted while the read is pending');
    await page.evaluate(() => window.perfTest.release());
    await page.waitForFunction(start => !window.perfTest.state.historyLoading && window.perfTest.metadata().start < start, initial);
    await page.waitForTimeout(400);
    const after = await page.evaluate(text => window.perfTest.visibleText().find(value => value.text === text), before.text);
    assert.ok(after, 'The same visible message must survive the page handover');
    assert.ok(Math.abs(after.y - before.y) < 3, JSON.stringify({ before, after }));
    assert.ok(await page.evaluate(() => window.perfTest.state.items.length <= 240));
  });
  await check('turn reset racing the user acknowledgement never renders one prompt twice', async () => {
    await page.evaluate(() => window.perfTest.open('small'));
    const prompt = 'single-copy-after-reset-race';
    await page.evaluate(() => window.perfTest.resetBeforeNextSend());
    await page.locator('textarea.composer-input').fill(prompt);
    await page.locator('.composer-btn.send').click();
    await page.waitForFunction(text => window.perfTest.state.items.some(item => item.id > 0 && item.text === text), prompt);
    await page.waitForTimeout(300);
    const result = await page.evaluate(text => {
      const ids = window.perfTest.state.items.filter(item => item.text === text).map(item => item.id);
      const canonicalId = ids.find(id => id > 0);
      return {
        matches: ids.length,
        optimistic: ids.filter(id => id < 0).length,
        ids,
        fetchedCanonicalId: window.perfTest.calls.some(call =>
          call.command === 'get_thread_display_items' && call.ids?.includes(canonicalId)),
      };
    }, prompt);
    assert.equal(result.matches, 1, JSON.stringify(result));
    assert.equal(result.optimistic, 0, JSON.stringify(result));
    assert.equal(result.fetchedCanonicalId, true, JSON.stringify(result));
    await page.evaluate(() => window.perfTest.run(false));
  });
  await check('resending invalidates an outstanding old page even when the thread and IDs are reused', async () => {
    await page.evaluate(() => window.perfTest.open('anchor'));
    await page.waitForTimeout(200);
    const target = await page.evaluate(() => window.perfTest.state.items.find(item => item.type === 'user').id);
    await page.evaluate(() => { window.perfTest.gate(); void window.perfTest.page('before'); });
    await page.waitForFunction(() => window.perfTest.state.historyLoading);
    await page.evaluate(id => window.perfTest.resend(id, 'resent-branch-unique-prompt'), target);
    await page.waitForFunction(() => window.perfTest.metadata()?.generation === 'g-anchor-resent');
    await page.evaluate(() => window.perfTest.release());
    await page.waitForTimeout(400);
    const result = await page.evaluate(() => ({
      generation: window.perfTest.metadata().generation,
      ids: window.perfTest.state.items.map(item => item.id),
      matches: window.perfTest.state.items.filter(item => item.text === 'resent-branch-unique-prompt').length,
      error: window.perfTest.state.historyError,
      calls: window.perfTest.calls.filter(call => call.command === 'truncate_thread').length,
    }));
    assert.equal(result.generation, 'g-anchor-resent');
    assert.equal(result.matches, 1);
    assert.equal(result.calls, 1);
    assert.equal(result.error, '');
    assert.ok(result.ids.every(id => id >= 0 && id <= target), JSON.stringify(result));
    await page.waitForFunction(() => window.perfTest.visibleText().some(value => value.text.includes('resent-branch-unique-prompt')));
    await page.evaluate(() => window.perfTest.run(false));
  });
  await check('the canvas itself retains the live anchor when the window changes without caller restoration', async () => {
    await page.evaluate(() => window.perfTest.canvas('big'));
    await page.waitForTimeout(250);
    await page.evaluate(() => window.perfTest.canvasJump(3));
    await page.waitForTimeout(200);
    const before = await page.evaluate(() => window.perfTest.canvasAnchor());
    assert.ok(before);
    await page.evaluate(() => window.perfTest.canvasPageWithoutRestore('before'));
    await page.waitForTimeout(400);
    const after = await page.evaluate(() => window.perfTest.canvasAnchor());
    assert.equal(after.itemId, before.itemId);
    assert.ok(Math.abs(after.offset - before.offset) < 3, JSON.stringify({ before, after }));
  });
  await check('same-thread branch replacement drops old layout, reveal, image and hit-test state', async () => {
    await page.evaluate(() => window.perfTest.canvas('small'));
    await page.waitForTimeout(250);
    await page.evaluate(() => window.perfTest.canvasReplace('same-id-new-branch-prompt'));
    await page.waitForFunction(() => window.perfTest.visibleText().some(value => value.text.includes('same-id-new-branch-prompt')));
    await page.waitForFunction(() => window.perfTest.visibleText().some(value => value.text.includes('replacement-answer')));
    await page.waitForTimeout(200);
    const visible = await page.evaluate(() => window.perfTest.visibleText());
    assert.ok(!visible.some(value => /prompt-\d|reply-\d/.test(value.text)), JSON.stringify(visible));
    const stats = await page.evaluate(() => window.perfTest.canvasStats());
    assert.equal(stats.entries, 0, 'Old image previews must not survive the branch change');
    assert.ok(stats.top >= 0 && stats.top <= stats.max);
  });
  assert.deepEqual(errors, []);
  await page.screenshot({ path: `${directory}/seamless-history.png` });
} catch (error) {
  results.push({ name: 'failure', error: String(error.stack || error), browserErrors: errors });
  console.error(await page.evaluate(() => ({ history: window.perfTest?.metadata(), calls: window.perfTest?.calls.slice(-20), visible: window.perfTest?.visibleText(), body: document.body.innerText.slice(-1500) })));
  await page.screenshot({ path: `${directory}/seamless-failure.png` }).catch(() => {});
  throw error;
} finally {
  await writeFile(`${directory}/seamless-report.json`, JSON.stringify({ results, errors }, null, 2));
  await close();
}
