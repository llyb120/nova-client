import { existsSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { callNapiTool } from "./nova-napi-tools.mjs";

const endpoint = String(process.env.NOVA_CONTEXT_SERVICE_ENDPOINT ?? "").trim();
const token = String(process.env.NOVA_CONTEXT_SERVICE_TOKEN ?? "");
const readyFile = String(process.env.NOVA_CONTEXT_READY_FILE ?? "").trim();
const parentPid = Number.parseInt(process.env.NOVA_CONTEXT_PARENT_PID ?? "", 10);

if (!endpoint || !token) {
  console.error("nova-context-service: missing endpoint or token");
  process.exit(2);
}

const isPipe = process.platform === "win32" || endpoint.startsWith("\\\\.\\pipe\\");
if (!isPipe && existsSync(endpoint)) rmSync(endpoint, { force: true });

let serial = Promise.resolve();
let closing = false;

async function dispatch(request) {
  if (!request || request.token !== token) throw new Error("unauthorized context service request");
  const root = String(request.root ?? "");
  const params = request.params ?? {};
  switch (request.method) {
    case "ping":
      return { pid: process.pid, transport: "nova-context-jsonl-v1" };
    case "fast_context":
    case "find_symbols":
    case "code_map":
    case "observe_context_feedback":
      return callNapiTool(request.method, root, params);
    default:
      throw new Error(`unknown context service method: ${request.method}`);
  }
}

function respond(socket, value) {
  if (!socket.destroyed) socket.end(`${JSON.stringify(value)}\n`);
}

const server = createServer((socket) => {
  socket.setEncoding("utf8");
  let input = "";
  let accepted = false;
  socket.on("data", (chunk) => {
    if (accepted) return;
    input += chunk;
    if (input.length > 2 * 1024 * 1024) {
      socket.destroy(new Error("context request too large"));
      return;
    }
    const newline = input.indexOf("\n");
    if (newline < 0) return;
    accepted = true;
    let request;
    try {
      request = JSON.parse(input.slice(0, newline));
    } catch (error) {
      respond(socket, { ok: false, error: error instanceof Error ? error.message : String(error) });
      return;
    }
    // One global queue: every Node bridge shares one context execution and one in-memory index.
    const run = serial.then(() => dispatch(request));
    serial = run.catch(() => {});
    run.then(
      (result) => respond(socket, { ok: true, result }),
      (error) => respond(socket, { ok: false, error: error instanceof Error ? error.message : String(error) }),
    );
  });
  socket.on("error", () => {});
});

function cleanup() {
  if (closing) return;
  closing = true;
  if (readyFile) rmSync(readyFile, { force: true });
  if (!isPipe) rmSync(endpoint, { force: true });
}

server.on("error", (error) => {
  console.error(`nova-context-service: ${error instanceof Error ? error.stack ?? error.message : String(error)}`);
  cleanup();
  process.exit(1);
});
server.on("close", cleanup);
server.listen(endpoint, () => {
  if (readyFile) writeFileSync(readyFile, String(process.pid), "utf8");
});

const parentWatch = Number.isInteger(parentPid) && parentPid > 0
  ? setInterval(() => {
      try {
        process.kill(parentPid, 0);
      } catch {
        server.close(() => process.exit(0));
      }
    }, 2000)
  : null;
parentWatch?.unref();

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}
process.on("exit", cleanup);
