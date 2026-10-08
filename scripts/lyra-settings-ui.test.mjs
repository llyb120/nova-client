import assert from "node:assert/strict";
import { access, mkdir, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

// Exercise the real Solid form with an in-memory config at the IPC boundary.
const name = `lyra-settings-check-${process.pid}`;
const screenshots = "tmp/lyra-settings";
let server, browser;
try {
  await mkdir(screenshots, { recursive: true });
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { api } from './src/ipc';
import { LyraConfigPanel } from './src/components/LyraConfigPanel';
import '@fontsource-variable/inter';
import '@fontsource-variable/noto-sans-sc';
import './src/app.css';
let config = {
  $schema: 'keep-this-schema', model: 'primary/gpt-5',
  provider: {
    primary: {
      name: '日常模型', preset: 'commandcode',
      options: { apiKey: 'sk-example', headers: { 'X-Project': 'nova' }, temperature: 0.7 },
      models: {
        'gpt-5': { name: 'GPT-5', limit: { context: 128000, output: 32000 }, variants: { high: { reasoningEffort: 'high' } } },
        'claude-sonnet': { name: 'Claude Sonnet' },
      },
    },
    'local-codex': { name: '本地 Codex', preset: 'local-codex', models: { codex: {} } },
  },
};
const calls = { saved: [], fetched: [], refresh: 0 };
api.getLyraConfig = async () => structuredClone(config);
api.getLyraPresets = async () => [
  { id: 'commandcode', name: 'Command Code', baseURL: 'https://api.commandcode.ai/provider/v1' },
  { id: 'local-codex', name: '本地 Codex', baseURL: '', local: true },
  { id: 'openai-compatible', name: 'OpenAI 兼容', baseURL: '' },
];
api.saveLyraConfig = async next => { config = structuredClone(next); calls.saved.push(config); };
api.refreshLyraConfig = async () => { calls.refresh++; };
api.fetchLyraModels = async (id, provider) => {
  calls.fetched.push({ id, provider });
  return Object.fromEntries(Array.from({ length: 18 }, (_, i) => ['fetched-' + i, { name: '自动获取模型' + i + '-' + 'long-model-name-'.repeat(5) }]));
};
window.testCalls = () => calls;
render(() => <div class="modal-backdrop"><div class="modal settings-modal">
  <div class="modal-head">设置</div>
  <div class="settings-tabs"><button class="settings-tab active">Lyra</button></div>
  <div class="modal-body"><LyraConfigPanel onSaver={save => { window.testSave = save; }} /></div>
  <div class="modal-foot"><button class="btn primary" onClick={() => window.testSave()}>保存</button></div>
</div></div>, document.getElementById('root')!);
`);
  const port = 15000 + process.pid % 1000;
  server = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, NO_COLOR: "1" },
  });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(Error("Vite startup timed out")), 20000);
    server.stdout.on("data", data => { if (data.toString().includes("Local:")) { clearTimeout(timeout); resolve(); } });
    server.on("error", error => { clearTimeout(timeout); reject(error); });
    server.on("exit", code => { clearTimeout(timeout); reject(Error("Vite exited: " + code)); });
  });
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Google/Chrome/Application/chrome.exe"].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch {}
  }
  assert.ok(executablePath, "Set TEST_BROWSER to a Chromium executable");
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 1120, height: 980 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    window.__TAURI_INTERNALS__ = { invoke: async command => {
      if (command === 'plugin:dialog|message') return 'Ok';
      throw Error('Unexpected IPC: ' + command);
    } };
  });
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil: "domcontentloaded", timeout: 30000 });
  const providers = page.locator("details.lyra-provider");
  const primary = providers.first();
  await primary.waitFor();
  assert.equal(await providers.count(), 2);
  assert.equal(await primary.evaluate(el => el.open), true, "first provider is immediately usable");
  assert.equal(await providers.nth(1).evaluate(el => el.open), false, "other providers stay compact");
  assert.equal(await primary.locator("details.lyra-model").first().evaluate(el => el.open), false);
  const advanced = primary.locator("details.lyra-advanced").filter({ has: page.getByLabel("代理", { exact: true }) });
  assert.equal(await advanced.evaluate(el => el.open), false, "advanced fields start collapsed");
  await page.evaluate(() => document.fonts.ready);
  for (const theme of ["ink-dark", "ink-light"]) {
    await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
    await page.screenshot({ path: `${screenshots}/${theme}.png`, fullPage: true, animations: "disabled" });
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = "ink-dark"; });
  await primary.locator("section.lyra-models").evaluate(el => el.scrollIntoView({ block: "start" }));
  await page.screenshot({ path: `${screenshots}/ink-dark-models.png`, fullPage: true, animations: "disabled" });

  await primary.getByLabel("API Key", { exact: true }).fill("sk-edited");
  await advanced.locator(":scope > summary").press("Enter");
  await advanced.getByLabel("代理", { exact: true }).fill("http://127.0.0.1:7890");
  await advanced.locator(":scope > summary").click();
  await primary.locator(":scope > summary").click();
  await primary.locator(":scope > summary").press("Enter");
  assert.equal(await primary.getByLabel("API Key", { exact: true }).inputValue(), "sk-edited");
  await advanced.locator(":scope > summary").click();
  assert.equal(await advanced.getByLabel("代理", { exact: true }).inputValue(), "http://127.0.0.1:7890");
  await advanced.locator(":scope > summary").click();
  const firstModel = primary.locator("details.lyra-model").first();
  await firstModel.locator(":scope > summary").click();
  await firstModel.getByLabel("上下文窗口", { exact: true }).fill("256000");
  await firstModel.locator(":scope > summary").click();
  await primary.getByRole("button", { name: "获取模型", exact: true }).click();
  await page.locator(".lyra-default-model select").selectOption("primary/fetched-0");
  assert.equal(await page.evaluate(() => window.testCalls().fetched[0].provider.options.apiKey), "sk-edited");

  await primary.getByRole("button", { name: "添加模型", exact: true }).click();
  const addedModel = primary.locator("details.lyra-model").last();
  assert.equal(await addedModel.evaluate(el => el.open), true, "new model opens for editing");
  await addedModel.getByLabel("模型 ID", { exact: true }).fill("temporary-model");
  await addedModel.getByRole("button", { name: "删除模型", exact: true }).click();
  assert.equal(await primary.locator("details.lyra-model").count(), 2);
  await page.getByLabel("添加 Provider", { exact: true }).selectOption("local-codex");
  const addedProvider = providers.last();
  assert.equal(await addedProvider.evaluate(el => el.open), true, "new provider opens for editing");
  assert.equal(await addedProvider.getByLabel("API Key", { exact: true }).count(), 0, "local credentials need no key");
  await addedProvider.getByRole("combobox").first().selectOption("");
  assert.equal(await addedProvider.locator("details.lyra-advanced").filter({ has: page.getByLabel("ID", { exact: true }) }).evaluate(el => el.open), true, "switching to manual exposes required protocol settings");
  await addedProvider.getByRole("button", { name: "删除 Provider", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll("details.lyra-provider").length === 2);
  await page.getByLabel("添加 Provider", { exact: true }).selectOption("manual");
  const manual = providers.last();
  const manualAdvanced = manual.locator("details.lyra-advanced").filter({ has: page.getByLabel("ID", { exact: true }) });
  assert.equal(await manualAdvanced.evaluate(el => el.open), true, "new manual provider exposes its required ID");
  await manualAdvanced.getByLabel("ID", { exact: true }).fill("manual-test");
  assert.equal(await manualAdvanced.evaluate(el => el.open), true, "editing required ID does not collapse advanced fields");
  await manual.getByRole("button", { name: "删除 Provider", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll("details.lyra-provider").length === 2);

  await page.getByRole("button", { name: "保存", exact: true }).click();
  await page.waitForFunction(() => window.testCalls().saved.length === 1);
  const saved = await page.evaluate(() => window.testCalls().saved[0]);
  assert.equal(saved.$schema, "keep-this-schema");
  assert.equal(saved.model, "primary/fetched-0");
  assert.deepEqual(saved.provider.primary.options, { apiKey: "sk-edited", proxy: "http://127.0.0.1:7890", headers: { "X-Project": "nova" }, temperature: 0.7 });
  assert.equal(saved.provider.primary.models["gpt-5"].limit.context, 256000);
  assert.deepEqual(saved.provider.primary.models["gpt-5"].variants, { high: { reasoningEffort: "high" } });
  assert.deepEqual(Object.keys(saved.provider), ["primary", "local-codex"]);
  await page.evaluate(() => window.testSave());
  assert.equal(await page.evaluate(() => window.testCalls().saved.length), 1, "unchanged forms do not save again");

  await page.setViewportSize({ width: 420, height: 900 });
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "ink-light";
    document.querySelectorAll('.lyra-config details').forEach(el => { el.open = true; });
    document.querySelector('.modal-body').scrollTop = 0;
  });
  const overflow = await page.evaluate(() => [...document.querySelectorAll('.modal-body, .lyra-config, .lyra-config details, .lyra-config section')]
    .filter(el => el.clientWidth && el.scrollWidth > el.clientWidth + 1)
    .map(el => ({ class: el.className, client: el.clientWidth, scroll: el.scrollWidth })));
  assert.deepEqual(overflow, [], "expanded form does not overflow on narrow windows");
  await page.screenshot({ path: `${screenshots}/narrow-expanded.png`, fullPage: true, animations: "disabled" });
  await page.locator("details.lyra-file-settings").getByRole("button", { name: "从文件重新加载", exact: true }).click();
  await page.waitForFunction(() => window.testCalls().refresh === 1);
  assert.deepEqual(errors, []);
  console.log("Lyra settings: hierarchy, keyboard collapse, edit persistence, model fetch, add/delete, serialization and narrow layout passed");
  console.log(`Screenshots: ${screenshots}`);
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
