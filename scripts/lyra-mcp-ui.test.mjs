import assert from "node:assert/strict";
import { access, readFile, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

// Exercise the real Solid editor with in-memory IPC; never touch the user's config.
const settings = await readFile("src/components/SettingsModal.tsx", "utf8");
assert.match(settings.match(/const LYRA_TABS[^]*?\n\];/)[0], /id: "mcp", name: "MCP"/);
assert.match(settings, /<LyraConfigPanel\s+section=\{tab\(\) === "mcp"/);
const name = `lyra-mcp-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { createSignal } from 'solid-js';
import { render } from '@solidjs/web';
import { api } from './src/ipc';
import { LyraConfigPanel } from './src/components/LyraConfigPanel';
import './src/app.css';
let config = {
  model: 'main/test', extra: { keep: true },
  provider: { main: { api: 'openai-responses', options: { baseURL: 'https://example.test/v1' }, models: { test: {} } } },
  mcpServers: {
    local: { command: 'node', args: ['a b'], extra: { keep: true } },
    off: { url: 'http://10.0.0.1/mcp', disabled: true }
  }
};
const saved = [];
api.getLyraConfig = async () => structuredClone(config);
api.getLyraPresets = async () => [];
api.saveLyraConfig = async next => { config = structuredClone(next); saved.push(config); };
api.refreshLyraConfig = async () => {};
window.testSaved = () => saved;
window.testReplaceMcp = value => { config.mcpServers = value; };
const [section, setSection] = createSignal('providers');
render(() => <div class="modal-backdrop"><div class="modal settings-modal">
  <div class="modal-head">设置</div>
  <div class="modal-body">
    <div role="tablist" aria-label="Lyra 设置">
      <button role="tab" onClick={() => setSection('providers')}>模型服务</button>
      <button role="tab" onClick={() => setSection('mcp')}>MCP</button>
    </div>
    <LyraConfigPanel section={section()} onSaver={save => { window.testSave = save; }} />
  </div>
</div></div>, document.getElementById('root')!);
`);
  const port = 16000 + process.pid % 1000;
  server = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, NO_COLOR: "1" },
  });
  await new Promise((resolve, reject) => {
    let output = "";
    const timeout = setTimeout(() => reject(Error(`Vite startup timed out: ${output}`)), 30000);
    server.stdout.on("data", data => { output += data.toString(); if (output.includes("Local:")) { clearTimeout(timeout); resolve(); } });
    server.stderr.on("data", data => { output += data.toString(); });
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
    window.testConfirm = true;
    window.__TAURI_INTERNALS__ = { invoke: async command => {
      if (command === 'plugin:dialog|message') return window.testConfirm ? 'Ok' : 'Cancel';
      throw Error('Unexpected IPC: ' + command);
    } };
  });
  const baseUrl = `http://127.0.0.1:${port}`;
  const moduleResponse = await fetch(`${baseUrl}/src/components/LyraConfigPanel.tsx`, { signal: AbortSignal.timeout(30000) });
  assert.equal(moduleResponse.status, 200, `Vite could not transform the editor: ${await moduleResponse.text()}`);
  await page.goto(`${baseUrl}/${name}.html`, { waitUntil: "commit", timeout: 30000 });
  await page.locator(".lyra-providers-head").waitFor({ timeout: 30000 });
  await page.getByRole("tab", { name: "MCP", exact: true }).click();
  const mcp = page.locator(".lyra-mcp");
  const rows = mcp.locator(".lyra-mcp-server");
  const row = name => rows.filter({ has: page.locator("strong", { hasText: new RegExp(`^${name}$`) }) });
  const save = () => page.evaluate(async () => { try { await window.testSave(); return ""; } catch (e) { return String(e); } });
  const saved = async () => page.evaluate(() => window.testSaved().at(-1));
  assert.equal(await rows.count(), 2);
  assert.equal(await row("local").getByRole("button", { name: "停用 MCP 服务器 local" }).getAttribute("aria-pressed"), "true");
  assert.equal(await row("off").getByRole("button", { name: "启用 MCP 服务器 off" }).getAttribute("aria-pressed"), "false");

  // Add a whole single-server wrapper without an explicit name or enabled flag.
  await mcp.getByRole("button", { name: "添加 MCP 服务器", exact: true }).click();
  const remote = JSON.stringify({ mcpServers: { remote: { url: "http://example.test/mcp", headers: { Authorization: "Bearer test" }, custom: null } } }, null, 2);
  await rows.first().locator("textarea").fill(remote);
  assert.equal(await row("remote").count(), 1);
  assert.equal(await mcp.getByRole("status").innerText(), "");
  await row("remote").getByRole("button", { name: "停用 MCP 服务器 remote" }).click();
  assert.equal(await row("remote").getByRole("button", { name: "启用 MCP 服务器 remote" }).getAttribute("aria-pressed"), "false");
  await row("off").getByRole("button", { name: "启用 MCP 服务器 off" }).click();

  // A duplicate or invalid JSON must not overwrite the existing configuration.
  await row("remote").locator("input").fill("local");
  assert.match(await save(), /重复/);
  await rows.first().locator("input").fill("");
  await row("remote").locator("textarea").fill("{");
  assert.match(await save(), /有效.*JSON/);
  assert.equal(await page.evaluate(() => window.testSaved().length), 0);
  await rows.first().locator("textarea").fill(remote);
  await row("remote").getByRole("button", { name: "停用 MCP 服务器 remote" }).click();

  await page.getByRole("tab", { name: "模型服务", exact: true }).click();
  await page.locator(".lyra-provider").first().getByLabel("显示名称").first().fill("模型草稿");
  await page.getByRole("tab", { name: "MCP", exact: true }).click();
  assert.equal(await save(), "");
  const config = await saved();
  assert.deepEqual(config.mcpServers, {
    remote: { url: "http://example.test/mcp", headers: { Authorization: "Bearer test" }, custom: null, enabled: false },
    local: { command: "node", args: ["a b"], extra: { keep: true } },
    off: { url: "http://10.0.0.1/mcp", disabled: false, enabled: true },
  });
  assert.equal(config.provider.main.name, "模型草稿");
  assert.deepEqual(config.extra, { keep: true });

  await page.evaluate(() => { window.testConfirm = false; });
  await row("remote").getByRole("button", { name: "删除 MCP 服务器 remote" }).click();
  assert.equal(await rows.count(), 3);
  await page.evaluate(() => { window.testConfirm = true; });
  await row("remote").getByRole("button", { name: "删除 MCP 服务器 remote" }).click();
  await page.waitForFunction(() => document.querySelectorAll('.lyra-mcp-server').length === 2);
  assert.equal(await save(), "");
  assert.deepEqual(Object.keys((await saved()).mcpServers), ["local", "off"]);

  await page.setViewportSize({ width: 420, height: 900 });
  const overflow = await page.evaluate(() => [...document.querySelectorAll('.lyra-mcp, .lyra-mcp-server')]
    .filter(el => el.clientWidth && el.scrollWidth > el.clientWidth + 1).map(el => el.className));
  assert.deepEqual(overflow, [], "MCP editors fit narrow windows");
  assert.deepEqual(errors, []);
  console.log("Per-server MCP JSON, default and explicit enable, HTTP, save and delete passed");
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
