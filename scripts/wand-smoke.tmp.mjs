import { chromium } from 'playwright-core';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const errors = [];
for (const bg of ['#f4f6f8', '#1f2430']) {
  const page = await browser.newPage({ viewport: { width: 280, height: 280 }, deviceScaleFactor: 1 });
  page.on('pageerror', e => errors.push(e.message));
  await page.goto('file:///' + process.cwd().replace(/\\/g, '/') + '/public/drag-wand.html');
  await page.evaluate(bg => { document.body.style.background = bg; }, bg);
  // Sweep along an arc toward the upper right, ~900px/s, the way a drag swings out of the panel.
  for (let i = 0; i <= 18; i++) {
    await page.evaluate(([x, y]) => wand.at(x, y), [1000 + i * 14, 800 - i * 9 + Math.round(Math.sin(i / 3) * 6)]);
    await page.waitForTimeout(16);
  }
  await page.screenshot({ path: `tmp/meteor-${bg.slice(1)}.png` });
}
console.log(JSON.stringify({ errors }));
await browser.close();
