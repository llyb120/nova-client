import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { createHash, createCipheriv, randomBytes } from "node:crypto";
import { mkdtemp, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { createDevinServer } from "./devin-openai.mjs";
import { decodeWorkBuddyToken } from "./workbuddy-login.cjs";

const buildSecret = "fake WorkBuddy build key";
function sealWorkBuddyToken(value) {
  const key = createHash("sha256").update(buildSecret).digest(), keyId = createHash("sha256").update(key).digest("hex").slice(0,16), nonce = randomBytes(12);
  const u32 = value => { const b=Buffer.alloc(4);b.writeUInt32BE(value);return b; };
  const text = value => Buffer.concat([u32(Buffer.byteLength(value)),Buffer.from(value)]);
  const cipher = createCipheriv("aes-256-gcm",key,nonce);
  cipher.setAAD(Buffer.concat([Buffer.from("WB-AAD\0"),Buffer.from([1]),text("WBEV1"),text("sym-v1"),u32(1),text(keyId),Buffer.from([2,0,0])]));
  const ciphertext = Buffer.concat([cipher.update(value),cipher.final()]);
  return {$wbEncrypted:1,envelope:Buffer.from(JSON.stringify({suite:1,keyId,nonce:nonce.toString("base64"),ciphertext:ciphertext.toString("base64"),authTag:cipher.getAuthTag().toString("base64")})).toString("base64")};
}
test("WorkBuddy encrypted login rejects a wrong key, modified ciphertext and unknown framing",()=>{
  const value=sealWorkBuddyToken("private token");
  assert.equal(decodeWorkBuddyToken(value,buildSecret),"private token");
  assert.throws(()=>decodeWorkBuddyToken(value,"wrong key"));
  const sealed=JSON.parse(Buffer.from(value.envelope,"base64"));sealed.authTag=Buffer.alloc(16).toString("base64");
  assert.throws(()=>decodeWorkBuddyToken({...value,envelope:Buffer.from(JSON.stringify(sealed)).toString("base64")},buildSecret));
  assert.throws(()=>decodeWorkBuddyToken({...value,scheme:"asym-v1"},buildSecret));
});

for (const source of ["codebuddy", "workbuddy"]) test(`${source} native model adapter`, { timeout: 30000 }, async t => {
  const label = source === "workbuddy" ? "WorkBuddy" : "CodeBuddy";
  const root = await mkdtemp(join(tmpdir(), "nova-codebuddy-api-")), authFile = join(root, "auth.info");
  const auth = { auth: { accessToken: "upstream-secret", expiresAt: Date.now() + 60000 }, account: { uid: "test-user" } };
  const saveAuth = () => writeFile(authFile,JSON.stringify(source === "workbuddy" ? {...auth,auth:{...auth.auth,accessToken:sealWorkBuddyToken(auth.auth.accessToken)}} : auth));
  await saveAuth();
  await writeFile(join(root, "settings.json"), JSON.stringify({ model: "test-model" }));
  let last, expectedKey = auth.auth.accessToken, cancelled = 0;
  const upstream = createServer(async (req, res) => {
    try {
      assert.equal(req.headers.authorization, `Bearer ${expectedKey}`);
      assert.ok(req.headers["user-agent"].includes(`${label}/`));
      assert.equal(req.headers["x-product"],label);
      if (req.url === "/v3/config") { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ code: 0, data: {
        agents: [{ name: "cli", models: ["test-model"] }], models: [
          { id: "hidden-model" }, { id: "test-model", name: "Test", maxOutputTokens: 1024, maxInputTokens: 96000, supportsImages: true, supportsToolCall: true, supportsReasoning: true,
            reasoning: { supportedEfforts: ["low", "high"], canDisableThinking: true } }]
      } })); return; }
      assert.equal(req.url, "/v2/chat/completions");
      const parts = []; for await (const part of req) parts.push(part);
      last = JSON.parse(Buffer.concat(parts));
      const mode = last.messages.at(-1).content;
      if (mode === "http-error") { res.writeHead(401); res.end("upstream-secret"); return; }
      res.writeHead(200, { "content-type": "text/event-stream" });
      const send = value => {
        const bytes = Buffer.from(`data: ${JSON.stringify(value)}\r\n\r\n`);
        // Byte splits exercise UTF-8 and SSE framing independently of HTTP chunks.
        for (let i = 0; i < bytes.length; i += 7) res.write(bytes.subarray(i, i + 7));
      };
      const delta = value => send({ choices: [{ index: 0, delta: value, finish_reason: "" }] });
      const tool = (index, id, name, args) => delta({ tool_calls: [{ index, ...(id ? { id, type: "function" } : {}), function: { name, arguments: args } }] });
      if (mode === "hang") { res.once("close", () => cancelled++); return; }
      if (mode === "cancel") { res.once("close", () => cancelled++); delta({ content: "started" }); return; }
      if (mode === "stream-error") { send({ error: { message: "upstream-secret" } }); res.end(); return; }
      if (mode === "truncated") { delta({ content: "partial" }); res.end(); return; }
      if (mode === "tool" || mode === "parallel") {
        delta({ reasoning_content: "需要天气信息" });
        tool(0, "call-a", "weather", '{"city":');
        if (mode === "parallel") tool(1, "call-b", "weather", '{"city":');
        tool(0, null, "", '"上海"}');
        if (mode === "parallel") tool(1, null, "", '"北京"}');
      } else if (mode === "schema") tool(0, "json-call", "__nova_structured_response", '{"result":{"ok":true}}');
      else delta({ content: "你好" });
      send({ choices: [{ index: 0, delta: {}, finish_reason: ["tool", "parallel", "schema"].includes(mode) ? "tool_calls" : "stop" }] });
      send({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28, prompt_tokens_details: { cached_tokens: 5 } } });
      res.end("data: [DONE]\r\n\r\n");
    } catch { res.destroy(); }
  });
  upstream.listen(0, "127.0.0.1"); await once(upstream, "listening");
  const prefix=source.toUpperCase(), env = { ...process.env, [`NOVA_${prefix}_AUTH_FILE`]: authFile, [`${prefix}_CONFIG_DIR`]: root, [`NOVA_${prefix}_ENDPOINT`]: `http://127.0.0.1:${upstream.address().port}` };
  for (const key of ["CODEBUDDY_AUTH_TOKEN", "CODEBUDDY_API_KEY", "CODEBUDDY_MODEL", "WORKBUDDY_AUTH_TOKEN", "WORKBUDDY_API_KEY", "WORKBUDDY_MODEL"]) delete env[key];
  if(source === "workbuddy") {
    await writeFile(join(root,"product.json"),JSON.stringify({authentication:{id:"workbuddy-desktop"},genieVersion:"5.8.2"}));
    const preload=join(root,"fake-workbuddy.cjs");
    await writeFile(preload,`process._linkedBinding = () => ({loggerGet: () => ${JSON.stringify(JSON.stringify({atRestSecretKey:buildSecret}))}});`);
    Object.assign(env,{NOVA_WORKBUDDY_PACKAGE:root,WORKBUDDY_EXE:process.execPath,NODE_OPTIONS:`--require ${JSON.stringify(preload)}`,CODEBUDDY_AUTH_TOKEN:"must-not-use-CodeBuddy-login"});
  }
  const server = createDevinServer({ source, apiKey: "local-key", env, timeoutMs: 1500 });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  const base = `http://127.0.0.1:${server.address().port}/v1`, headers = { authorization: "Bearer local-key", "content-type": "application/json" };
  const post = (content, extra = {}, options = {}) => fetch(`${base}/chat/completions`, { method: "POST", headers, body: JSON.stringify({ model: source, messages: [{ role: "user", content }], ...extra }), ...options });
  const responses = body => fetch(`${base}/responses`, { method: "POST", headers, body: JSON.stringify(body) });
  const tools = [{ type: "function", function: { name: "weather", parameters: { type: "object", properties: { city: { type: "string" } }, required: ["city"] } } }];
  const events = wire => wire.split("\n").filter(line => line.startsWith("data: {")).map(line => JSON.parse(line.slice(6)));
  t.after(async () => { server.shutdown(); upstream.close(); upstream.closeAllConnections(); if (resolve(root).startsWith(resolve(tmpdir()) + sep)) await rm(root, { recursive: true, force: true, maxRetries: 3 }); });

  await t.test("automatic credentials, CLI catalog and real usage", async () => {
    assert.equal((await fetch(`${base}/models`)).status, 401);
    const list = await (await fetch(`${base}/models`, { headers })).json();
    assert.deepEqual(list.data.map(m => m.id), [source, "test-model"]);
    assert.equal(list.data[1].context_length, 96000);
    assert.deepEqual(list.data[1].architecture.input_modalities, ["text", "image"]);
    const result = await (await post("hello", { max_tokens: 1 })).json();
    assert.equal(result.choices[0].message.content, "你好"); assert.equal(result.usage.total_tokens, 28);
    assert.equal(result.usage.prompt_tokens_details.cached_tokens, 5); assert.equal(last.model, "test-model");
    assert.equal((await post("hello", { model: "hidden-model" })).status, 400);
    assert.equal((await post("hello", { reasoning_effort: "max" })).status, 400);
    assert.equal((await post("hello", { max_tokens: 1025 })).status, 400);
  });
  await t.test("images and generation parameters reach native API", async () => {
    const content = [{ type: "image_url", image_url: { url: "data:image/png;base64,YQ==", detail: "high" } }, { type: "text", text: "image" }];
    assert.equal((await post(content, { temperature: 0.4, top_p: 0.8, max_completion_tokens: 100, reasoning_effort: "low" })).status, 200);
    assert.deepEqual(last.messages[0].content, content); assert.equal(last.max_tokens, 100);
    assert.equal(last.temperature, 0.4); assert.equal(last.reasoning_effort, "low");
    assert.equal(last.reasoning_summary,"auto");
    assert.equal((await post("hello",{reasoning_effort:"none"})).status,200); assert.equal(last.reasoning_summary,undefined);
  });
  await t.test("interleaved parallel tool deltas and reasoning roundtrip", async () => {
    const result = await (await post("parallel", { tools, tool_choice: "required" })).json();
    const assistant = result.choices[0].message;
    assert.equal(result.choices[0].finish_reason, "tool_calls");
    assert.deepEqual(assistant.tool_calls.map(call => JSON.parse(call.function.arguments).city), ["上海", "北京"]);
    const messages = [{ role: "user", content: "parallel" }, assistant, ...assistant.tool_calls.map(call => ({ role: "tool", tool_call_id: call.id, content: "sunny" }))];
    assert.equal((await post("", { messages, tools })).status, 200);
    assert.equal(last.messages[1].reasoning_content, "需要天气信息");
    const wire = await (await post("tool", { tools, stream: true, stream_options: { include_usage: true } })).text();
    assert.ok(wire.endsWith("data: [DONE]\n\n"));
    assert.ok(events(wire).some(event => event.choices?.[0]?.delta?.tool_calls?.[0]?.function?.arguments));
    assert.equal(events(wire).at(-1).usage.total_tokens, 28);
  });
  await t.test("Responses history, tool results, structured output and SSE", async () => {
    const result = await (await responses({ input: "tool", tools: tools.map(tool => ({ type: "function", ...tool.function })), tool_choice: "required" })).json();
    assert.equal(result.output[0].name, "weather");
    const next = await responses({ previous_response_id: result.id, input: [{ type: "function_call_output", call_id: result.output[0].call_id, output: "sunny" }] });
    assert.equal(next.status, 200); assert.equal(last.messages[1].reasoning_content, "需要天气信息");
    const schema = await (await post("schema", { response_format: { type: "json_schema", json_schema: { name: "answer", schema: { type: "object", properties: { ok: { type: "boolean" } }, required: ["ok"] } } } })).json();
    assert.deepEqual(JSON.parse(schema.choices[0].message.content), { ok: true });
    const wire = await (await responses({ input: "hello", stream: true })).text();
    assert.ok(wire.includes("response.completed")); assert.ok(wire.includes("response.output_text.delta"));
  });
  await t.test("upstream failures and truncated streams never report success or leak secrets", async () => {
    for (const mode of ["http-error", "stream-error", "truncated"]) {
      const result = await post(mode); assert.ok(result.status >= 400); assert.ok(!(await result.text()).includes("upstream-secret"));
    }
    const wire = await (await post("truncated", { stream: true })).text();
    assert.ok(wire.includes('"error"')); assert.ok(!wire.includes("[DONE]"));
  });
  await t.test("timeout and client disconnect cancel upstream work", async () => {
    assert.equal((await post("hang")).status, 504);
    const abort = new AbortController(), result = await post("cancel", { stream: true }, { signal: abort.signal });
    await result.body.getReader().read(); abort.abort();
    await new Promise(resolve => setTimeout(resolve, 80)); assert.equal(cancelled, 2);
  });
  await t.test("login changes and logout are observed without overwriting CLI state", async () => {
    auth.auth.accessToken = expectedKey = "rotated-secret"; await saveAuth();
    const before = await readFile(authFile,"utf8");
    assert.equal((await post("hello")).status, 200);
    assert.equal(await readFile(authFile,"utf8"),before);
    await writeFile(`${authFile}.logged-out`, "yes"); assert.equal((await post("hello")).status, 503);
  });
  await t.test("managed adapter starts without a key or connection file and exits with owner", async () => {
    const child = spawn(process.execPath, ["scripts/devin-openai.mjs", `--${source}`, "--managed"], { env: { ...env, NOVA_DATA_DIR: root }, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    t.after(() => { if (child.exitCode === null) child.kill(); });
    const [line] = await once(child.stdout, "data"), connection = JSON.parse(line);
    assert.match(connection.baseURL, /^http:\/\/127\.0\.0\.1:\d+\/v1$/); assert.ok(connection.apiKey);
    await assert.rejects(readFile(join(root, `alkaid/${source}-api.json`)));
    const exit = once(child, "exit"); child.stdin.end(); assert.equal((await exit)[0], 0);
  });
});
