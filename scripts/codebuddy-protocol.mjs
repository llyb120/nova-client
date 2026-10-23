import { readFile, access, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { failure, LIMIT } from "./devin-protocol.mjs";

const authError = message => failure(503, message, "codebuddy_auth_error");
async function jsonFile(path, optional = false) {
  try { return JSON.parse(await readFile(path, "utf8")); }
  catch (error) { if (optional && error.code === "ENOENT") return {}; throw authError("Cannot read CodeBuddy configuration or login."); }
}

export function createCodeBuddyClient(env = process.env, source = "codebuddy") {
  const workbuddy = source === "workbuddy", label = workbuddy ? "WorkBuddy" : "CodeBuddy";
  const home = env.USERPROFILE || env.HOME || homedir();
  const configDir = env[workbuddy ? "WORKBUDDY_CONFIG_DIR" : "CODEBUDDY_CONFIG_DIR"] || join(home, `.${source}`);
  const dataDir = process.platform === "win32" ? join(home, "AppData/Local") : process.platform === "darwin" ? join(home, "Library/Application Support") : join(home, ".local/share");
  const authPath = env[`NOVA_${source.toUpperCase()}_AUTH_FILE`] || join(dataDir, `CodeBuddyExtension/Data/Public/auth/${workbuddy ? "workbuddy-desktop" : "Tencent-Cloud.coding-copilot"}.info`);
  let catalog, catalogTime = 0, catalogIdentity, packageInfo, unlocked;
  async function installed() {
    if (packageInfo) return packageInfo;
    if (workbuddy) {
      const exe = env.WORKBUDDY_EXE || join(env.LOCALAPPDATA || join(home, "AppData/Local"), "Programs/WorkBuddy/WorkBuddy.exe");
      const root = env.NOVA_WORKBUDDY_PACKAGE || join(dirname(exe), "resources/app.asar.unpacked/cli");
      const product = await jsonFile(join(root, "product.json"));
      if (product.authentication?.id !== "workbuddy-desktop") throw authError("Select WorkBuddy's bundled CLI, not the global CodeBuddy package.");
      return packageInfo = { root, exe, version: product.genieVersion || "5.8.2" };
    }
    const roots = [env.NOVA_CODEBUDDY_PACKAGE, env.APPDATA && join(env.APPDATA, "npm/node_modules/@tencent-ai/codebuddy-code"),
      ...(env.PATH || env.Path || "").split(delimiter).filter(Boolean).flatMap(path => [join(path, "node_modules/@tencent-ai/codebuddy-code"), join(path, "../lib/node_modules/@tencent-ai/codebuddy-code")])].filter(Boolean);
    for (const root of new Set(roots)) {
      try {
        const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
        if (pkg.name === "@tencent-ai/codebuddy-code") return packageInfo = { root, version: pkg.version };
      } catch { /* Native installations can use the cloud catalog without npm metadata. */ }
    }
    // ponytail: native installs lack package metadata; update this protocol version when CLI headers change.
    return packageInfo = { version: "2.169.0" };
  }
  async function credentials() {
    const settings = await jsonFile(join(configDir, "settings.json"), true), vars = { ...env, ...settings.env };
    const explicitKey = workbuddy ? env.WORKBUDDY_AUTH_TOKEN || env.WORKBUDDY_API_KEY || settings.env?.CODEBUDDY_AUTH_TOKEN || settings.env?.CODEBUDDY_API_KEY : vars.CODEBUDDY_AUTH_TOKEN || vars.CODEBUDDY_API_KEY;
    let session = {};
    if (!explicitKey) {
      try { await access(`${authPath}.logged-out`); throw authError("CodeBuddy is logged out. Sign in with CodeBuddy CLI first."); }
      catch (error) { if (error.code !== "ENOENT") throw error; }
      session = await jsonFile(authPath);
    }
    let key = explicitKey || session.auth?.accessToken;
    const { version, exe } = await installed();
    if (workbuddy && key && typeof key === "object") {
      const identity = JSON.stringify([key, exe, (await stat(exe)).mtimeMs]);
      if (unlocked?.identity !== identity) {
        const script = await readFile(new URL("./workbuddy-login.cjs", import.meta.url), "utf8");
        let result;
        try { result = await promisify(execFile)(exe, ["-e", script, "--", "--nova-workbuddy-login", authPath], { env: { ...env, ELECTRON_RUN_AS_NODE: "1" }, windowsHide: true, timeout: 15000, maxBuffer: 65536 }); }
        catch { throw authError("Cannot unlock WorkBuddy login. Open WorkBuddy and sign in again."); }
        const login = JSON.parse(result.stdout);
        if (JSON.stringify(login.envelope) !== JSON.stringify(key)) throw authError("WorkBuddy login changed while reading it. Retry the request.");
        unlocked = { identity, key: login.accessToken };
      }
      key = unlocked.key;
    }
    if (typeof key !== "string" || !key.trim()) throw authError("CodeBuddy login is missing or encrypted. Use a supported CLI login or CODEBUDDY_AUTH_TOKEN.");
    if (!explicitKey && session.auth?.expiresAt <= Date.now()) throw authError(`${label} login has expired. Sign in again, then retry.`);
    const endpoint = env[`NOVA_${source.toUpperCase()}_ENDPOINT`] || (session.auth?.domain ? `https://${session.auth.domain}` : workbuddy ? "https://www.workbuddy.cn" : "https://www.codebuddy.ai");
    let url;
    try { url = new URL(endpoint); } catch { throw authError("Invalid CodeBuddy endpoint."); }
    if ((url.protocol !== "https:" && !(url.protocol === "http:" && ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname))) || url.username || url.password || url.search || url.hash || url.pathname !== "/") throw authError("CodeBuddy endpoint must be an HTTPS origin (HTTP is allowed for loopback tests).");
    const headers = { Authorization: `Bearer ${key}`, "User-Agent": `CLI/${version} ${label}/${version}`, "X-IDE-Type": workbuddy ? "WorkBuddy" : "CLI", "X-IDE-Version": version,
      "X-Product": label, "X-Agent-Intent": "craft", "X-Agent-Runtime": "local-agent", "Content-Type": "application/json" };
    if (session.account?.uid) headers["X-User-Id"] = session.account.uid;
    if (session.account?.enterpriseId) headers["X-Enterprise-Id"] = session.account.enterpriseId;
    return { url: url.origin, headers, defaultModel: env[`${source.toUpperCase()}_MODEL`] || settings.model, identity: `${url.origin}:${key}:${session.account?.uid || ""}:${session.account?.enterpriseId || ""}` };
  }
  async function models(signal) {
    const auth = await credentials();
    if (catalog && catalogIdentity === auth.identity && Date.now() - catalogTime < 60000) return catalog;
    const response = await fetch(`${auth.url}/v3/config`, { headers: auth.headers, signal, redirect: "error" });
    if (!response.ok) throw failure([401, 403, 429].includes(response.status) ? response.status : 502, `CodeBuddy model catalog returned HTTP ${response.status}.`, "codebuddy_models_error");
    const config = await response.json();
    if (config.code !== 0 || !Array.isArray(config.data?.models)) throw failure(502, "Invalid CodeBuddy model catalog.", "codebuddy_models_error");
    const { root } = await installed();
    let defaults = [];
    if (root) {
      const base = await jsonFile(join(root, "product.json"), true);
      const regional = auth.url.includes("copilot.tencent.com") || auth.url.includes("codebuddy.cn") ? await jsonFile(join(root, "product.internal.json"), true) : {};
      defaults = [...(base.models || []), ...(regional.models || [])];
    }
    const merged = new Map();
    for (const model of [...defaults, ...config.data.models]) if (typeof model.id === "string") merged.set(model.id, { ...merged.get(model.id), ...model });
    const allowed = config.data.agents?.find(agent => agent.name === "cli")?.models;
    const ids = Array.isArray(allowed) ? allowed : config.data.models.map(model => model.id);
    catalog = ids.filter(id => typeof id === "string").map(id => {
      const model = merged.get(id) || { id };
      return { slug: id, variants: [{ model_uid: id, label: model.name || id, max_context_tokens: model.contextWindow?.defaultLength || model.maxAllowedSize || model.maxInputTokens,
        max_output_tokens: model.maxOutputTokens, model }] };
    });
    catalogIdentity = auth.identity; catalogTime = Date.now();
    return catalog;
  }
  async function resolveModel(requested, effort, signal) {
    const families = await models(signal), auth = await credentials();
    const id = requested === source ? auth.defaultModel || families[0]?.slug : requested;
    const model = families.find(family => family.slug === id)?.variants[0].model;
    if (!model) throw failure(400, "Unknown CodeBuddy model. Refresh /v1/models.", "model_not_found");
    if (effort && model.reasoning?.supportedEfforts && !(effort === "none" && model.reasoning.canDisableThinking) && !model.reasoning.supportedEfforts.includes(effort)) throw failure(400, "This CodeBuddy model does not support the requested reasoning effort.");
    return id;
  }
  async function* inference(body, auth, _version, signal) {
    const model = catalog?.find(family => family.slug === body.model)?.variants[0].model;
    const request = { ...body, stream: true, stream_options: { include_usage: true } };
    if (request.reasoning_summary == null && !["none", "off"].includes(request.reasoning_effort) && (model?.supportsReasoning || typeof request.reasoning_effort === "string")) request.reasoning_summary = "auto";
    for (const key of ["n", "store", "metadata", "prompt_cache_key", "response_format"]) delete request[key];
    if (request.max_completion_tokens !== undefined) { request.max_tokens = request.max_completion_tokens; delete request.max_completion_tokens; }
    if (!request.max_tokens && model?.maxOutputTokens) request.max_tokens = model.maxOutputTokens;
    if (model?.maxOutputTokens && request.max_tokens > model.maxOutputTokens) throw failure(400, "Token limit exceeds this CodeBuddy model's output limit.");
    if (model?.supportsToolCall === false && request.tools.length) throw failure(400, "This CodeBuddy model does not support tools.");
    if (model?.supportsImages === false && request.messages.some(message => Array.isArray(message.content) && message.content.some(part => part.type === "image_url"))) throw failure(400, "This CodeBuddy model does not support images.");
    const response = await fetch(`${auth.url}/v2/chat/completions`, { method: "POST", headers: { ...auth.headers, "X-Request-ID": randomUUID(), "X-Conversation-ID": randomUUID() }, body: JSON.stringify(request), signal, redirect: "error" });
    if (!response.ok) { await response.body?.cancel(); throw failure([400, 401, 403, 429].includes(response.status) ? response.status : 502, `CodeBuddy upstream returned HTTP ${response.status}.`, "upstream_error"); }
    if (!response.headers.get("content-type")?.startsWith("text/event-stream")) { await response.body?.cancel(); throw failure(502, "CodeBuddy did not return an event stream.", "upstream_protocol_error"); }
    const reader = response.body.getReader(), decoder = new TextDecoder(), calls = new Map();
    let pending = "", data = [], bytes = 0, finished = false, done = false;
    try {
      while (!done) {
        const chunk = await reader.read();
        pending += chunk.done ? decoder.decode() : decoder.decode(chunk.value, { stream: true });
        bytes += chunk.value?.length || 0;
        if (bytes > LIMIT) throw failure(502, "CodeBuddy output exceeds 32 MiB.", "output_limit");
        let newline;
        while ((newline = pending.indexOf("\n")) >= 0) {
          const line = pending.slice(0, newline).replace(/\r$/, ""); pending = pending.slice(newline + 1);
          if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
          if (line !== "" || !data.length) continue;
          const payload = data.join("\n"); data = [];
          if (payload === "[DONE]") { done = true; break; }
          let event;
          try { event = JSON.parse(payload); } catch { throw failure(502, "Invalid CodeBuddy stream JSON.", "upstream_protocol_error"); }
          if (event.error || (event.code !== undefined && event.code !== 0)) throw failure(502, "CodeBuddy reported an upstream error.", "upstream_error");
          for (const choice of event.choices || []) {
            if (choice.index !== undefined && choice.index !== 0) throw failure(502, "Unexpected CodeBuddy choice index.", "upstream_protocol_error");
            const delta = choice.delta || {};
            if (delta.content) yield { type: "text", text: delta.content };
            if (delta.reasoning_content) yield { type: "reasoning", text: delta.reasoning_content };
            for (const call of delta.tool_calls || []) {
              if (!Number.isInteger(call.index) || call.index < 0) throw failure(502, "Invalid CodeBuddy tool index.", "upstream_protocol_error");
              if (call.id) calls.set(call.index, call.id);
              const id = calls.get(call.index);
              if (!id) throw failure(502, "CodeBuddy tool arguments arrived without an ID.", "upstream_protocol_error");
              yield { type: "tool", id, name: call.function?.name || "", arguments: call.function?.arguments || "" };
            }
            if (choice.finish_reason) {
              const reason = { stop: 1, length: 3, tool_calls: 10 }[choice.finish_reason];
              if (reason === undefined) throw failure(502, `Unsupported CodeBuddy finish reason: ${choice.finish_reason}.`, "upstream_protocol_error");
              finished = true; yield { type: "stop", reason };
            }
          }
          if (event.usage) yield { type: "usage", usage: { ...event.usage, prompt_tokens_details: { cached_tokens: event.usage.prompt_tokens_details?.cached_tokens || event.usage.prompt_cache_hit_tokens || 0 } } };
        }
        if (chunk.done) break;
      }
      if (!done || !finished) throw failure(502, "CodeBuddy stream ended before completion.", "upstream_protocol_error");
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  }
  return { credentials, models, resolveModel, inference };
}
