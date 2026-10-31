import assert from "node:assert/strict";
import { access, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer } from "vite";

// Real Solid component and CSS, with every IPC intercepted before the component loads.
const root = fileURLToPath(new URL("../", import.meta.url));
const name = `meteor-ui-check-${process.pid}`;
const entryFiles = [new URL(`../${name}.html`, import.meta.url)];
const viewport = { width: 1000, height: 720 };
let server, browser;
try {
  await writeFile(entryFiles[0], `<div id="root"></div><script type="module" src="/src/index.tsx"></script>`);
  server = await createServer({ root, optimizeDeps: { noDiscovery: true, include: ['solid-js', '@solidjs/web'] },
    cacheDir: `node_modules/.vite-${name}`, server: { host: "127.0.0.1", port: 15000 + process.pid % 1000, strictPort: false, hmr: false }, logLevel: "error" });
  await server.listen();
  const address = server.httpServer.address();
  assert.ok(address && typeof address !== "string");
  const origin = `http://127.0.0.1:${address.port}`;
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Google/Chrome/Application/chrome.exe", "/usr/bin/chromium", "/usr/bin/google-chrome"].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch {}
  }
  assert.ok(executablePath, "Set TEST_BROWSER to a Chromium executable");
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport, deviceScaleFactor: 2 });
  page.setDefaultTimeout(10000);
  const errors = [];
  const blockedRequests = [];
  const ipcCalls = [];
  const requests = [];
  page.on("request", request => requests.push(request.url()));
  page.on("pageerror", error => errors.push(error.message));
  await page.exposeFunction("recordMeteorIpc", command => ipcCalls.push(command));
  await page.route("**/*", async route => {
    const url = route.request().url();
    if (url.startsWith(`${origin}/`) || url.startsWith("data:")) await route.continue();
    else { blockedRequests.push(url); await route.abort(); }
  });
  await page.addInitScript(() => {
    const calls = { commands: [], submitted: [], cancelled: 0, cancelIds: [] };
    const callbacks = new Map();
    const listeners = new Map();
    let callbackId = 0, pending, contextReply, cancelReply;
    const scenario = new URLSearchParams(location.search).get("scenario");
    const displays = scenario === "horizontal" ? {
      monitors: [{ x: 0, y: 0, width: 1000, height: 1440 }, { x: 1000, y: 240, width: 1000, height: 1200 }], pointer: { x: 1500, y: 600 },
    } : scenario === "vertical" ? {
      monitors: [{ x: 0, y: 0, width: 1800, height: 600 }, { x: 200, y: 600, width: 1800, height: 840 }], pointer: { x: 1500, y: 1000 },
    } : {};
    const context = { captureId: "mock-capture-id", width: 2000, height: 1440, ...displays };
    window.meteorTest = {
      calls, delayCancel: false,
      emitOpen(overrides = {}) {
        for (const [id, item] of listeners) if (item.event === "meteor:open") {
          callbacks.get(item.handler)({ event: "meteor:open", id, payload: { ...context, ...overrides } });
        }
      },
      finishContext(value) { contextReply(value); },
      finishCancel(error) { if (error) cancelReply.reject(new Error(error)); else cancelReply.resolve(); },
      finishSubmission(error) {
        if (!pending) throw new Error("No submission is pending");
        const current = pending;
        pending = undefined;
        if (error) current.reject(new Error(error));
        else current.resolve("mock-thread-id");
      },
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: (_event, id) => listeners.delete(id) };
    window.__TAURI_INTERNALS__ = {
      transformCallback(handler) { const id = ++callbackId; callbacks.set(id, handler); return id; },
      invoke: async (command, args) => {
        calls.commands.push(command);
        await window.recordMeteorIpc(command);
        if (command === "plugin:event|listen") {
          if (args.event !== "meteor:open") throw new Error(`Unexpected listener: ${args.event}`);
          listeners.set(args.handler, args);
          return args.handler;
        }
        if (command === "plugin:event|unlisten") { listeners.delete(args.eventId); return; }
        if (command === "meteor_context") {
          if (scenario === "context-error") throw new Error("模拟屏幕布局 IPC 失败");
          if (scenario === "prewarm") return null;
          if (scenario === "delayed-context") return new Promise(resolve => { contextReply = resolve; });
          return { ...context, ...(scenario === "geometry-error" ? { width: 0 } : {}) };
        }
        if (command === "meteor_submit") {
          calls.submitted.push(structuredClone(args));
          return new Promise((resolve, reject) => { pending = { resolve, reject }; });
        }
        if (command === "meteor_cancel") {
          calls.cancelled++;
          calls.cancelIds.push(args?.captureId);
          if (window.meteorTest.delayCancel) return new Promise((resolve, reject) => { cancelReply = { resolve, reject }; });
          return;
        }
        throw new Error(`Unexpected IPC (never forwarded): ${command}`);
      },
    };
  });
  const open = async (scenario = "ready") => {
    await page.goto(`${origin}/${name}.html?meteor=1&scenario=${scenario}`, { waitUntil: "domcontentloaded", timeout: 30000 });
    await page.waitForFunction(() => window.meteorTest.calls.commands.includes("meteor_context"));
    if (!["context-error", "geometry-error", "prewarm", "delayed-context"].includes(scenario)) {
      await page.waitForFunction(() => document.querySelector(".meteor-surface--selectable"));
    }
  };
  const calls = () => page.evaluate(() => structuredClone(window.meteorTest.calls));
  const submittedCount = count => page.waitForFunction(value => window.meteorTest.calls.submitted.length === value, count);
  const textarea = page.locator("#meteor-request");
  const focused = () => page.waitForFunction(() => document.activeElement?.id === "meteor-request");
  const settleLayout = () => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const assertVisibleMark = async (locator, pseudo) => {
    const style = await locator.evaluate((element, pseudo) => {
      const style = getComputedStyle(element, pseudo);
      return { color: style.borderTopColor, width: parseFloat(style.borderTopWidth), shadow: style.boxShadow };
    }, pseudo);
    assert.equal(style.color, "rgb(255, 159, 28)", "draft, active and saved selections retain high-contrast orange");
    assert.ok(style.width >= 3);
    assert.ok(style.shadow.includes("rgb(17, 17, 17)") && style.shadow.includes("rgb(255, 255, 255)"), "black/white outlines remain visible on light and dark desktops");
  };

  await open();
  assert.deepEqual((await calls()).commands.slice(0, 2), ["plugin:event|listen", "meteor_context"], "subscribe before requesting the current geometry");
  assert.equal(await page.locator("img, .meteor-shade").count(), 0, "a live overlay has no screenshot or dimming layer");
  const transparency = await page.evaluate(() => {
    const styles = [document.documentElement, document.body, document.getElementById("root")].map(element => getComputedStyle(element));
    return { backgrounds: styles.map(style => style.backgroundColor), margins: styles.map(style => style.margin),
      overlay: getComputedStyle(document.querySelector(".meteor-overlay")).backgroundColor,
      surface: getComputedStyle(document.querySelector(".meteor-surface")).backgroundColor,
      pointerEvents: getComputedStyle(document.querySelector(".meteor-surface")).pointerEvents };
  });
  assert.deepEqual(transparency.backgrounds, Array(3).fill("rgba(0, 0, 0, 0)"));
  assert.deepEqual(transparency.margins, Array(3).fill("0px"));
  assert.equal(transparency.overlay, "rgba(0, 0, 0, 0)");
  assert.equal(transparency.surface, "rgba(0, 0, 0, 0.008)");
  assert.equal(transparency.pointerEvents, "auto");
  // Reverse drag on a 2x desktop geometry must normalize and submit physical pixels.
  await page.mouse.move(700, 500);
  await page.mouse.down();
  await page.mouse.move(300, 200, { steps: 5 });
  await assertVisibleMark(page.locator(".meteor-region"));
  await page.mouse.up();
  await textarea.waitFor();
  await assertVisibleMark(page.locator(".meteor-region.meteor-mark--active"));
  await focused();
  assert.equal(await page.locator(".meteor-note-actions span").textContent(), "已框选 800 × 600 px");
  assert.equal(await page.locator(".meteor-submit").isDisabled(), true);
  await textarea.fill("  \n \t ");
  await page.keyboard.press("Control+Enter");
  await page.locator(".meteor-error").waitFor();
  assert.equal((await calls()).submitted.length, 0, "empty/whitespace instructions never reach submit IPC");

  await textarea.fill("  检查选中表格  ");
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  assert.equal(await textarea.isDisabled(), true);
  assert.equal(await page.locator(".meteor-submit").isDisabled(), true);
  await page.keyboard.press("Control+Enter");
  await page.keyboard.press("Control+Enter");
  assert.equal((await calls()).submitted.length, 1, "busy Ctrl+Enter presses cannot dispatch duplicate work");
  assert.deepEqual((await calls()).submitted[0], {
    captureId: "mock-capture-id", annotations: [{ text: "检查选中表格", region: { x: 600, y: 400, width: 800, height: 600 } }],
  });
  await page.evaluate(() => window.meteorTest.finishSubmission());
  await page.getByRole("status").filter({ hasText: "任务已在后台启动" }).waitFor();
  await page.keyboard.press("Control+Enter");
  assert.equal((await calls()).submitted.length, 1, "successful submission remains locked until a new capture opens");

  await open();
  // Click near bottom-right to exercise point capture, autofocus, and panel clamping.
  await page.mouse.click(990, 710);
  await textarea.waitFor();
  await focused();
  await settleLayout();
  const panel = await page.locator(".meteor-note").boundingBox();
  assert.ok(panel && panel.x >= 0 && panel.y >= 0 && panel.x + panel.width <= viewport.width
    && panel.y + panel.height <= viewport.height, `bottom-right panel stays inside the viewport: ${JSON.stringify(panel)}`);
  assert.equal(await page.locator(".meteor-note-actions span").textContent(), "已标注位置");
  await assertVisibleMark(page.locator(".meteor-point"), "::before");
  assert.equal(await page.locator(".meteor-marker").evaluate(element => getComputedStyle(element).backgroundColor), "rgb(255, 209, 102)");
  const retryText = "检查此处，保留我的输入 👀";
  await textarea.fill(retryText);
  await page.mouse.move(500, 600);
  await page.mouse.down();
  await page.mouse.move(300, 200, { steps: 5 });
  await page.mouse.up();
  await focused();
  assert.equal(await textarea.inputValue(), "", "new regions never inherit previous instructions");
  await textarea.fill("比较这个区域和标注1");
  await page.mouse.click(800, 160);
  await focused();
  assert.equal(await textarea.inputValue(), "", "a blank reference point can remain without instructions");
  assert.equal(await page.locator(".meteor-marker").count(), 3);
  await assertVisibleMark(page.locator(".meteor-region:not(.meteor-mark--active)"));
  await page.getByRole("button", { name: "编辑标注 1", exact: true }).click();
  await focused();
  assert.equal(await textarea.inputValue(), retryText);
  const editedText = retryText + "（已修改）";
  await textarea.fill(editedText);
  await page.getByRole("button", { name: "编辑标注 2", exact: true }).click();
  assert.equal(await textarea.inputValue(), "比较这个区域和标注1");
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  assert.deepEqual((await calls()).submitted[0], { captureId: "mock-capture-id", annotations: [
    { text: editedText, region: { x: 1980, y: 1420, width: 0, height: 0 } },
    { text: "比较这个区域和标注1", region: { x: 600, y: 400, width: 400, height: 800 } },
    { text: "", region: { x: 1600, y: 320, width: 0, height: 0 } },
  ] });
  await page.evaluate(() => window.meteorTest.finishSubmission("模拟提交失败"));
  await page.getByRole("alert").filter({ hasText: "模拟提交失败" }).waitFor();
  assert.equal(await textarea.inputValue(), "比较这个区域和标注1", "IPC failure preserves the active instruction");
  assert.equal(await page.locator(".meteor-marker").count(), 3, "IPC failure preserves all selections");
  await page.getByRole("button", { name: "编辑标注 1", exact: true }).click();
  assert.equal(await textarea.inputValue(), editedText, "IPC failure preserves inactive instructions too");
  assert.equal(await textarea.isEnabled(), true);
  assert.equal(await page.locator(".meteor-submit").isEnabled(), true);
  assert.equal((await calls()).submitted.length, 1, "a failed request is not retried automatically");
  await page.locator(".meteor-submit").click();
  await submittedCount(2);
  assert.deepEqual((await calls()).submitted[1], (await calls()).submitted[0], "explicit retry submits the preserved request");
  await page.evaluate(() => window.meteorTest.finishSubmission());
  await page.getByRole("status").filter({ hasText: "任务已在后台启动" }).waitFor();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.meteorTest.calls.cancelled === 1);

  await open();
  await page.mouse.click(400, 300);
  await textarea.fill("第一条");
  await page.mouse.click(700, 180);
  await focused();
  assert.equal(await textarea.inputValue(), "", "clicking another blank position directly adds the second annotation");
  await textarea.fill("第二条");
  await page.mouse.move(250, 560);
  await page.mouse.down();
  await page.mouse.move(100, 350, { steps: 5 });
  await page.mouse.up();
  await textarea.fill("第三条");
  assert.equal(await page.locator(".meteor-marker").count(), 3, "dragging directly after editing adds the third annotation");
  assert.match(await page.locator("#meteor-reference").textContent(), /所有屏幕的完整截图/);
  await page.getByRole("button", { name: "编辑标注 2", exact: true }).click();
  await page.getByRole("button", { name: "删除此标注", exact: true }).click();
  assert.equal(await textarea.inputValue(), "第三条", "deleting selects the following annotation without losing its text");
  assert.deepEqual(await page.locator(".meteor-marker").allTextContents(), ["1", "2"], "labels are renumbered in payload order");
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  assert.deepEqual((await calls()).submitted[0].annotations, [
    { text: "第一条", region: { x: 800, y: 600, width: 0, height: 0 } },
    { text: "第三条", region: { x: 200, y: 700, width: 300, height: 420 } },
  ]);
  await page.evaluate(() => window.meteorTest.finishSubmission());

  await open();
  await page.mouse.click(400, 300);
  await textarea.fill("保留的标注");
  await page.mouse.click(700, 180);
  await textarea.fill("删除的末项");
  await page.getByRole("button", { name: "删除此标注", exact: true }).click();
  await focused();
  assert.equal(await textarea.inputValue(), "保留的标注", "deleting the last of several notes selects the previous note");
  await page.getByRole("button", { name: "删除此标注", exact: true }).click();
  await page.waitForFunction(() => document.activeElement?.classList.contains("meteor-surface"));
  assert.equal(await page.locator(".meteor-marker").count(), 0, "deleting the final note clears selection and restores surface focus");
  await page.mouse.click(500, 400);
  await focused();
  assert.equal(await textarea.inputValue(), "", "a fresh note remains editable after deleting all notes");

  const inside = async (selector, screen) => {
    await settleLayout();
    const rect = await page.locator(selector).boundingBox();
    assert.ok(rect && rect.x >= screen.x - 1 && rect.y >= screen.y - 1
      && rect.x + rect.width <= screen.x + screen.width + 1 && rect.y + rect.height <= screen.y + screen.height + 1,
    `${selector} stays on a real screen: ${JSON.stringify(rect)} in ${JSON.stringify(screen)}`);
  };
  await open("horizontal");
  const rightScreen = { x: 500, y: 120, width: 500, height: 600 };
  await inside(".meteor-toolbar", rightScreen);
  await inside(".meteor-help", rightScreen);
  await page.mouse.click(20, 20);
  await textarea.fill("左屏");
  await inside(".meteor-note", { x: 0, y: 0, width: 500, height: 720 });
  await inside(".meteor-annotations", { x: 0, y: 0, width: 500, height: 720 });
  const topLeftPanel = await page.locator(".meteor-note").boundingBox();
  const topLeftList = await page.locator(".meteor-annotations").boundingBox();
  assert.ok(topLeftPanel.y >= topLeftList.y + topLeftList.height, "top-left annotation input never covers the annotation switcher");
  await page.getByRole("button", { name: "编辑标注 1", exact: true }).click();
  await focused();
  assert.equal(await textarea.inputValue(), "左屏", "the switcher remains directly clickable beside a top-left annotation");
  await page.getByRole("button", { name: "继续标注", exact: true }).click();
  await page.mouse.click(960, 650);
  await textarea.fill("右屏");
  await inside(".meteor-note", rightScreen);
  await inside(".meteor-annotations", rightScreen);
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  assert.deepEqual((await calls()).submitted[0].annotations.map(item => item.region), [
    { x: 40, y: 40, width: 0, height: 0 }, { x: 1920, y: 1300, width: 0, height: 0 },
  ]);
  await page.evaluate(() => window.meteorTest.finishSubmission());
  await open("vertical");
  const lowerScreen = { x: 100, y: 300, width: 900, height: 420 };
  await inside(".meteor-toolbar", lowerScreen);
  await inside(".meteor-help", lowerScreen);
  await page.mouse.click(750, 580);
  await textarea.fill("下屏");
  await inside(".meteor-note", lowerScreen);
  await inside(".meteor-annotations", lowerScreen);

  await open();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.meteorTest.calls.cancelled === 1);
  assert.equal((await calls()).submitted.length, 0, "Escape cancels a fresh capture without starting a task");

  await open("prewarm");
  assert.equal(await page.locator(".meteor-surface--selectable").count(), 0);
  assert.equal(await page.getByRole("alert").count(), 0, "a prewarm null is normal, not a capture failure");
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-1" }));
  await page.waitForFunction(() => document.querySelector(".meteor-surface--selectable"));
  await page.mouse.click(400, 300);
  await textarea.fill("预热后输入");
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-1" }));
  assert.equal(await textarea.inputValue(), "预热后输入", "a repeated capture event cannot clear the draft");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.meteorTest.calls.cancelled === 1);
  assert.deepEqual((await calls()).cancelIds, ["warm-1"], "cancel is bound to the capture that requested it");
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-2" }));
  assert.equal(await page.locator(".meteor-marker").count(), 0, "cancel/reopen with a new ID starts an empty task without reloading");
  await page.mouse.click(400, 300);
  await textarea.fill("第二次任务");
  await page.mouse.move(50, 600);
  await page.mouse.down();
  await page.evaluate(() => document.querySelector(".meteor-surface").dispatchEvent(new PointerEvent("pointercancel", { pointerId: 1, isPrimary: true })));
  await page.mouse.up();
  await focused();
  assert.equal(await textarea.inputValue(), "第二次任务", "cancelled selection restores the original note and text");
  assert.equal(await page.locator(".meteor-marker").count(), 1);
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-3" }));
  await page.mouse.click(400, 300);
  await textarea.fill("不要被旧提交覆盖");
  await page.evaluate(() => window.meteorTest.finishSubmission());
  assert.equal(await textarea.isEnabled(), true);
  assert.equal(await textarea.inputValue(), "不要被旧提交覆盖");
  assert.equal(await page.getByRole("status").textContent(), "1 / 20 条标注", "a late old submit cannot mark the reopened task submitted");
  await page.keyboard.press("Control+Enter");
  await submittedCount(2);
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-4" }));
  await page.mouse.click(400, 300);
  await textarea.fill("新任务无旧错误");
  await page.evaluate(() => window.meteorTest.finishSubmission("旧任务失败"));
  assert.equal(await page.getByRole("alert").count(), 0, "an old task failure cannot leak into a reopened capture");
  await page.evaluate(() => { window.meteorTest.delayCancel = true; });
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.meteorTest.calls.cancelled === 2);
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "warm-5" }));
  await page.mouse.click(400, 300);
  await textarea.fill("新任务无旧取消错误");
  await page.evaluate(() => window.meteorTest.finishCancel("旧取消失败"));
  assert.equal(await page.getByRole("alert").count(), 0);
  assert.equal(await textarea.isEnabled(), true);
  assert.equal(await textarea.inputValue(), "新任务无旧取消错误");

  await open("delayed-context");
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "event-before-null" }));
  await page.mouse.click(400, 300);
  await textarea.fill("事件优先");
  await page.evaluate(() => window.meteorTest.finishContext(null));
  assert.equal(await textarea.inputValue(), "事件优先", "a delayed null fallback never overwrites an open event");
  assert.equal(await page.locator(".meteor-surface--selectable").count(), 1);

  await open("delayed-context");
  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "event-newer-than-fallback" }));
  await page.mouse.click(400, 300);
  await textarea.fill("旧context不可覆盖");
  await page.evaluate(() => window.meteorTest.finishContext({ captureId: "stale-context", width: 2000, height: 1440 }));
  assert.equal(await textarea.inputValue(), "旧context不可覆盖", "a delayed older context cannot reset an open event's annotations");
  await page.keyboard.press("Control+Enter");
  await submittedCount(1);
  assert.equal((await calls()).submitted[0].captureId, "event-newer-than-fallback");
  await page.evaluate(() => window.meteorTest.finishSubmission());

  await page.evaluate(() => window.meteorTest.emitOpen({ captureId: "annotation-limit" }));
  for (let index = 0; index < 20; index++) {
    await page.mouse.click(30 + index * 30, 690);
    await textarea.waitFor();
    if (index === 0) await textarea.fill("保留全部参考");
  }
  assert.equal(await page.locator(".meteor-marker").count(), 20);
  await page.mouse.click(980, 690);
  assert.equal(await page.locator(".meteor-marker").count(), 20, "a 21st pointer gesture cannot replace or erase existing annotations");
  await page.getByRole("button", { name: "编辑标注 1", exact: true }).click();
  assert.equal(await textarea.inputValue(), "保留全部参考");

  for (const [scenario, message] of [["context-error", "模拟屏幕布局 IPC 失败"], ["geometry-error", "屏幕布局无效"]]) {
    await open(scenario);
    await page.getByRole("alert").filter({ hasText: message }).waitFor();
    await page.getByRole("button", { name: "关闭", exact: true }).click();
    await page.waitForFunction(() => window.meteorTest.calls.cancelled === 1);
    assert.equal((await calls()).submitted.length, 0, `${scenario} remains closable and never starts a task`);
  }
  assert.deepEqual(errors, [], "the real Solid component raises no unhandled browser errors");
  assert.deepEqual(blockedRequests, [], "the test performs no external network/model requests");
  assert.deepEqual(requests.filter(url => /\/src\/(App\.tsx|store\.ts|app\.css)|fontsource|\.(woff2?|ttf)(\?|$)/.test(url)), [], "the real Meteor route never loads App/store/global CSS/fonts");
  assert.deepEqual([...new Set(ipcCalls)].sort(), ["meteor_cancel", "meteor_context", "meteor_submit", "plugin:event|listen"], "only Meteor IPC and its own open listener are used");
  console.log("Meteor UI: transparent geometry-only startup, direct multi-annotation, reusable-window events, async isolation, retry preservation, DPI and multi-screen layout passed; all IPC mocked.");
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all(entryFiles.map(path => rm(path, { force: true })));
  await rm(new URL(`../node_modules/.vite-${name}`, import.meta.url), { recursive: true, force: true });
}
