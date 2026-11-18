import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { transform } from "@solidjs/compiler";
import { build } from "esbuild";
import { chromium } from "playwright-core";
import ts from "typescript";

const panel = readFileSync(new URL("../src/components/UsageStatsPanel.tsx", import.meta.url), "utf8");
const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

test("project memory cache uses the complete prefix and keeps legacy totals separate", () => {
  assert.match(panel, /估算命中/);
  assert.match(panel, /包含系统提示、工具定义和项目记忆/);
  assert.match(panel, /旧版只统计记忆消息的数据已保留，但不能换算，不计入本卡/);
  assert.match(panel, /未返回缓存明细按零计算/);
  assert.match(panel, /按连续前缀/);
  const source = panel.match(/function memoryRate\(b: UsageBucket\): number \{[\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;
  const memoryRate = new Function(`${js}; return memoryRate;`)();
  assert.equal(memoryRate({ memory: 900, memoryCacheRead: 700, memoryPrefixTokens: 1000, memoryPrefixCacheRead: 800 }), 0.8);
  assert.equal(memoryRate({ memory: 900, memoryCacheRead: 700, memoryPrefixTokens: 0, memoryPrefixCacheRead: 0 }), 0);
});

test("estimated costs expose incomplete pricing and retain every model", () => {
  assert.match(panel, /legacyPricingPending/);
  assert.match(panel, /未定价/);
  assert.match(panel, /未完整返回用量/);
  assert.doesNotMatch(panel, /stats\(\)\?\.models\s*\?\?\s*\[\]\)\.slice/);
  const source = panel.match(/const cost = \(b: UsageBucket\) => [^;]+;/)?.[0];
  assert.ok(source);
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;
  const format = new Function(`${js}; return cost;`)();
  assert.equal(format({costUsd: 1.25, unpricedTokens: 0, unknownRequests: 0}), "$1.2500");
  assert.equal(format({costUsd: 0, unpricedTokens: 10, unknownRequests: 0}), "≥$0.00");
  assert.equal(format({costUsd: 0.000001, unpricedTokens: 0, unknownRequests: 0}), "$0.00000100");
});

test("range switches keep the cards mounted until the latest selection succeeds", async () => {
  const mocks = {
    "../ipc": "export const api = { usageStats: days => new Promise((resolve, reject) => requests.push({ days, resolve, reject })) };",
    "../store": "export const showToast = text => toasts.push(text);",
    "./TurnGroup": "export const fmtTokens = String;",
  };
  const { outputFiles } = await build({
    stdin: {
      contents: `import { render } from '@solidjs/web';
        import Panel from './src/components/UsageStatsPanel';
        window.requests = []; window.toasts = [];
        render(Panel, document.getElementById('root'));`,
      resolveDir: fileURLToPath(new URL("..", import.meta.url)),
    },
    bundle: true, write: false, format: "esm", platform: "browser", loader: { ".css": "empty" },
    plugins: [{ name: "usage-fixture", setup(builder) {
      builder.onResolve({ filter: /^(\.\.\/(ipc|store)|\.\/TurnGroup)$/ }, ({ path }) => ({ path, namespace: "mock" }));
      builder.onLoad({ filter: /.*/, namespace: "mock" }, ({ path }) => ({ contents: mocks[path] }));
      builder.onLoad({ filter: /UsageStatsPanel\.tsx$/ }, ({ path }) => ({
        contents: transform(panel, { filename: path }).code, loader: "ts",
      }));
    } }],
  });
  const executablePath = [process.env.TEST_BROWSER, chromium.executablePath(),
    `${process.env["ProgramFiles(x86)"]}/Microsoft/Edge/Application/msedge.exe`, "/usr/bin/chromium"]
    .find(path => path && existsSync(path));
  assert.ok(executablePath, "Set TEST_BROWSER to an installed Chromium");
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const page = await browser.newPage();
    const css = ["../src/app.css", "../src/components/UsageStatsPanel.css"]
      .map(path => readFileSync(new URL(path, import.meta.url), "utf8")).join("\n");
    await page.setContent(`<style>${css}</style><div class="settings-modal" id="root"></div>`);
    await page.addScriptTag({ type: "module", content: outputFiles[0].text });
    const tab = name => page.getByRole("tab", { name, exact: true });
    const cards = page.locator(".usage-cards");
    const totalIs = total => cards.getByText(String(total), { exact: true });
    const refresh = page.locator(".usage-refresh");
    const reply = (index, total) => page.evaluate(async ([index, total]) => {
      if (total === null) requests[index].reject(new Error("mock read failure"));
      else requests[index].resolve({ days: [], models: [], total: {
        total, input: total - 1, turns: 1, output: 1, cacheRead: 0, cacheWrite: 0,
        memoryPrefixTokens: 0, memoryPrefixCacheRead: 0, costUsd: 0, unpricedTokens: 0, unknownRequests: 0,
      } });
      await new Promise(requestAnimationFrame);
    }, [index, total]);
    await page.waitForFunction(() => window.requests?.length === 1);
    await reply(0, 101);
    await totalIs(101).waitFor();
    const original = await cards.elementHandle();
    const start = await cards.boundingBox();
    const refreshWidth = (await refresh.boundingBox()).width;

    await tab("7 天").click();
    await tab("14 天").click();
    assert.deepEqual(await page.evaluate(() => requests.map(r => r.days)), [1, 7, 14]);
    assert.equal(await totalIs(101).count(), 1, "pending requests must not hide the last completed stats");
    assert.deepEqual(await cards.boundingBox(), start);
    assert.equal((await refresh.boundingBox()).width, refreshWidth);
    assert.equal(await tab("今天").getAttribute("aria-selected"), "true");

    await reply(2, 303);
    await totalIs(303).waitFor();
    await reply(1, 202);
    assert.equal(await totalIs(303).count(), 1, "a slower response cannot replace the latest result");
    assert.equal(await tab("14 天").getAttribute("aria-selected"), "true");
    assert.equal(await cards.evaluate((node, initial) => node === initial, original), true);

    await tab("全部").click();
    await reply(3, null);
    const retry = page.getByRole("button", { name: "重试", exact: true });
    await retry.waitFor();
    assert.equal(await tab("14 天").getAttribute("aria-selected"), "true", "the numbers still belong to 14 days");
    assert.equal(await totalIs(303).count(), 1);
    assert.match(await page.evaluate(() => toasts[0]), /全部.*失败/);
    await retry.click();
    assert.deepEqual(await page.evaluate(() => requests.map(r => r.days)), [1, 7, 14, null, null]);
    await reply(4, 404);
    await totalIs(404).waitFor();
    assert.equal(await tab("全部").getAttribute("aria-selected"), "true");
  } finally {
    await browser.close();
  }
});

// 读累计值要等会话存储锁并遍历全部轮次，同步命令会卡住界面线程。
test("usage stats runs off the UI thread", () => {
  const decl = lib.indexOf("async fn usage_stats(");
  assert.ok(decl > 0, "usage_stats 必须是 async 命令（同步命令会在界面线程等存储锁）");
  const before = lib.slice(Math.max(0, decl - 400), decl);
  assert.ok(/#\[tauri::command\]\r?\n(?:\s*\/\/\/[^\n]*\r?\n)*\s*$/.test(before), "紧挨命令宏，没有被改成别的属性");
});
