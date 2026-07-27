const now = Date.now();
const svgImg = (color, label) =>
  Buffer.from(
    `<svg xmlns="http://www.w3.org/2000/svg" width="160" height="100"><rect width="160" height="100" fill="${color}"/><text x="12" y="55" font-size="16" fill="#fff">${label}</text></svg>`,
  ).toString("base64");

const MD_SHOWCASE = [
  "# 重构方案总览",
  "",
  "把**鉴权逻辑**抽到 `AuthService`，登录入口只做*参数校验*，旧的 ~~login() 直查库~~ 全部下线。",
  "详见 [内部文档](https://example.com/docs/auth) 和 src/auth/login.ts:42 的调用点，配置在 `config/auth.yaml`。",
  "",
  "## 改动清单",
  "",
  "- 抽离鉴权逻辑到 `src/auth/service.ts`",
  "  - 支持 **refresh token** 旋转",
  "  - 旧 token 立即作废",
  "    1. 第一步：改 service",
  "    2. 第二步：换调用点",
  "- [x] 补单元测试",
  "- [ ] 灰度发布",
  "",
  "### 核心代码",
  "",
  "```ts",
  "const token = sign(user, { expiresIn: \"2h\" });",
  "logAudit(\"login\", user.id);",
  "const veryLongLine = \"这是一行特别特别长的代码，用来验证横向滚动是否正常，it should overflow the code block and show a horizontal scrollbar instead of wrapping\";",
  "```",
  "",
  "> 注意：旧接口保留一个版本周期。",
  ">",
  "> 多行引用第二段，`token` 有效期不变。",
  "",
  "| 文件 | 改动 | 状态 |",
  "| --- | --- | --- |",
  "| src/auth/service.ts | 新增 | 完成 |",
  "| src/auth/login.ts | 简化 | 完成 |",
  "| tests/auth.test.ts | 新增 12 例 | 通过 |",
  "",
  "---",
  "",
  "测试全部通过 ✅，可以提交。",
].join("\n");

const LONG_CJK = Array.from(
  { length: 6 },
  (_, i) =>
    `第 ${i + 1} 段：这是一个用来撑高度的中文长段落。朱砂砚台、笔墨纸砚，Canvas 渲染需要正确处理中文换行——中文没有空格分词，必须逐字断行，同时英文单词 keepTogether 不拆分。混合排版 mixed content ${i + 1} 结束。`,
).join("\n\n");

const LONG_OUTPUT = Array.from(
  { length: 60 },
  (_, i) => `[${String(i + 1).padStart(2, "0")}] 编译输出 compile output line ${i + 1} — everything looks fine`,
).join("\n");

const diffOld = [
  "import { sign } from \"./jwt\";",
  "",
  "export function login(user, pass) {",
  "  check(pass);",
  "  const a = 1;",
  "  const b = 2;",
  "  const c = 3;",
  "  const d = 4;",
  "  const e = 5;",
  "  const f = 6;",
  "  const g = 7;",
  "  const h = 8;",
  "  const i = 9;",
  "  const j = 10;",
  "  return sign(user);",
  "}",
].join("\n");
const diffNew = diffOld
  .replace("return sign(user);", "return sign(user, { expiresIn: \"2h\" });\n  logAudit(\"login\", user.id);");

