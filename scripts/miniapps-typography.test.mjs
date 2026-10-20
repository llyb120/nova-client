import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { chromium } from 'playwright-core';

const css = await readFile(new URL('../src/components/MiniAppsView.css', import.meta.url), 'utf8');
const theme = await readFile(new URL('../src/app.css', import.meta.url), 'utf8');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1080, height: 800 } });
  await page.setContent(`<style>${theme}\n${css}</style>
    <main class="mini-studio">
      <header class="mini-topbar"><div class="mini-brand"><strong>观星台</strong><small>每一颗星，都是灵光一闪</small></div><span class="mini-chip">星海 v1</span></header>
      <div class="mini-workbench">
        <aside class="mini-library"><div class="mini-section-label">我的应用</div><button class="mini-new-app">＋ 新应用</button><div class="mini-app-meta"><strong>构建助手</strong><small>星海 v1 · 有改动</small></div></aside>
        <section class="mini-canvas"><div class="mini-canvas-toolbar"><div class="mini-segment"><button class="active">设计</button><button>试用草稿</button></div></div>
          <div class="mini-preview-scroll"><article class="mini-preview">
            <header class="mini-app-heading"><div><small>构建助手</small><h1>构建助手</h1><p>选择构建环境和分支，确认后由 Nova 触发构建。</p></div></header>
            <div class="mini-blocks">
              <div class="mini-block"><label class="mini-field">构建环境<select><option>请选择环境</option></select></label></div>
              <div class="mini-block"><button class="mini-action">读取远端分支列表</button></div>
              <div class="mini-block"><label class="mini-field">搜索分支<input placeholder="输入关键字过滤分支"></label></div>
              <div class="mini-block"><div class="mini-result"><header><h3>远端分支列表（支持搜索过滤）</h3></header><div class="empty">执行结果会显示在这里</div></div></div>
              <div class="mini-block" id="custom" style="--mini-font-size:18px"><label class="mini-field"><input value="保留自定义字号"></label></div>
            </div>
          </article></div>
        </section>
        <aside class="mini-copilot"><div class="mini-side-tabs"><button class="active">设计会话</button><button>版本</button></div><div class="mini-chat">
          <div class="mini-conversation"><div class="mini-assistant-intro"><strong>从一句话开始。</strong><p>描述整个应用，或按住 Ctrl / ⌘ 点选多个控件，一起调整界面和行为。</p></div><div class="mini-message"><small>Nova</small><p>已更新构建助手，可以继续修改或切到试用。</p></div></div>
          <div class="mini-composer"><textarea placeholder="例如：做一个日报助手，有首页和编辑页"></textarea><div><span>Enter 发送 · Shift+Enter 换行</span><button class="mini-primary">修改应用 ↑</button></div></div>
          <p class="mini-model-note">使用编码场景模型 · 草稿保存在本机</p>
        </div></aside>
      </div>
    </main>`);
  const bodyText = ['.mini-app-heading p', '.mini-field', '.mini-field input', '.mini-field select', '.mini-action', '.mini-result h3', '.mini-result .empty', '.mini-assistant-intro p', '.mini-message p', '.mini-composer textarea'];
  const hints = ['.mini-brand small', '.mini-chip', '.mini-app-meta small', '.mini-message small', '.mini-composer > div > span', '.mini-model-note'];
  for (const name of ['ink-dark', 'ink-light']) {
    await page.evaluate(name => document.documentElement.dataset.theme = name, name);
    for (const [selectors, minimum] of [[bodyText, 14], [hints, 12]]) {
      for (const selector of selectors) {
        const size = await page.locator(selector).first().evaluate(el => parseFloat(getComputedStyle(el).fontSize));
        assert.ok(size >= minimum, `${name} ${selector}: ${size}px < ${minimum}px`);
      }
    }
    for (const selector of ['.mini-app-heading p', '.mini-result .empty', '.mini-assistant-intro p', '.mini-message p', '.mini-model-note', '.mini-field input', '.mini-composer textarea']) {
      const contrast = await page.locator(selector).first().evaluate(el => {
        const rgba = value => value.match(/[\d.]+/g).map(Number);
        let background = [255, 255, 255];
        const ancestors = [];
        for (let node = el; node; node = node.parentElement) ancestors.unshift(node);
        for (const node of ancestors) {
          const [r, g, b, a = 1] = rgba(getComputedStyle(node).backgroundColor);
          background = [r, g, b].map((channel, i) => channel * a + background[i] * (1 - a));
        }
        const style = getComputedStyle(el, el.matches('input, textarea') ? '::placeholder' : null);
        const foreground = rgba(style.color).slice(0, 3);
        const luminance = color => color.map(v => v / 255).map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4).reduce((sum, v, i) => sum + v * [.2126, .7152, .0722][i], 0);
        const [a, b] = [luminance(background), luminance(foreground)].sort((a, b) => b - a);
        return (a + .05) / (b + .05);
      });
      assert.ok(contrast >= 4.5, `${name} ${selector}: contrast ${contrast.toFixed(2)} < 4.5`);
    }
    assert.equal(await page.locator('#custom input').evaluate(el => getComputedStyle(el).fontSize), '18px');
    await page.locator('#custom').evaluate(el => el.style.setProperty('--mini-font-size', '10px'));
    assert.equal(await page.locator('#custom input').evaluate(el => getComputedStyle(el).fontSize), '10px');
    await page.locator('#custom').evaluate(el => el.style.setProperty('--mini-font-size', '18px'));
    if (process.env.MINIAPP_TYPOGRAPHY_SCREENSHOT) {
      await page.screenshot({ path: `${process.env.MINIAPP_TYPOGRAPHY_SCREENSHOT}-${name}.png` });
    }
  }
  console.log('Mini-app typography: readable defaults, contrast and custom sizes passed in both themes.');
} finally {
  await browser.close();
}
