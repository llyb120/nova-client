import assert from "node:assert/strict";
import { createServer } from "node:net";
import { randomUUID } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { createNovaBatchTools } from "./nova-batch-tools.mjs";

test("background tools keep their owner across calls and isolate clients in the same workspace", async () => {
  const endpoint = process.platform === "win32"
    ? `\\\\.\\pipe\\nova-background-test-${randomUUID()}`
    : join(tmpdir(), `nova-${randomUUID()}.sock`);
  const requests = [];
  const server = createServer(socket => {
    let data = "";
    socket.setEncoding("utf8");
    socket.on("data", chunk => {
      data += chunk;
      if (!data.endsWith("\n")) return;
      requests.push(JSON.parse(data));
      socket.end(JSON.stringify({ ok: true, result: {} }) + "\n");
    });
  });
  await new Promise(resolve => server.listen(endpoint, resolve));
  const keys = ["NOVA_CONTEXT_SERVICE_ENDPOINT", "NOVA_CONTEXT_SERVICE_TOKEN"];
  const previous = keys.map(key => process.env[key]);
  try {
    process.env[keys[0]] = endpoint;
    process.env[keys[1]] = "test-token";
    const a = createNovaBatchTools(process.cwd(), { readOnly: false });
    const b = createNovaBatchTools(process.cwd(), { readOnly: false });
    await a.chrome.execute({ operation: "inspect", tabTag: "C1" });
    await b.chrome.execute({ operation: "inspect", tabTag: "C2" });
    await a.chrome.execute({ operation: "act", tabTag: "C1" });
    await a.jianlai.execute({ operation: "windows" });
    assert.ok(requests[0].owner);
    assert.notEqual(requests[0].owner, requests[1].owner);
    assert.equal(requests[0].owner, requests[2].owner);
    assert.equal(requests[0].owner, requests[3].owner);
    assert.equal(requests[0].root, requests[1].root);
  } finally {
    keys.forEach((key, i) => {
      if (previous[i] === undefined) delete process.env[key];
      else process.env[key] = previous[i];
    });
    await new Promise(resolve => server.close(resolve));
  }
});
