import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { webviewMcpResult } from './webview-mcp-result.mjs';
import {
  createNovaBatchTools,
  normalizePolarisArgs,
  novaDevinBatchToolPolicy,
} from "./nova-batch-tools.mjs";

function withContextService(run) {
  const previousEndpoint = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  const previousToken = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = "test-endpoint";
  process.env.NOVA_CONTEXT_SERVICE_TOKEN = "test-token";
  try { return run(); }
  finally {
    if (previousEndpoint === undefined) delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
    else process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken === undefined) delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
    else process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
  }
}

test('webview sends native MCP images without putting base64 in text; failures preserve action status', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'nova-mcp-image-'));
  try {
    const path = join(dir, 'shot.png');
    const bytes = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZioAAAAASUVORK5CYII=', 'base64');
    await writeFile(path, bytes);
    const text = JSON.stringify({status:'executed',snapshotId:'fresh',images:[{path},{path:join(dir,'missing.png')}]});
    const result = await webviewMcpResult(text);
    assert.equal(result.content[0].text,text);
    assert.equal(result.content[1].type,'image');
    assert.deepEqual(Buffer.from(result.content[1].data,'base64'),bytes);
    assert.match(result.content[2].text,/图片加载失败/);
    assert.equal(JSON.parse(result.content[0].text).status,'executed');
    assert.deepEqual(await webviewMcpResult('not ready'),{content:[{type:'text',text:'not ready'}]});
    const desktop = await webviewMcpResult(JSON.stringify({images:Array.from({length:16},(_,i)=>({path,imageId:`monitor-${i}`}))}));
    assert.equal(desktop.content.filter(part=>part.type==='image').length,16);
  } finally { await rm(dir,{recursive:true,force:true}); }
});

test("createNovaBatchTools exposes context tools only with the native service", () => {
  const previousEndpoint = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  const previousToken = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  try { assert.deepEqual(createNovaBatchTools(process.cwd(), { fastContext: true }), {}); }
  finally {
    if (previousEndpoint !== undefined) process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken !== undefined) process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
  }
  const tools = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true }));
  assert.deepEqual(Object.keys(tools).sort(), ["chrome", "edit_image", "generate_image", "jianlai", "operator", "polaris", "webview"]);
});

test("NOVA_FAST_CONTEXT=0 omits context tools", () => {
  const previous = process.env.NOVA_FAST_CONTEXT;
  process.env.NOVA_FAST_CONTEXT = "0";
  try {
    const tools = withContextService(() => createNovaBatchTools(process.cwd()));
    assert.deepEqual(Object.keys(tools).sort(), ["chrome", "edit_image", "generate_image", "jianlai", "operator", "webview"]);
    assert.deepEqual(withContextService(() => createNovaBatchTools(process.cwd(), { readOnly: true })), {});
  }
  finally {
    if (previous === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = previous;
  }
});

test("context argument aliases normalize", () => {
  assert.deepEqual(normalizePolarisArgs({ query: "Widget" }).keywords, ["Widget"]);
});

test("polaris keywords normalize to top five", () => {
  assert.deepEqual(normalizePolarisArgs({ keywords: "Widget" }).keywords, ["Widget"]);
  assert.deepEqual(
    normalizePolarisArgs({ keywords: ["a", "b", "a", "c", "d", "e", "f"] }).keywords,
    ["a", "b", "c", "d", "e"],
  );
  const schema = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true })).polaris.inputSchema.properties.keywords;
  assert.equal(schema.maxItems, undefined);
  assert(schema.anyOf.some((option) => option.type === "string"));
});

test("CodeBuddy direct mode describes polaris as a direct tool", () => {
  const previous = process.env.NOVA_MCP_DIRECT;
  process.env.NOVA_MCP_DIRECT = "1";
  try {
    const tool = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true }).polaris);
    assert.match(tool.description, /直接调用/);
    assert.doesNotMatch(tool.description, /mcp_call_tool/);
    assert.doesNotMatch(tool.description, /ToolSearch|DeferExecuteTool/);
  } finally {
    if (previous === undefined) delete process.env.NOVA_MCP_DIRECT;
    else process.env.NOVA_MCP_DIRECT = previous;
  }
});

