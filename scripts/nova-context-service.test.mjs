import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import test from "node:test";
import { callGlobalContextTool } from "./nova-context-client.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";

// fast_context 的唯一实现是 Rust native（JS 镜像已移除）。
const fastContextNative = (args, root) => callNapiTool("fast_context", root, args ?? {});

const execFileAsync = promisify(execFile);
const scriptsDir = dirname(fileURLToPath(import.meta.url));
const bundledService = join(scriptsDir, "..", "src-tauri", "resources", "nova-context-service.mjs");

async function waitForFile(path, timeout = 5000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try { return await readFile(path, "utf8"); } catch {}
    await new Promise((done) => setTimeout(done, 25));
  }
  throw new Error(`timed out waiting for ${path}`);
}

test("fast_context prioritizes resolved targets and lets required units exceed line budget", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-context-priority-"));
  try {
    const body = Array.from({ length: 140 }, (_, index) => `  const value_${index} = ${index};`).join("\n");
    const filler = Array.from({ length: 830 }, (_, index) => `export const PAD_${index} = ${index};`).join("\n");
    await writeFile(join(root, "large_target.ts"), `export function requiredTarget() {\n${body}\n  return value_139;\n}\n${filler}\nexport function unrelatedLaterFlow() { return 1; }\n`);

    const output = await fastContextNative({
      task: "inspect requiredTarget implementation",
      budget: 100,
      maxBytes: 32768,
      _contextMode: "fast",
    }, root);
    assert.match(output, /@@ 1-143 fn requiredTarget \[def\]/);
    assert.match(output, /return value_139;\n}/);
    assert.doesNotMatch(output, /export function unrelatedLaterFlow/);
    assert.match(output, /目标定义: 已闭合/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("fast_context closes third-level dependencies and samples caller behaviors", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-context-graph-"));
  try {
    await writeFile(join(root, "target.ts"), "import { stageOne } from './stageOne';\nexport function targetApi(input) { return stageOne(input); }\n");
    await writeFile(join(root, "stageOne.ts"), "import { stageTwo } from './stageTwo';\nexport function stageOne(input) { return stageTwo(input); }\n");
    await writeFile(join(root, "stageTwo.ts"), "import { stageThree } from './stageThree';\nexport function stageTwo(input) { return stageThree(input); }\n");
    await writeFile(join(root, "stageThree.ts"), "export function stageThree(input) { return `DEPTH_THREE:${input}`; }\n");
    for (let index = 0; index < 4; index += 1) {
      await writeFile(join(root, `duplicate${index}.ts`), `import { targetApi } from './target';\nexport function duplicate${index}(input) { const value = targetApi(input); return value; }\n`);
    }
    await writeFile(join(root, "returnCaller.ts"), "import { targetApi } from './target';\nexport function returnCaller(input) { return targetApi(input); }\n");
    await writeFile(join(root, "awaitCaller.ts"), "import { targetApi } from './target';\nexport async function awaitCaller(input) { return await targetApi(input); }\n");
    await writeFile(join(root, "errorCaller.ts"), "import { targetApi } from './target';\nexport function errorCaller(input) { try { return targetApi(input); } catch { return null; } }\n");

    const output = await fastContextNative({
      keywords: ["targetApi"],
      task: "修改 targetApi 签名并保持调用方兼容",
      budget: 180,
      maxBytes: 12288,
      _contextMode: "fast",
    }, root);
    assert.match(output, /DEPTH_THREE/);
    assert.match(output, /export function returnCaller/);
    assert.match(output, /export async function awaitCaller/);
    assert.match(output, /export function errorCaller/);
    const expanded = output.split("## IMPACT")[0];
    const duplicateBodies = [...expanded.matchAll(/export function duplicate\d/g)].length;
    assert.ok(duplicateBodies <= 2, `duplicate callers consumed budget: ${duplicateBodies}\n${output}`);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("fast_context guarantees explicit caller/test representatives under tight line budget", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-context-obligations-"));
  try {
    const targetBody = Array.from({ length: 120 }, (_, index) => `  const target_${index} = ${index};`).join("\n");
    await writeFile(join(root, "target.ts"), `export function targetApi(input) {\n${targetBody}\n  return input;\n}\n`);
    await writeFile(join(root, "entryCaller.ts"), "import { targetApi } from './target';\nexport function entryCaller(input) { const entry = targetApi(input); return entry; }\n");
    await writeFile(join(root, "errorCaller.ts"), "import { targetApi } from './target';\nexport function errorCaller(input) { try { return targetApi(input); } catch { return null; } }\n");
    await writeFile(join(root, "target.test.ts"), "import { targetApi } from './target';\nexport function targetContract() { return targetApi('x') === 'x'; }\n");

    const output = await fastContextNative({
      keywords: ["targetApi"],
      task: "修改 targetApi 签名，检查调用方和测试",
      budget: 100,
      maxBytes: 12288,
    }, root);
    assert.match(output, /export function entryCaller/);
    assert.match(output, /export function errorCaller/);
    assert.match(output, /export function targetContract/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("different Node processes share one global context service", async (t) => {
  if (!existsSync(bundledService)) {
    t.skip(`bundled service not built: ${bundledService} (run npm run build:nova-context-service)`);
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "nova-context-service-"));
  const endpoint = process.platform === "win32"
    ? `\\\\.\\pipe\\nova-context-test-${process.pid}-${Date.now()}`
    : join(root, "context.sock");
  const readyFile = join(root, "ready");
  const token = `token-${process.pid}-${Date.now()}`;
  await writeFile(join(root, "a.ts"), "export function sharedTarget() { return 1; }\n");
  const previousEndpoint = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  const previousToken = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = endpoint;
  process.env.NOVA_CONTEXT_SERVICE_TOKEN = token;
  const env = {
    ...process.env,
    NOVA_CONTEXT_READY_FILE: readyFile,
    NOVA_CONTEXT_PARENT_PID: String(process.pid),
    NOVA_DATA_DIR: join(root, "data"),
  };
  const service = spawn(process.execPath, [bundledService], {
    env,
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  service.stderr.setEncoding("utf8");
  service.stderr.on("data", (chunk) => { stderr += chunk; });
  try {
    const servicePid = Number.parseInt(await waitForFile(readyFile), 10);
    const clientUrl = pathToFileURL(join(scriptsDir, "nova-context-client.mjs")).href;
    const source = `import { callGlobalContextTool } from ${JSON.stringify(clientUrl)}; console.log((await callGlobalContextTool('ping', process.cwd(), {})).pid);`;
    const [first, second] = await Promise.all([
      execFileAsync(process.execPath, ["--input-type=module", "-e", source], { cwd: root, env }),
      execFileAsync(process.execPath, ["--input-type=module", "-e", source], { cwd: root, env }),
    ]);
    assert.equal(Number.parseInt(first.stdout, 10), servicePid);
    assert.equal(Number.parseInt(second.stdout, 10), servicePid);

    const params = { keywords: ["sharedTarget"] };
    const context = await callGlobalContextTool("fast_context", root, params);
    assert.match(context, /sharedTarget/);
    assert.match(context, /algorithm=no-index-one-pass/);
    assert.match(await fastContextNative({ ...params, _contextMode: "fast" }, root), /sharedTarget/);
  } finally {
    // 先挂 exit 监听再 kill：进程可能已退出（如构建产物缺失），事后挂监听会永等。
    const exited = service.exitCode !== null || service.signalCode !== null
      ? Promise.resolve()
      : new Promise((done) => service.once("exit", done));
    service.kill();
    await exited;
    if (previousEndpoint === undefined) delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
    else process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken === undefined) delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
    else process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
    await rm(root, { recursive: true, force: true });
  }
  assert.equal(stderr, "");
});
