import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import test from "node:test";
import { callGlobalContextTool } from "./nova-context-client.mjs";

const execFileAsync = promisify(execFile);
const scriptsDir = dirname(fileURLToPath(import.meta.url));

async function waitForFile(path, timeout = 5000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try { return await readFile(path, "utf8"); } catch {}
    await new Promise((done) => setTimeout(done, 25));
  }
  throw new Error(`timed out waiting for ${path}`);
}

test("different Node processes share one global context service", async () => {
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
  const service = spawn(process.execPath, [join(scriptsDir, "nova-context-service.mjs")], {
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

    const context = await callGlobalContextTool("fast_context", root, { keywords: ["sharedTarget"] });
    assert.match(context, /sharedTarget/);
  } finally {
    service.kill();
    await new Promise((done) => service.once("exit", done));
    if (previousEndpoint === undefined) delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
    else process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken === undefined) delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
    else process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
    await rm(root, { recursive: true, force: true });
  }
  assert.equal(stderr, "");
});