test("desktop optimizations are shared by custom tools and MCP, not Lyra-only", async () => {
  const { jianlai } = withContextService(() => createNovaBatchTools(process.cwd()));
  for (const name of ["jianlai", "chrome"]) {
    const tool = withContextService(() => createNovaBatchTools(process.cwd()))[name];
    for (const operation of ["experience_search", "experience_save", "experience_feedback"]) {
      assert(tool.inputSchema.properties.operation.enum.includes(operation));
    }
    assert.deepEqual(tool.inputSchema.properties.experience.required, ["scope", "task"]);
    assert.match(tool.description, /经验按需使用/);
    assert.match(tool.description, /首次观察核对conditions/);
    assert.match(tool.description, /executed不代表业务成功/);
  }
  assert(jianlai.inputSchema.properties.operation.enum.includes('recall'));
  assert.equal(jianlai.inputSchema.properties.actions.items.properties.ms.maximum, 2000);
  assert.match(jianlai.description, /notes/);
  assert.match(jianlai.description, /先.*触发测试/);
  const dir = await mkdtemp(join(tmpdir(), 'nova-desktop-mcp-'));
  try {
    const path = join(dir, 'shot.png');
    await writeFile(path, Buffer.from('PNG'));
    const result = await webviewMcpResult(JSON.stringify({ source: 'jianlai', status: 'not_executed',
      error: '前台程序已改变', snapshotId: 'new', notes: '已读第1页，测速未确认',
      operation: 'act', basedOnSnapshotId: 'old', observationSequence: 2, historical: false,
      verification: 'unverified',
      images: [{ path, snapshotId: 'new', capturedAt: '2026-09-17T00:00:00Z', stability: { status: 'not_checked', samples: 1 } }] }));
    const text = JSON.parse(result.content[0].text);
    assert.equal(text.status, 'not_executed');
    assert.equal(text.snapshotId, 'new');
    assert.equal(text.basedOnSnapshotId, 'old');
    assert.equal(text.operation, 'act');
    assert.equal(text.observationSequence, 2);
    assert.equal(text.historical, false);
    assert.equal(text.verification, 'unverified');
    assert.equal(text.images[0].snapshotId, 'new');
    assert.equal(text.images[0].capturedAt, '2026-09-17T00:00:00Z');
    assert.deepEqual(text.images[0].stability, { status: 'not_checked', samples: 1 });
    assert.equal(text.notes, '已读第1页，测速未确认');
    assert.equal(text.imageHistoryPolicy, undefined);
    assert(!jianlai.description.includes('最近2次'));
    assert.equal(text.images[0].path, path);
    assert.equal(result.content[1].type, 'image');
    assert.equal(text.deliveredImageBytes, 3);
    assert(text.deliveryTimingsMs.total >= 0);
    assert(!result.content[0].text.includes('UE5H'));
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test("devin policy routes context through MCP when the native service exists", () => {
  const policy = withContextService(() => novaDevinBatchToolPolicy({ fastContext: true }));
  assert.match(policy, /mcp_call_tool/);
  assert.match(policy, /polaris/);
  assert.match(policy, /Devin native edit tools/);
});

test("directory switching is session scoped, works without polaris, and updates its root only on success", async () => {
  const endpoint = process.platform === "win32"
    ? `\\\\.\\pipe\\nova-cwd-test-${process.pid}`
    : join(tmpdir(), `nova-cwd-test-${process.pid}.sock`);
  const previous = { ...process.env };
  const calls = [];
  const target = resolve("new project");
  const server = createServer((socket) => {
    let line = "";
    socket.on("data", (chunk) => {
      line += chunk;
      if (!line.includes("\n")) return;
      const request = JSON.parse(line.trim());
      calls.push(request);
      if (request.params.prompt === "lost response" || ["webview", "chrome", "jianlai", "operator"].includes(request.method)) { socket.destroy(); return; }
      const rejected = request.params.path === "missing";
      socket.end(JSON.stringify(rejected
        ? { ok: false, error: "directory missing" }
        : { ok: true, result: request.method === "polaris" ? "context" : request.method.endsWith("_image") ? { path: "C:/images/result.png" } : { cwd: target, changed: true } }) + "\n");
    });
  });
  try {
    await new Promise((done, reject) => { server.once("error", reject); server.listen(endpoint, done); });
    process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = endpoint;
    process.env.NOVA_CONTEXT_SERVICE_TOKEN = "secret";
    delete process.env.NOVA_CWD_CHANGE_SCOPE;
    assert.equal(createNovaBatchTools(process.cwd()).change_working_directory, undefined);
    process.env.NOVA_CWD_CHANGE_SCOPE = "session-A";
    const tools = createNovaBatchTools(process.cwd(), { fastContext: true });
    assert(createNovaBatchTools(process.cwd(), { fastContext: false }).change_working_directory);
    await assert.rejects(tools.change_working_directory.execute({ path: " " }), /缺少 path/);
    assert.equal(calls.length, 0);
    await assert.rejects(tools.change_working_directory.execute({ path: "missing" }), /directory missing/);
    await tools.polaris.execute({ query: "Widget" });
    assert.equal(calls.at(-1).root, process.cwd());
    await tools.change_working_directory.execute({ path: "../new project" });
    assert.deepEqual(calls.at(-1).params, { scope: "session-A", path: "../new project" });
    await tools.polaris.execute({ query: "Widget" });
    assert.equal(calls.at(-1).root, target);
    const args = { prompt: "blue background", referenceImages: ["original.png"] };
    assert.deepEqual(JSON.parse(await tools.edit_image.execute(args)), { path: "C:/images/result.png" });
    assert.equal(calls.at(-1).method, "edit_image");
    assert.equal(calls.at(-1).root, target);
    assert.deepEqual(calls.at(-1).params, args);
    const before = calls.length;
    await assert.rejects(tools.generate_image.execute({ prompt: "lost response" }));
    assert.equal(calls.length, before + 1);
    const webviewArgs = { operation: "act", browserId: "test-browser", snapshotId: "test-snapshot", action: { action: "press", key: "Enter" } };
    await assert.rejects(tools.webview.execute(webviewArgs));
    assert.equal(calls.length, before + 2, "browser mutations must not retry a lost response");
    assert.equal(calls.at(-1).method, "webview");
    assert.deepEqual(calls.at(-1).params, webviewArgs);
    const chromeArgs = { operation: "act", tabTag: "C1-test", snapshotId: "test-snapshot", action: { action: "press", key: "Enter" } };
    await assert.rejects(tools.chrome.execute(chromeArgs));
    assert.equal(calls.length, before + 3, "Chrome mutations must not retry a lost response");
    assert.equal(calls.at(-1).method, "chrome");
    assert.deepEqual(calls.at(-1).params, chromeArgs);
    const desktopArgs = {operation:'act',snapshotId:'test-snapshot',imageId:'monitor-1',actions:[{action:'press',key:'Enter'}]};
    await assert.rejects(tools.jianlai.execute(desktopArgs));
    assert.equal(calls.length,before+4,'desktop input must never retry a lost response');
    assert.equal(calls.at(-1).method,'jianlai');
    assert.deepEqual(calls.at(-1).params,desktopArgs);
    const operatorArgs={goal:'finish the current UI task',maxSeconds:10};
    await assert.rejects(tools.operator.execute(operatorArgs));
    assert.equal(calls.length,before+5,'operator mutations must never retry a lost response');
    assert.equal(calls.at(-1).method,'operator');
    assert.deepEqual(calls.at(-1).params,operatorArgs);
  } finally {
    for (const key of ["NOVA_CONTEXT_SERVICE_ENDPOINT", "NOVA_CONTEXT_SERVICE_TOKEN", "NOVA_CWD_CHANGE_SCOPE"]) {
      if (previous[key] === undefined) delete process.env[key];
      else process.env[key] = previous[key];
    }
    await new Promise((done) => server.close(done));
  }
});
