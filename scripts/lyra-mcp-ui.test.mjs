import assert from "node:assert/strict";
import { access, readFile, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

// Test the real Solid editor with in-memory IPC; never touch the user's config.
const settings = await readFile("src/components/SettingsModal.tsx", "utf8");
assert.match(settings.match(/const LYRA_TABS[^]*?\n\];/)[0], /id: "mcp", name: "MCP"/);
assert.match(settings, /<LyraConfigPanel section=\{tab\(\) === "mcp"/);
const name = `lyra-mcp-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import { api } from './src/ipc';
import { LyraConfigPanel } from './src/components/LyraConfigPanel';
import './src/app.css';
let config = {
  model: 'main/test', extra: { keep: true },
  provider: { main: { api: 'openai-responses', options: { baseURL: 'https://example.test/v1' }, models: { test: {} } } },
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
    const timeout = setTimeout(() => reject(Error("Vite startup timed out")), 20000);
    server.stdout.on("data", data => { if (data.toString().includes("Local:")) { clearTimeout(timeout); resolve(); } });
    server.stderr.on("data", () => {});
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
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil: "domcontentloaded" });
  await page.locator(".lyra-providers-head").waitFor();
  assert.equal(await page.locator(".lyra-config [role=tablist]").count(), 0, "no nested MCP tabs");
  await page.getByRole("tab", { name: "MCP", exact: true }).click();
  const mcp = page.locator(".lyra-mcp");
  const editor = mcp.locator(".lyra-mcp-editor");
  const save = () => page.evaluate(async () => { try { await window.testSave(); return ""; } catch (e) { return String(e); } });
  const saved = async () => JSON.parse(await page.evaluate(() => JSON.stringify(window.testSaved().at(-1))));
  const rows = mcp.locator(".lyra-mcp-server");
  const row = name => rows.filter({ has: page.locator("strong", { hasText: new RegExp(`^${name}$`) }) });
  const add = () => mcp.getByRole("button", { name: "添加 MCP 服务器", exact: true }).click();
  const finish = async () => {
    assert.equal(await editor.getByRole("status").innerText(), "", "valid form enables completion");
    await editor.getByRole("button", { name: "完成编辑", exact: true }).click();
  };
  assert.equal(await mcp.getByLabel("mcpServers JSON").count(), 0, "JSON is opt-in");

  await add();
  assert.equal(await editor.getByLabel("启用此服务器").isChecked(), false);
  await editor.getByLabel("服务器名称").fill("local");
  await editor.getByLabel("启动命令").fill("C:/Program Files/MCP/server.exe");
  for (const arg of ["--project", "a b", ""]) {
    await editor.getByRole("button", { name: "添加参数", exact: true }).click();
    await editor.getByLabel(/^参数 \d+$/).last().fill(arg);
  }
  const env = editor.locator(".field").filter({ has: page.locator(".field-label", { hasText: /^环境变量$/ }) });
  await env.getByRole("button", { name: "添加", exact: true }).click();
  await env.getByLabel("环境变量名称").fill("TOKEN");
  await env.getByLabel("环境变量值").fill("{env:MCP_TOKEN}");
  await editor.getByLabel("工作目录（可选）").fill("C:/project a");
  await page.getByRole("tab", { name: "模型服务", exact: true }).click();
  await page.locator(".lyra-provider").first().getByLabel("显示名称").first().fill("模型草稿");
  await page.getByRole("tab", { name: "MCP", exact: true }).click();
  assert.equal(await editor.getByLabel("服务器名称").inputValue(), "local");
  assert.equal(await save(), "", "global save commits the unfinished single-server form");
  assert.deepEqual((await saved()).mcpServers.local, { enabled: false, command: "C:/Program Files/MCP/server.exe", args: ["--project", "a b", ""], cwd: "C:/project a", env: { TOKEN: "{env:MCP_TOKEN}" } });
  assert.equal((await saved()).provider.main.name, "模型草稿");
  assert.deepEqual((await saved()).extra, { keep: true });

  await add();
  await editor.getByLabel("服务器名称").fill("local");
  await editor.getByLabel("启动命令").fill("other");
  assert.match(await save(), /已存在/);
  assert.equal(await editor.getByRole("button", { name: "完成编辑" }).isDisabled(), true);
  await editor.getByRole("button", { name: "取消编辑" }).click();
  await add();
  await editor.getByLabel("服务器名称").fill("remote");
  await editor.getByLabel("连接方式").selectOption("http");
  await editor.getByLabel("服务地址").fill("http://example.test/mcp");
  assert.match(await save(), /https/);
  await editor.getByLabel("服务地址").fill("https://example.test/mcp");
  const headers = editor.locator(".field").filter({ has: page.locator(".field-label", { hasText: /^请求头$/ }) });
  await headers.getByRole("button", { name: "添加", exact: true }).click();
  await headers.getByLabel("请求头名称").fill("Authorization");
  await headers.getByLabel("请求头值").fill("{env:MCP_AUTH}");
  await editor.getByLabel("启用此服务器").check();
  await editor.locator("details > summary").click();
  await editor.getByLabel("请求超时（毫秒）").fill("0");
  assert.match(await save(), /1–600000/);
  await editor.getByLabel("请求超时（毫秒）").fill("45000");
  await editor.getByLabel("允许只读会话调用的工具").fill("lookup\nsearch");
  await finish();
  assert.equal(await rows.count(), 2);
  assert.equal(await save(), "");
  assert.deepEqual((await saved()).mcpServers.remote, { enabled: true, url: "https://example.test/mcp", headers: { Authorization: "{env:MCP_AUTH}" }, request_timeout_ms: 45000, readOnlyTools: ["lookup", "search"] });

  await row("remote").getByRole("button", { name: "编辑" }).click();
  await editor.getByLabel("服务器名称").fill("renamed");
  await editor.getByLabel("连接方式").selectOption("stdio");
  await editor.getByLabel("启动命令").fill("node");
  await finish();
  assert.equal(await save(), "");
  const renamed = (await saved()).mcpServers;
  assert.equal(Object.hasOwn(renamed, "remote"), false);
  assert.equal(Object.hasOwn(renamed.renamed, "url"), false);
  assert.equal(Object.hasOwn(renamed.renamed, "headers"), false);
  assert.equal(renamed.renamed.command, "node");
  assert.deepEqual(renamed.renamed.readOnlyTools, ["lookup", "search"]);
  await page.evaluate(() => { window.testConfirm = false; });
  await row("renamed").getByRole("button", { name: "删除" }).click();
  assert.equal(await rows.count(), 2);
  await page.evaluate(() => { window.testConfirm = true; });
  await row("renamed").getByRole("button", { name: "删除" }).click();
  await page.waitForFunction(() => document.querySelectorAll('.lyra-mcp-server').length === 1);
  assert.equal(await save(), "");
  assert.deepEqual(Object.keys((await saved()).mcpServers), ["local"]);

  await mcp.getByRole("button", { name: "高级：编辑 JSON" }).click();
  const json = mcp.getByLabel("mcpServers JSON");
  const before = await json.inputValue();
  await json.fill("{");
  assert.equal(await mcp.getByRole("button", { name: "返回服务器列表" }).isDisabled(), true);
  assert.match(await save(), /有效 JSON/);
  await json.fill(before);
  await mcp.getByRole("button", { name: "返回服务器列表" }).click();
  assert.equal(await rows.count(), 1);
  await row("local").getByRole("button", { name: "编辑" }).click();
  assert.equal(await editor.getByLabel("环境变量值").inputValue(), "{env:MCP_TOKEN}");
  await editor.getByRole("button", { name: "取消编辑" }).click();
  await mcp.getByRole("button", { name: "高级：编辑 JSON" }).click();
  await json.fill("{}");
  await mcp.getByRole("button", { name: "返回服务器列表" }).click();
  assert.equal(await rows.count(), 0);
  assert.equal(await save(), "");
  assert.deepEqual((await saved()).mcpServers, {});

  await page.evaluate(() => { window.testReplaceMcp({ broken: { command: 'node' } }); });
  await page.locator("details.lyra-file-settings > summary").click();
  await page.getByRole("button", { name: "从文件重新加载" }).click();
  await json.waitFor();
  assert.deepEqual(JSON.parse(await json.inputValue()), { broken: { command: "node" } }, "invalid imported config is not silently erased");
  await json.fill('{"__proto__":{"enabled":false,"command":"node"}}');
  await mcp.getByRole("button", { name: "返回服务器列表" }).click();
  await row("__proto__").getByRole("button", { name: "编辑" }).click();
  await editor.getByLabel("启动命令").fill("npx");
  assert.equal(await save(), "");
  assert.equal((await saved()).mcpServers.__proto__.command, "npx");

  await row("__proto__").getByRole("button", { name: "编辑" }).click();
  await page.setViewportSize({ width: 420, height: 900 });
  const overflow = await page.evaluate(() => [...document.querySelectorAll('.lyra-mcp, .lyra-mcp section, .lyra-mcp-server')]
    .filter(el => el.clientWidth && el.scrollWidth > el.clientWidth + 1).map(el => el.className));
  assert.deepEqual(overflow, [], "MCP editor fits narrow windows");
  assert.deepEqual(errors, []);
  console.log("MCP top-level section, form CRUD, shared drafts, validation, JSON round-trip and narrow layout passed");
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
