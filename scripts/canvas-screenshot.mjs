// Canvas 改造 A/B 对比截图：富 mock 会话 + 交互走查。
// 用法：先 `npm run dev`，再 `node scripts/canvas-screenshot.mjs [outDir]`
// - 首轮（DOM 版）运行会把关键元素的坐标写入 <outDir>/coords.json；
// - canvas 版（无 DOM 选择器）自动回退到 coords.json 中的坐标做同样交互。
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { chromium } from "playwright-core";
import { now, mockThread, threadMeta, mockSettings, tauriMock, items } from "./canvas-mock.mjs";

const outDir = process.argv[2] ?? "tmp/canvas-shots";
const base = process.env.UI_SHOT_URL ?? "http://localhost:5173";
mkdirSync(outDir, { recursive: true });


// —— 交互序列：能用 DOM 选择器就记录坐标；canvas 版走 __canvasFind 定位；最后回退 coords.json ——
const coordsPath = `${outDir}/coords.json`;
const savedCoords = existsSync(coordsPath)
  ? JSON.parse(readFileSync(coordsPath, "utf8"))
  : {};
const coords = {};

// name → canvas 查询（DOM 选择器不存在时使用）
const CANVAS_QUERIES = {
  turnFold: { text: "已处理 42s" },
  turnFold2: { text: "已处理 1m 23s" },
  turnFold3: { text: "已处理 12s" },
  thought: { text: "思考过程" },
  toolBuild: { text: "npm run build" },
  permAllow: { text: "允许一次" },
  permReject: { text: "拒绝回答" },
  userBubble: { clickId: "userBubble" },
  userEditBtn: { clickId: "userEditBtn" },
};

async function pointOf(page, name, selector, nth = 0) {
  const loc = page.locator(selector).nth(nth);
  if ((await loc.count()) > 0) {
    await loc.scrollIntoViewIfNeeded();
    await page.waitForTimeout(100);
    const box = await loc.boundingBox();
    if (box) {
      coords[name] = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
      return coords[name];
    }
  }
  const query = CANVAS_QUERIES[name];
  if (query) {
    const rects = await page.evaluate(
      (q) => (window.__canvasFind ? window.__canvasFind(q, { scrollIntoView: true }) : []),
      query,
    );
    await page.waitForTimeout(150);
    if (rects.length > 0) {
      const r = rects[Math.min(nth, rects.length - 1)];
      const p = { x: r.x + r.w / 2, y: r.y + r.h / 2 };
      coords[name] = p;
      return p;
    }
  }
  if (savedCoords[name]) return savedCoords[name];
  throw new Error(`无法定位交互点 ${name}（${selector} 不存在且无缓存坐标）`);
}

async function clickPoint(page, name, selector, nth = 0) {
  const p = await pointOf(page, name, selector, nth);
  await page.mouse.click(p.x, p.y);
  // 展开/收起会触发 canvas 重建，等一拍再定位下一个交互点
  await page.waitForTimeout(300);
}

async function wheelOver(page, dy) {
  const body = await page.locator(".chat-body").boundingBox();
  await page.mouse.move(body.x + body.width / 2, body.y + body.height / 2);
  await page.mouse.wheel(0, dy);
  await page.waitForTimeout(120);
}

async function shot(page, name) {
  await page.waitForTimeout(350);
  await page.screenshot({ path: `${outDir}/${name}.png` });
  console.log(`saved ${name}`);
}

const browser = await chromium.launch({ channel: "chrome", headless: true });

