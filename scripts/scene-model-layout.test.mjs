import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { chromium } from "playwright-core";

const css = readFileSync(new URL("../src/app.css", import.meta.url), "utf8");
// Mirror the scene rows and ModelPicker's closed SearchSelect DOM; use the real stylesheet.
const picker = (label) => `<div class="sel wide"><button class="pill">
  <span class="pill-text">模型：${label}</span><svg width="12" height="12"></svg>
</button></div>`;
const number = '<span style="width:28px;flex-shrink:0">1</span>';
const browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL });
try {
  const page = await browser.newPage();
  await page.setContent(`<style>${css}</style><div class="modal-backdrop">
    <div class="modal settings-modal"><div class="modal-body"><section class="settings-group">
      <div class="field"><div class="share-list">
        <div class="share-row stage-model-row">${number}${picker("长模型名称 / provider-".repeat(24))}
          <button class="icon-btn" aria-label="上移">↑</button>
          <button class="icon-btn share-row-remove" aria-label="移除该模型"><svg width="14" height="14"></svg></button>
        </div>
        <div class="share-row stage-model-row">${number}${picker("默认模型")}
          <button class="icon-btn" aria-label="上移" disabled>↑</button>
          <button class="icon-btn share-row-remove" aria-label="移除该模型"><svg width="14" height="14"></svg></button>
        </div>
        <div class="share-row share-row-add stage-model-row">${number}${picker("选择并添加模型")}
          <span class="share-row-plus"><svg width="13" height="13"></svg></span>
        </div>
        <div class="share-row"><div class="proj-picker"><button class="pill"><span class="pill-text">共享项目</span></button></div>
          <button class="icon-btn share-row-remove"><svg width="14" height="14"></svg></button>
        </div>
      </div></div>
    </section></div></div>
  </div>`);

  for (const width of [1280, 800, 480, 375, 320]) {
    await page.setViewportSize({ width, height: 900 });
    const rows = await page.locator(".share-row").evaluateAll((elements) => elements.map((row) => {
      const bounds = row.getBoundingClientRect();
      const rect = (node) => {
        const r = node.getBoundingClientRect();
        return { left: r.left, right: r.right, centerY: r.top + r.height / 2, width: r.width };
      };
      const label = row.querySelector(".pill-text");
      return {
        bounds: { left: bounds.left, right: bounds.right, width: bounds.width },
        scrollWidth: row.scrollWidth,
        children: [...row.children].map(rect),
        trigger: rect(row.querySelector(".pill")),
        label: { width: label.clientWidth, scrollWidth: label.scrollWidth, ellipsis: getComputedStyle(label).textOverflow },
      };
    }));
    for (const [index, row] of rows.entries()) {
      const context = `viewport ${width}, row ${index}`;
      assert.ok(row.bounds.left >= 0 && row.bounds.right <= width, `${context}: row stays in viewport`);
      assert.ok(row.scrollWidth <= row.bounds.width + 1, `${context}: no horizontal overflow`);
      for (const [i, child] of row.children.entries()) {
        assert.ok(Math.abs(child.centerY - row.children[0].centerY) < 1, `${context}: all controls stay on one line`);
        assert.ok(child.left >= row.bounds.left - 1 && child.right <= row.bounds.right + 1, `${context}: controls stay inside row`);
        if (i) assert.ok(child.left >= row.children[i - 1].right, `${context}: controls do not overlap`);
      }
      const pickerIndex = index === 3 ? 0 : 1;
      assert.ok(row.trigger.width > 0 && row.trigger.right <= row.children[pickerIndex].right + 1, `${context}: picker fits its column`);
      assert.ok(row.children.at(-1).width >= 24 || index === 2, `${context}: action button does not shrink`);
      assert.ok(row.bounds.right - row.children.at(-1).right <= 3, `${context}: final action stays at the right edge`);
    }
    assert.ok(rows[0].label.scrollWidth > rows[0].label.width, `viewport ${width}: long name is constrained`);
    assert.equal(rows[0].label.ellipsis, "ellipsis");
  }
  console.log("Scene model layout checks passed (1280, 800, 480, 375, 320px)");
} finally {
  await browser.close();
}
