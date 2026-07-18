// 界面走查截图：用系统 Chrome 打开 vite dev 页面，mock 掉 Tauri IPC，
// 截取明/暗主题的首页与会话页。用法：先 `npm run dev`，再 `node scripts/ui-screenshot.mjs [outDir]`
import { chromium } from "playwright-core";

const outDir = process.argv[2] ?? "tmp/shots";
const base = process.env.UI_SHOT_URL ?? "http://localhost:5173";

const now = Date.now();
const mockThread = {
  id: "t1",
  title: "重构登录模块",
  cwd: "D:/code/nova",
  agentKind: "codex",
  model: "gpt-5.2-codex",
  mode: "build",
  starred: false,
  createdAt: now - 3600_000,
  updatedAt: now - 60_000,
  items: [
    { type: "user", id: 1, text: "帮我重构登录模块，把鉴权逻辑抽到独立 service，并补上单元测试", ts: now - 3500_000 },
    { type: "thought", id: 2, text: "先看下现有结构，登录入口应该在 src/auth 下……", ts: now - 3490_000 },
    {
      type: "tool", id: 3, ts: now - 3480_000, toolCallId: "c1", title: "读取 src/auth/login.ts", kind: "read", status: "completed",
      content: [{ type: "content", content: { type: "text", text: "export function login(user, pass) {\n  // 128 lines ...\n}" } }],
      locations: [{ path: "src/auth/login.ts", line: 1 }],
    },
    {
      type: "tool", id: 4, ts: now - 3470_000, toolCallId: "c2", title: "修改 src/auth/service.ts", kind: "edit", status: "completed",
      content: [{ type: "diff", path: "src/auth/service.ts", oldText: "const token = sign(user);", newText: "const token = sign(user, { expiresIn: \"2h\" });\nlogAudit(\"login\", user.id);" }],
      locations: [{ path: "src/auth/service.ts", line: 42 }],
    },
    { type: "assistant", id: 5, text: "已完成重构：\n\n- 鉴权逻辑抽到 `AuthService`，登录入口只做参数校验\n- 补了 12 个单元测试，覆盖token 过期、刷新、并发刷新三个场景\n\n```ts\nconst token = sign(user, { expiresIn: \"2h\" });\nlogAudit(\"login\", user.id);\n```\n\n测试全部通过，可以提交。", ts: now - 3460_000 },
    { type: "turn", id: 6, ts: now - 3450_000, durationMs: 42000, totalTokens: 18320, stopReason: "end_turn" },
    { type: "user", id: 7, text: "继续，把 refresh token 的旋转也加上", ts: now - 3400_000 },
    { type: "assistant", id: 8, text: "好的，我会在 `AuthService.refresh()` 里做旋转：旧 refresh token 立即作废，新 token 与访问令牌一起下发，并记录 rotation 审计日志。", ts: now - 3390_000 },
  ],
  plan: null,
};

const threadMeta = {
  id: "t1",
  title: "重构登录模块",
  cwd: "D:/code/nova",
  agentKind: "codex",
  createdAt: now - 3600_000,
  updatedAt: now - 60_000,
  running: false,
  starred: false,
};

const mockSettings = {
  devinPath: "", acpArgs: "", devinProxy: "", codebuddyPath: "", codebuddyProxy: "",
  claudecodePath: "", claudecodeProxy: "", claudecodeSdkApiKey: "", cursorProxy: "",
  cursorPath: "", cursorSdkApiKey: "", opencodePath: "", opencodeProxy: "",
  codexPath: "", codexProxy: "", windowsShellShimEnabled: false,
  defaultMode: "build", titleModelAgent: "", titleModel: "", shareModelAgent: "",
  shareModel: "", editor: "cursor", theme: "", historyDisplayMode: "project",
  relayServer: "", relayToken: "", relayGroups: "", remoteControlEnabled: false,
  quotaSharedModels: [], devinEnabled: true, codexEnabled: true,
  codebuddyEnabled: false, claudecodeEnabled: false, cursorEnabled: false,
  opencodeEnabled: false, codexIntegration: "sdk", codebuddyIntegration: "sdk",
  claudecodeIntegration: "sdk", cursorIntegration: "sdk", opencodeIntegration: "sdk",
  worktreeDir: "", sessionAutoCleanupEnabled: false, sessionAutoCleanupHours: 720,
  semanticEnabled: false, embedEndpoint: "", embedModel: "", embedApiKey: "",
};

// @tauri-apps/api 走 window.__TAURI_INTERNALS__.invoke；给个最小 mock 让前端在纯浏览器里跑起来
const tauriMock = `
  window.__MOCK_THREAD__ = ${JSON.stringify(mockThread)};
  window.__TAURI_INTERNALS__ = {
    invoke(cmd) {
      const mocks = {
        list_threads: [${JSON.stringify(threadMeta)}],
        list_projects: [{ path: "D:/code/nova" }],
        get_thread: window.__MOCK_THREAD__,
        scratch_dir: "D:/scratch",
        get_quota: null,
        get_settings: ${JSON.stringify(mockSettings)},
        check_update: null,
        get_model_costs: {},
        get_relay_status: { enabled: false, connected: false },
        get_status: { connected: false, agent: null },
        get_logs: [],
        list_roaming_folders: [],
        list_worktrees: [],
        list_skills: [],
        get_skills_dir: "D:/skills",
        get_global_agent_instructions: { content: "", path: "", targets: [] },
        semantic_status: { ok: false, dim: 0 },
        list_clue_groups: [],
        take_restore_thread: null,
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

const browser = await chromium.launch({ channel: "chrome", headless: true });

for (const theme of ["ink-dark", "ink-light"]) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await ctx.addInitScript(tauriMock);
  await ctx.addInitScript((t) => localStorage.setItem("fd:theme", t), theme);
  const page = await ctx.newPage();
  await page.goto(base, { waitUntil: "networkidle" });
  await page.waitForTimeout(1200);
  await page.screenshot({ path: `${outDir}/home-${theme}.png` });
  // 点开会话看聊天页
  await page.click(".thread-item");
  await page.waitForTimeout(1000);
  await page.screenshot({ path: `${outDir}/chat-${theme}.png` });
  // 设置弹窗
  await page.click(".settings-btn");
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${outDir}/settings-${theme}.png` });
  console.log(`saved home/chat/settings ${theme}`);
  await ctx.close();
}

await browser.close();
