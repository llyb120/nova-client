// 无 API、无磁盘写入的基准编排回归检查。
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { EventEmitter } from "node:events";
import path from "node:path";
import vm from "node:vm";

const source = readFileSync(new URL("./lyra-bench.mjs", import.meta.url), "utf8")
  .replace(/^import .*;\n/gm, "")
  .replace("import.meta.url", JSON.stringify(import.meta.url));
const requests = [];
let report;
await vm.runInNewContext(source, {
  path,
  fileURLToPath: () => new URL("./lyra-bench.mjs", import.meta.url).pathname,
  process: { env: { LYRA_SPECULATE: "off" }, platform: "linux", argv: ["node", "bench", "--cells", "r2", "--runs", "2"] },
  console: { log() {} },
  fs: { writeFileSync(_file, text) { report = JSON.parse(text); } },
  setTimeout(callback, ms) { if (ms === 3000) queueMicrotask(callback); return 0; },
  clearTimeout() {},
  spawn(_exe, _args, { env }) {
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stderr = new EventEmitter();
    child.kill = () => {};
    child.stdin = { write(text) {
      requests.push({ ...JSON.parse(text), speculate: env.LYRA_SPECULATE });
      queueMicrotask(() => child.stdout.emit("data", '{"type":"done"}\n'));
    } };
    return child;
  },
});
assert.equal(requests.length, 4);
assert.ok(requests.every(request => request.mode === "plan"));
assert.deepEqual(requests.map(request => request.speculate), ["off", "on", "on", "off"]);
assert.deepEqual(report.results.map(({ variant, run }) => [variant, run]), [
  ["baseline", 1], ["treatment", 1], ["treatment", 2], ["baseline", 2],
]);
console.log("Lyra benchmark orchestration checks passed");
