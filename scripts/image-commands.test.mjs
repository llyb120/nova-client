import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const compile = (source) => transformSync(source, { loader: "ts", format: "cjs", target: "esnext" }).code;
test("local image previews enable the native asset protocol with image-only scope", () => {
  const config = JSON.parse(read("../src-tauri/tauri.conf.json"));
  assert.equal(config.app.security.assetProtocol.enable, true);
  assert.ok(config.app.security.assetProtocol.scope.includes("**/*.[pP][nN][gG]"));
  assert.ok(!config.app.security.assetProtocol.scope.includes("**"));
  const tauri = read("../src-tauri/Cargo.toml").split("\n").find((line) => line.startsWith("tauri ="));
  assert.ok(tauri.includes('"protocol-asset"'));
});
function exportsOf(source, imports = {}) {
  const module = { exports: {} };
  new Function("module", "exports", "require", compile(source))(module, module.exports, (id) => imports[id]);
  return module.exports;
}

test("image commands are available to every backend and keep credentials out of generation instructions", () => {
  const prompts = exportsOf(read("../src/builtinPrompts.ts"));
  const suggestions = exportsOf(read("../src/components/slashSuggestions.ts"), { "../utils": { agentLabel: (id) => id } });
  for (const backend of ["lyra", "codex", "devin", "codebuddy", "claudecode", "cursor", "opencode", "kimi"]) {
    const commands = suggestions.getSlashSuggestions(backend, [], "image").map((v) => v.title);
    assert.ok(commands.includes("/setup-image"));
    assert.ok(commands.includes("/generate-image"));
  }
  const prompt = prompts.buildGenerateImagePrompt("画一只猫", { configPath: "D:/My Data/image.json", executable: "D:/My Apps/Nova.exe", models: ["one", "two"], apiKey: "secret-token" });
  assert.ok(prompt.includes('__generate_image'));
  assert.ok(prompt.includes('["one","two"]'));
  assert.ok(!prompt.includes("secret-token"));
  assert.ok(prompts.buildSetupImagePrompt("", "D:/image.json").includes("/images/generations"));
});

test("local image URLs handle Windows paths and spaces; unsupported schemes stay disabled", () => {
  const image = exportsOf(read("../src/transcriptImage.ts"), { "@tauri-apps/api/core": { convertFileSrc: (path) => `asset:${path}` } });
  assert.equal(image.transcriptImageSrc("D:/My Images/cat.png"), "asset:D:/My Images/cat.png");
  assert.equal(image.localImagePath(String.raw`\\?\D:\My Images\cat.png`), "D:/My Images/cat.png");
  assert.equal(image.transcriptImageSrc("/tmp/cat.png"), "asset:/tmp/cat.png");
  for (const href of ["javascript:alert(1)", "file://secret", "relative.png", "//remote/test.png"]) assert.equal(image.transcriptImageSrc(href), "");
});

test("canvas recognizes generated image blocks without swallowing them into prose or code", () => {
  const source = read("../src/components/CanvasTranscript.tsx");
  const parserStart = source.indexOf("interface MdBlock");
  const end = source.indexOf("const TABLE_FS", parserStart);
  const parser = exportsOf(source.slice(parserStart, end) + "\nexport { parseMarkdownBlocks }; ").parseMarkdownBlocks;
  const blocks = parser("已生成\n![图片](<D:/My Images/cat.png>)\n\n```md\n![example](x.png)\n```");
  assert.deepEqual(blocks.map((v) => v.type), ["paragraph", "image", "code"]);
  assert.equal(blocks[1].raw, "D:/My Images/cat.png");
});

test("new-thread send failures restore the draft, stop running and show the actual error", { timeout: 5000 }, async () => {
  const source = read("../src/store.ts");
  const body = source.slice(source.indexOf("export function createThreadOptimistic("), source.indexOf("export const lastUsed"));
  for (const stage of ["create", "send", "switched"]) {
    const state = { currentId: null, items: [], running: {} };
    let draft;
    let toast;
    let finished;
    const done = new Promise((resolve) => { finished = resolve; });
    const noop = () => {};
    const deps = {
      state, PENDING_THREAD_PREFIX: "pending-", zenModeOn: () => false, zenDropPrompt: noop,
      reconcile: (v) => v, bumpChatScrollToBottom: noop,
      setState: (...args) => {
        if (typeof args[0] === "object") Object.assign(state, args[0]);
        else if (args.length === 3) state[args[0]][args[1]] = args[2];
        else state[args[0]] = args[1];
      },
      api: { createThread: async () => {
        if (stage === "create") throw "create failed";
        return { id: "created", items: [], cwd: "D:/repo", agentKind: "codebuddy" };
      } },
      threadEffort: noop, rememberThreadSnapshot: noop,
      lastUsed: { setMode: noop }, reportActivity: noop,
      sendPromptTo: async () => {
        if (stage === "switched") state.currentId = "another-thread";
        throw "尚未配置图片服务，请先使用 /setup-image";
      },
      refreshThreads: noop, refreshProjects: noop, ensureModelOptions: noop,
      optimisticRunningThreads: new Set(["created"]),
      rememberPromptDraft: (text, images) => { draft = { text, images }; },
      setView: noop, showToast: (text) => { toast = text; },
      console: { error: () => finished() },
    };
    const { createThreadOptimistic } = exportsOf(`const { ${Object.keys(deps).join(", ")} } = require("test");\n${body}`, { test: deps });
    createThreadOptimistic("D:/repo", "codebuddy", "model", "build", "", "/generate-image 一只猫", [], false, "");
    await done;
    assert.ok(toast.includes(stage === "create" ? "create failed" : "/setup-image"));
    if (stage !== "create") {
      assert.equal(state.running.created, false);
      assert.equal(deps.optimisticRunningThreads.has("created"), false);
    }
    if (stage === "switched") {
      assert.equal(state.currentId, "another-thread");
      assert.equal(draft, undefined);
    } else {
      assert.equal(state.currentId, null);
      assert.equal(draft.text, "/generate-image 一只猫");
    }
  }
});