for (const theme of ["ink-dark", "ink-light"]) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await ctx.addInitScript(tauriMock);
  await ctx.addInitScript((t) => localStorage.setItem("fd:theme", t), theme);
  const page = await ctx.newPage();
  page.on("pageerror", (err) => console.error(`[pageerror ${theme}]`, err.stack ?? err.message));
  await page.goto(base, { waitUntil: "networkidle" });
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(800);
  await page.click(".thread-item");
  await page.waitForTimeout(1000);
  await page.evaluate(() => document.fonts.ready);

  await shot(page, `01-bottom-${theme}`);
  // 滚到顶
  for (let i = 0; i < 24; i++) await wheelOver(page, -3000);
  await shot(page, `02-top-${theme}`);
  // 展开第 1 轮的折叠行
  await clickPoint(page, "turnFold", ".turn-fold", 0);
  await shot(page, `03-turn-expanded-${theme}`);
  // 其余轮次的折叠行也展开，让所有工具卡可见
  await clickPoint(page, "turnFold2", ".turn-fold", 1);
  await clickPoint(page, "turnFold3", ".turn-fold", 2);
  // 展开思考
  await clickPoint(page, "thought", ".thought-toggle", 0);
  // 展开长输出工具（第 2 轮 build 工具是列表里第 4 条 tool-line：read/edit/failed/build）
  await clickPoint(page, "toolBuild", ".tool-line", 3);
  await shot(page, `04-tools-expanded-${theme}`);
  // 中部（markdown 展示区）
  for (let i = 0; i < 6; i++) await wheelOver(page, 3000);
  await shot(page, `05-mid-${theme}`);
  // 拖选一段文字
  {
    const body = await page.locator(".chat-body").boundingBox();
    const x = body.x + body.width * 0.35;
    const y = body.y + body.height * 0.3;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 260, y + 90, { steps: 12 });
    await shot(page, `06-selection-${theme}`);
    await page.mouse.up();
  }
  // 权限卡（选项式）
  await page.evaluate(() => {
    window.__MOCK_EMIT__("acp:permission", {
      threadId: "t1", agentKind: "codex", requestKey: "perm-1",
      toolCall: { title: "npm run deploy", kind: "execute", rawInput: { command: "npm run deploy --prod" } },
      options: [
        { optionId: "allow", name: "允许一次", kind: "allow_once" },
        { optionId: "always", name: "总是允许", kind: "allow_always" },
        { optionId: "reject", name: "拒绝", kind: "reject_once" },
      ],
    });
  });
  await shot(page, `07-permission-${theme}`);
  await clickPoint(page, "permAllow", ".perm-btn.allow", 0);
  // 权限卡（问答式）
  await page.evaluate(() => {
    window.__MOCK_EMIT__("acp:permission", {
      threadId: "t1", agentKind: "codex", requestKey: "perm-2",
      toolCall: { title: "选择发布目标", kind: "other" },
      options: [],
      questions: [{
        header: "发布配置", question: "选择要发布的环境（可多选）", multiple: true, custom: true,
        options: [
          { label: "staging", description: "预发环境，随时可发" },
          { label: "prod", description: "生产环境，需要评审" },
        ],
      }],
    });
  });
  await shot(page, `08-permission-question-${theme}`);
  await clickPoint(page, "permReject", ".perm-btn.reject", 0);
  // 用户消息编辑覆盖层
  await wheelOver(page, -12000);
  await wheelOver(page, -12000);
  {
    const p = await pointOf(page, "userBubble", ".user-bubble", 0);
    await page.mouse.move(p.x, p.y);
    await page.waitForTimeout(250);
    await clickPoint(page, "userEditBtn", ".user-edit-btn", 0);
  }
  await shot(page, `09-edit-${theme}`);
  await page.keyboard.press("Escape");
  // 回底
  for (let i = 0; i < 24; i++) await wheelOver(page, 3000);
  // 流式：running + 新用户消息 + delta 分段
  await page.evaluate(() => {
    window.__MOCK_EMIT__("acp:turn", { threadId: "t1", running: true });
    window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "upsert", item: { type: "user", id: 100, text: "再把部署脚本加上", ts: Date.now() } } });
    window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "upsert", item: { type: "tool", id: 101, ts: Date.now(), toolCallId: "c5", title: "npm run deploy --dry", kind: "execute", status: "in_progress", content: [], locations: [] } } });
    window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "upsert", item: { type: "assistant", id: 102, text: "", ts: Date.now() } } });
  });
  const chunks = ["好的，", "我先跑一遍 dry-run 确认部署脚本没有问题。\n\n", "```bash\n", "npm run deploy --dry\n", "```\n\n", "输出看起来正常，", "接下来我把脚本固化到 package.json 里。"];
  for (const c of chunks) {
    await page.evaluate((text) => window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "delta", itemId: 102, text } }), c);
    await page.waitForTimeout(180);
  }
  await shot(page, `10-streaming-${theme}`);
  await page.evaluate(() => {
    window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "upsert", item: { type: "tool", id: 101, ts: Date.now(), toolCallId: "c5", title: "npm run deploy --dry", kind: "execute", status: "completed", content: [{ type: "content", content: { type: "text", text: "dry-run ok" } }], locations: [] } } });
    window.__MOCK_EMIT__("acp:update", { threadId: "t1", op: { t: "upsert", item: { type: "turn", id: 103, ts: Date.now(), durationMs: 8000, totalTokens: 1200, stopReason: "end_turn" } } });
    window.__MOCK_EMIT__("acp:turn", { threadId: "t1", running: false });
  });
  await shot(page, `11-after-stream-${theme}`);
  await ctx.close();
}

