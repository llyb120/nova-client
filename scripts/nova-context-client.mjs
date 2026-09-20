import { connect } from "node:net";
import { resolve } from "node:path";
import { randomUUID } from "node:crypto";

// Stable for this tool client, even when another Nova conversation becomes visible.
const owner = randomUUID();

const CONNECT_RETRY_MS = 50;
const CONNECT_TIMEOUT_MS = 3000;
const CALL_TIMEOUT_MS = 120_000;

function serviceConfig() {
  const endpoint = String(process.env.NOVA_CONTEXT_SERVICE_ENDPOINT ?? "").trim();
  const token = String(process.env.NOVA_CONTEXT_SERVICE_TOKEN ?? "");
  return endpoint && token ? { endpoint, token } : null;
}

function wait(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

async function requestOnce(config, method, root, params, clientOwner) {
  return new Promise((resolveResult, reject) => {
    const socket = connect(config.endpoint);
    let response = "";
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error);
      else resolveResult(value);
    };
    const timer = setTimeout(() => finish(new Error(`global context service timed out: ${method}`)), method === "operator" ? 305_000 : ["webview", "chrome"].includes(method) ? 45_000 : ["generate_image", "edit_image"].includes(method) ? 610_000 : CALL_TIMEOUT_MS);
    socket.setEncoding("utf8");
    socket.on("connect", () => {
      socket.write(`${JSON.stringify({ token: config.token, method, root: resolve(root), owner: clientOwner, params: params ?? {} })}\n`);
    });
    socket.on("data", (chunk) => { response += chunk; });
    socket.on("end", () => {
      try {
        const message = JSON.parse(response.trim());
        if (!message.ok) finish(new Error(message.error || "global context service failed"));
        else finish(null, message.result);
      } catch (error) {
        finish(error instanceof Error ? error : new Error(String(error)));
      }
    });
    socket.on("error", (error) => finish(error));
  });
}

export function globalContextServiceConfigured() {
  return serviceConfig() !== null;
}

export async function callGlobalContextTool(method, root, params, clientOwner = owner) {
  const config = serviceConfig();
  if (!config) throw new Error("global context service is not configured");
  const mode = "fast";
  const requestParams = method === "polaris"
    ? { ...(params ?? {}), _contextMode: mode }
    : params;
  const deadline = Date.now() + CONNECT_TIMEOUT_MS;
  for (;;) {
    try {
      return await requestOnce(config, method, root, requestParams, clientOwner);
    } catch (error) {
      if (["operator", "generate_image", "edit_image", "webview", "chrome", "jianlai"].includes(method)) throw error;
      const code = error?.code;
      if (Date.now() >= deadline || !["ENOENT", "ECONNREFUSED", "EPIPE"].includes(code)) throw error;
      await wait(CONNECT_RETRY_MS);
    }
  }
}