const items = [
  // —— 第 1 轮：完整轮次（折叠行 + 实际模型 pill + 已编辑文件卡）——
  {
    type: "user", id: 1, ts: now - 3500_000,
    text: "帮我重构登录模块，把鉴权逻辑抽到独立 service，并补上单元测试。\n要求：\n1. 保持接口兼容\n2. 测试覆盖 token 过期场景",
    images: [
      { name: "架构图.svg", mimeType: "image/svg+xml", data: svgImg("#4a6fa5", "arch") },
      { name: "流程图.svg", mimeType: "image/svg+xml", data: svgImg("#7a4a8a", "flow") },
      { name: "需求文档.pdf", mimeType: "application/pdf", uri: "file:///D:/code/nova/docs/req.pdf" },
    ],
  },
  { type: "thought", id: 2, text: "先看下现有结构，登录入口应该在 src/auth 下。\n\n**计划**：读 login.ts → 抽 service → 换调用点。", ts: now - 3490_000 },
  {
    type: "tool", id: 3, ts: now - 3480_000, toolCallId: "c1", title: "读取 src/auth/login.ts", kind: "read", status: "completed",
    content: [{ type: "content", content: { type: "text", text: "export function login(user, pass) {\n  // 128 lines ...\n}" } }],
    locations: [{ path: "D:/code/nova/src/auth/login.ts", line: 1 }, { path: "D:/code/nova/src/auth/jwt.ts", line: 8 }],
  },
  {
    type: "tool", id: 4, ts: now - 3470_000, toolCallId: "c2", title: "修改 src/auth/service.ts", kind: "edit", status: "completed",
    content: [{ type: "diff", path: "D:/code/nova/src/auth/service.ts", oldText: diffOld, newText: diffNew }],
    locations: [{ path: "D:/code/nova/src/auth/service.ts", line: 42 }],
  },
  {
    type: "tool", id: 5, ts: now - 3468_000, toolCallId: "c3", title: "npm run test -- auth", kind: "execute", status: "failed",
    content: [{ type: "content", content: { type: "text", text: "FAIL tests/auth.test.ts\n  ● token 过期 › 应拒绝\n    Expected 401, received 200" } }],
    locations: [],
  },
  { type: "assistant", id: 6, text: "第一轮改动完成，测试有一处断言需要调整，我接着修。", ts: now - 3466_000 },
  {
    type: "turn", id: 7, ts: now - 3465_000, durationMs: 42000, totalTokens: 18320,
    inputTokens: 15000, outputTokens: 3320, cacheReadTokens: 4000, cacheWriteTokens: 1000,
    actualModel: "gpt-5.2-codex", stopReason: "end_turn",
  },
  // —— 第 2 轮：markdown 全特性 + 系统消息 + compaction + 长输出工具 ——
  { type: "user", id: 8, text: "继续，把 refresh token 的旋转也加上，然后给我一份完整说明", ts: now - 3400_000 },
  { type: "assistant", id: 9, text: MD_SHOWCASE, ts: now - 3390_000 },
  { type: "system", id: 10, text: "执行出错：npm 进程退出码 1（示例错误消息）", level: "error", ts: now - 3385_000 },
  { type: "system", id: 11, text: "上下文已使用 78%，建议适时压缩", level: "warn", ts: now - 3384_000 },
  { type: "system", id: 12, text: "已切换到 Plan 模式", level: "info", ts: now - 3383_000 },
  { type: "system", id: 13, text: "已压缩 24 条早期消息", level: "compacted", ts: now - 3382_000 },
  {
    type: "tool", id: 14, ts: now - 3380_000, toolCallId: "c4", title: "npm run build", kind: "execute", status: "completed",
    content: [{ type: "content", content: { type: "text", text: LONG_OUTPUT } }],
    locations: [],
    rawInput: { command: "npm run build" },
    rawOutput: { exitCode: 0 },
  },
  { type: "turn", id: 15, ts: now - 3370_000, durationMs: 83000, totalTokens: 25100, inputTokens: 20000, outputTokens: 5100, stopReason: "end_turn" },
  // —— 第 3 轮：思考 + 长中文结论 ——
  { type: "user", id: 16, text: "总结一下这次改动的关键点", ts: now - 3300_000 },
  {
    type: "thought", id: 17, ts: now - 3290_000,
    text: "用户想要一份总结。\n\n要点：\n- service 抽离\n- refresh 旋转\n- 测试 12 例\n\n用清单形式给出，附上关键文件引用 src/auth/service.ts。",
  },
  { type: "assistant", id: 18, text: LONG_CJK, ts: now - 3280_000 },
  { type: "turn", id: 19, ts: now - 3270_000, durationMs: 12000, totalTokens: 4200, stopReason: "end_turn" },
];

const mockThread = {
  id: "t1", title: "重构登录模块", cwd: "D:/code/nova", agentKind: "codex",
  model: "gpt-5.2-codex", mode: "build", starred: false,
  createdAt: now - 3600_000, updatedAt: now - 60_000, items, plan: null,
};

const threadMeta = {
  id: "t1", title: "重构登录模块", cwd: "D:/code/nova", agentKind: "codex",
  createdAt: now - 3600_000, updatedAt: now - 60_000, running: false, starred: false,
};

const mockSettings = {
  devinPath: "", acpArgs: "", devinProxy: "", codebuddyPath: "", codebuddyProxy: "",
  claudecodePath: "", claudecodeProxy: "", claudecodeSdkApiKey: "", cursorProxy: "",
  cursorPath: "", cursorSdkApiKey: "", opencodePath: "", opencodeProxy: "",
  codexPath: "", codexProxy: "", vegaProxy: "", windowsShellShimEnabled: false,
  defaultMode: "build", lightweightModelAgent: "alkaid", lightweightModel: "",
  editor: "cursor", theme: "", historyDisplayMode: "project",
  relayServer: "", relayToken: "", relayGroups: "", remoteControlEnabled: false,
  quotaSharedModels: [], devinEnabled: true, codexEnabled: true,
  codebuddyEnabled: false, claudecodeEnabled: false, cursorEnabled: false,
  opencodeEnabled: false, codexIntegration: "sdk", codebuddyIntegration: "sdk",
  claudecodeIntegration: "sdk", cursorIntegration: "sdk", opencodeIntegration: "sdk",
  worktreeDir: "", sessionAutoCleanupEnabled: false, sessionAutoCleanupHours: 720,
  semanticEnabled: false, embedEndpoint: "", embedModel: "", embedApiKey: "",
  modelFavorites: [], vegaSlimContextEnabled: false, checkpointEnabled: false,
  alkaidEnabled: true,
};

const tauriMock = `
  window.__MOCK_THREAD__ = ${JSON.stringify(mockThread)};
  window.__EVENT_LISTENERS__ = new Map();
  window.__TAURI_INTERNALS__ = {
    invoke(cmd, args) {
      if (cmd === "plugin:event|listen") {
        const { event, handler } = args;
        const list = window.__EVENT_LISTENERS__.get(event) ?? [];
        list.push(handler);
        window.__EVENT_LISTENERS__.set(event, list);
        return Promise.resolve(handler);
      }
      if (cmd === "plugin:event|unlisten") return Promise.resolve(null);
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
  window.__MOCK_EMIT__ = (event, payload) => {
    for (const id of window.__EVENT_LISTENERS__.get(event) ?? []) {
      const cb = window.__TAURI_CBS__.get(id);
      if (cb) cb({ event, payload });
    }
  };
`;

export { now, mockThread, threadMeta, mockSettings, tauriMock, items };