writeFileSync(coordsPath, JSON.stringify(coords, null, 2));
console.log(`coords saved to ${coordsPath}`);

// —— 长会话性能摸底（120 轮）：滚动全程采样帧间隔 ——
{
  const longItems = [];
  let id = 1;
  for (let turn = 0; turn < 120; turn++) {
    longItems.push({ type: "user", id: id++, text: `第 ${turn + 1} 轮：请继续优化模块 ${turn + 1}`, ts: now });
    longItems.push({ type: "assistant", id: id++, text: `已完成模块 ${turn + 1} 的优化。\n\n- 要点一：\`src/mod${turn}.ts\` 重写\n- 要点二：补了 **${turn + 3}** 个测试\n\n\`\`\`ts\nexport const mod${turn} = optimize(${turn});\n\`\`\``, ts: now });
    longItems.push({ type: "turn", id: id++, ts: now, durationMs: 15000, totalTokens: 5000, stopReason: "end_turn" });
  }
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const longMock = `
    window.__MOCK_THREAD__ = ${JSON.stringify({ ...mockThread, items: longItems })};
    window.__EVENT_LISTENERS__ = new Map();
    window.__TAURI_INTERNALS__ = {
      invoke(cmd, args) {
        if (cmd === "plugin:event|listen") return Promise.resolve(args?.handler ?? 1);
        if (cmd === "plugin:event|unlisten") return Promise.resolve(null);
        const mocks = {
          list_threads: [${JSON.stringify(threadMeta)}],
          list_projects: [{ path: "D:/code/nova" }],
          get_thread: window.__MOCK_THREAD__,
          get_settings: ${JSON.stringify(mockSettings)},
          get_relay_status: { enabled: false, connected: false },
          get_status: { connected: false, agent: null },
          get_logs: [], list_skills: [], list_clue_groups: [],
          take_restore_thread: null, scratch_dir: "D:/scratch",
          get_model_options: { configOptions: [], modes: null },
          get_time_machine_timeline: { checkpoints: [], currentCheckpointId: null },
          list_employees: [], list_tasks: [], list_decisions: [], list_marks: [],
        };
        return Promise.resolve(cmd in mocks ? mocks[cmd] : null);
      },
      transformCallback(cb) {
        const id = Math.floor(Math.random() * 1e9);
        (window.__TAURI_CBS__ ??= new Map()).set(id, cb);
        return id;
      },
      unregisterCallback() {},
      convertFileSrc: (p) => p,
      isTauri: true,
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  `;
  await ctx.addInitScript(longMock);
  await ctx.addInitScript(() => localStorage.setItem("fd:theme", "ink-dark"));
  const page = await ctx.newPage();
  page.on("pageerror", (err) => console.error("[pageerror long]", err.message));
  await page.goto(base, { waitUntil: "networkidle" });
  await page.waitForTimeout(800);
  await page.click(".thread-item");
  await page.waitForTimeout(1500);
  await page.evaluate(() => {
    window.__FRAME_SAMPLES__ = [];
    let last = performance.now();
    const loop = (t) => {
      window.__FRAME_SAMPLES__.push(t - last);
      last = t;
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  });
  const body = await page.locator(".chat-body").boundingBox();
  await page.mouse.move(body.x + body.width / 2, body.y + body.height / 2);
  for (let i = 0; i < 40; i++) {
    await page.mouse.wheel(0, -4000);
    await page.waitForTimeout(60);
  }
  for (let i = 0; i < 40; i++) {
    await page.mouse.wheel(0, 4000);
    await page.waitForTimeout(60);
  }
  const stats = await page.evaluate(() => {
    const s = window.__FRAME_SAMPLES__.slice(10).sort((a, b) => a - b);
    const avg = s.reduce((a, b) => a + b, 0) / s.length;
    return { frames: s.length, avg: avg.toFixed(1), p95: s[Math.floor(s.length * 0.95)]?.toFixed(1) };
  });
  console.log(`[long-session] frames=${stats.frames} avg=${stats.avg}ms p95=${stats.p95}ms`);
  await page.screenshot({ path: `${outDir}/12-long-session.png` });
  await ctx.close();
}

await browser.close();
console.log("done");
