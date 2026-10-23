import http, { createServer } from "node:http";
import { execFile } from "node:child_process";
import { randomBytes, randomUUID, timingSafeEqual } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { AjvJsonSchemaValidator } from "@modelcontextprotocol/sdk/validation/ajv";
import { failure, inference, LIMIT } from "./devin-protocol.mjs";
import { createCodeBuddyClient } from "./codebuddy-protocol.mjs";

const runFile = promisify(execFile);
const object = value => value !== null && typeof value === "object" && !Array.isArray(value);
const namePattern = /^[a-zA-Z0-9_-]{1,64}$/;
const structuredName = "__nova_structured_response";
const requireThat = (condition, message) => { if (!condition) throw failure(400, message); };

function contentParts(content, role, source) {
  if (typeof content === "string" || (content === null && role === "assistant")) return content;
  requireThat(Array.isArray(content), "Message content must be text or a content-part array.");
  return content.map(part => {
    if (part?.type === "text" && typeof part.text === "string") return { type: "text", text: part.text };
    requireThat(!["system", "developer", "assistant"].includes(role) && part?.type === "image_url" && typeof part.image_url?.url === "string", "Only text and user/tool image_url content parts are supported.");
    requireThat((source !== "devin" ? [undefined, "auto", "low", "high"] : [undefined, "auto"]).includes(part.image_url.detail), "Unsupported image detail selection; use auto.");
    return { type: "image_url", image_url: { ...part.image_url } };
  });
}
function validator(schema) {
  requireThat(object(schema) && JSON.stringify(schema).length <= 65536, "Tool/response schema must be an object of at most 64 KiB.");
  try { return new AjvJsonSchemaValidator().getValidator(schema); }
  catch { throw failure(400, "Invalid or unsupported JSON Schema."); }
}
function validate(body, source = "devin") {
  requireThat(object(body), "Expected a JSON object.");
  const supported = new Set(["model", "messages", "stream", "stream_options", "n", "user", "tools", "tool_choice", "parallel_tool_calls", "temperature", "top_p", "max_tokens", "max_completion_tokens", "stop", "reasoning_effort", "response_format", "metadata", "store", "prompt_cache_key"]);
  for (const key of Object.keys(body)) requireThat(supported.has(key), `Unsupported parameter: ${key}`);
  // Cache routing is owned by Devin; accept the client's optional hint without forwarding it.
  requireThat(body.prompt_cache_key === undefined || (typeof body.prompt_cache_key === "string" && body.prompt_cache_key.length <= 64), "prompt_cache_key must be a string of at most 64 characters.");
  requireThat(typeof body.model === "string" && body.model.trim() && body.model.length <= 256, `model must be a model ID or ${source}.`);
  for (const key of ["stream", "parallel_tool_calls", "store"]) requireThat(body[key] === undefined || typeof body[key] === "boolean", `${key} must be boolean.`);
  requireThat(body.n === undefined || (Number.isInteger(body.n) && body.n >= 1 && body.n <= 4), "n must be between 1 and 4.");
  for (const [key, max] of [["temperature", 2], ["top_p", 1]]) requireThat(body[key] === undefined || (typeof body[key] === "number" && Number.isFinite(body[key]) && body[key] >= 0 && body[key] <= max), `${key} must be between 0 and ${max}.`);
  const minimumTokens = source !== "devin" ? 1 : 32, maximumTokens = source !== "devin" ? 1048576 : 128000;
  for (const key of ["max_tokens", "max_completion_tokens"]) requireThat(body[key] === undefined || (Number.isInteger(body[key]) && body[key] >= minimumTokens && body[key] <= maximumTokens), `${key} must be between ${minimumTokens} and ${maximumTokens}.`);
  requireThat(body.max_tokens === undefined || body.max_completion_tokens === undefined, "Specify only one token limit.");
  requireThat(body.reasoning_effort === undefined || ["none", "minimal", "low", "medium", "high", "xhigh", "max"].includes(body.reasoning_effort), "Invalid reasoning_effort.");
  if (body.stream_options !== undefined) requireThat(body.stream && object(body.stream_options) && Object.keys(body.stream_options).every(key => key === "include_usage") && [undefined,true,false].includes(body.stream_options.include_usage), "Invalid stream_options.");
  const stops = body.stop == null ? [] : typeof body.stop === "string" ? [body.stop] : body.stop;
  requireThat(Array.isArray(stops) && stops.length <= 4 && stops.every(stop => typeof stop === "string" && stop.length > 0 && stop.length <= 1024), "stop must contain one to four non-empty strings of at most 1024 characters.");
  body.stop = stops;
  requireThat(Array.isArray(body.messages) && body.messages.length, "messages must be non-empty.");
  const pending = new Set(), seen = new Set();
  body.messages = body.messages.map(message => {
    requireThat(object(message) && ["system","developer","user","assistant","tool"].includes(message.role), "Invalid message role.");
    requireThat(Object.keys(message).every(key => ["role","content","tool_calls","tool_call_id","name","reasoning_content"].includes(key)), "Unsupported message field.");
    requireThat(!pending.size || message.role === "tool", "Every assistant tool call must have a tool result before the next message.");
    const result = { role: message.role, content: contentParts(message.content ?? (message.tool_calls ? null : undefined), message.role, source) };
    if (message.reasoning_content !== undefined) {
      requireThat(typeof message.reasoning_content === "string", "reasoning_content must be text.");
      result.reasoning_content = message.reasoning_content;
    }
    if (message.tool_calls !== undefined) {
      requireThat(message.role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0, "tool_calls must be a non-empty assistant array.");
      result.tool_calls = message.tool_calls.map(call => {
        requireThat(call?.type === "function" && typeof call.id === "string" && call.id && !seen.has(call.id) && typeof call.function?.name === "string" && namePattern.test(call.function.name) && typeof call.function.arguments === "string", "Invalid or duplicate function call.");
        try { requireThat(object(JSON.parse(call.function.arguments)), "Tool arguments must be a JSON object."); } catch { throw failure(400, "Tool arguments must be a JSON object."); }
        seen.add(call.id); pending.add(call.id);
        return { id: call.id, type: "function", function: { name: call.function.name, arguments: call.function.arguments } };
      });
    }
    if (message.role === "tool") {
      requireThat(pending.delete(message.tool_call_id), "Unknown or duplicate tool_call_id."); result.tool_call_id = message.tool_call_id;
    } else requireThat(message.tool_call_id === undefined, "tool_call_id requires role=tool.");
    return result;
  });
  requireThat(!pending.size, "Missing tool results.");
  requireThat(["user","tool"].includes(body.messages.at(-1).role), "The final message must be user or tool.");
  requireThat(body.tools === undefined || (Array.isArray(body.tools) && body.tools.length <= 128), "tools must be an array of at most 128 functions.");
  const checks = new Map();
  body.tools = (body.tools || []).map(tool => {
    requireThat(tool?.type === "function" && object(tool.function) && typeof tool.function.name === "string" && namePattern.test(tool.function.name) && tool.function.name !== structuredName && !checks.has(tool.function.name), "Invalid, reserved or duplicate tool name.");
    requireThat(tool.function.description === undefined || typeof tool.function.description === "string", "Tool description must be text.");
    requireThat([undefined,true,false].includes(tool.function.strict), "strict must be boolean.");
    const parameters = tool.function.parameters || { type: "object", properties: {} };
    requireThat(parameters.type === "object", "Function parameters must use an object schema.");
    checks.set(tool.function.name, validator(parameters));
    return { type: "function", function: { ...tool.function, parameters } };
  });
  const choice = body.tool_choice ?? (body.tools.length ? "auto" : "none");
  requireThat(["auto","none","required"].includes(choice) || (choice?.type === "function" && checks.has(choice.function?.name)), "Invalid tool_choice.");
  requireThat(choice !== "required" || body.tools.length, "tool_choice=required needs tools.");
  body.tool_choice = choice;
  if (choice === "none") body.tools = [];
  else if (object(choice)) body.tools = body.tools.filter(tool => tool.function.name === choice.function.name);
  let structured;
  if (body.response_format && body.response_format.type !== "text") {
    const format = body.response_format;
    requireThat(["json_object","json_schema"].includes(format.type), "Invalid response_format.");
    const schema = format.type === "json_object" ? { type: "object" } : format.json_schema?.schema;
    structured = validator(schema);
    if (choice !== "required" && !object(choice)) {
      const parameters = { type: "object", properties: { result: schema }, required: ["result"], additionalProperties: false };
      checks.set(structuredName, validator(parameters));
      body.tools.push({ type: "function", function: { name: structuredName, description: "Return the final response in the requested JSON structure.", parameters } });
      body.tool_choice = "required";
    }
  }
  return { checks, structured };
}

function responseInput(input) {
  if (typeof input === "string") return [{ role: "user", content: input }];
  requireThat(Array.isArray(input), "Responses input must be text or an array.");
  const messages = [];
  const parts = (content, role) => typeof content === "string" ? content : content?.map(part => {
    if (["input_text","output_text"].includes(part.type)) return { type: "text", text: part.text };
    if (part.type === "input_image") return { type: "image_url", image_url: { url: part.image_url, detail: part.detail } };
    throw failure(400, `Unsupported Responses content: ${part.type}`);
  });
  for (const item of input) {
    if (item?.type === "function_call") {
      const call = { id: item.call_id, type: "function", function: { name: item.name, arguments: item.arguments } };
      if (messages.at(-1)?.tool_calls) messages.at(-1).tool_calls.push(call);
      else messages.push({ role: "assistant", content: null, tool_calls: [call] });
    } else if (item?.type === "function_call_output") messages.push({ role: "tool", tool_call_id: item.call_id, content: parts(item.output, "tool") });
    else {
      requireThat(object(item) && [undefined,"message"].includes(item.type), "Unsupported Responses input item.");
      messages.push({ role: item.role, content: parts(item.content, item.role) });
    }
  }
  return messages;
}

async function complete(body, credentials, version, signal, checks, structured, onDelta, runInference = inference, label = "Devin") {
  const calls = [], callIndexes = new Map(); let current = -1, text = "", thinking = "", tail = "", usage = null, finish = "stop";
  const keep = Math.max(0, ...body.stop.map(stop => stop.length - 1));
  const output = piece => { text += piece; if (piece && !structured) onDelta({ content: piece }); };
  for await (const event of runInference(body, credentials, version, signal)) {
    if (event.type === "text") {
      tail += event.text;
      const positions = body.stop.map(stop => tail.indexOf(stop)).filter(index => index >= 0);
      if (positions.length) { output(tail.slice(0, Math.min(...positions))); tail = ""; break; }
      const length = Math.max(0, tail.length - keep); output(tail.slice(0, length)); tail = tail.slice(length);
    } else if (event.type === "reasoning") { thinking += event.text; if (event.text) onDelta({ reasoning_content: event.text }); }
    else if (event.type === "usage") usage = event.usage;
    else if (event.type === "stop") {
      if (event.reason === 3) finish = "length";
      // SWE-2 emits 10 (tool handoff) before the function-call frame; keep reading its arguments.
      else if (event.reason === 10) finish = "tool_calls";
      else if (event.reason !== 0 && event.reason !== 1 && event.reason !== 2) throw failure(502, `Unknown ${label} stop reason: ${typeof event.reason === "number" ? event.reason : "invalid enum"}.`, "upstream_protocol_error");
    } else if (event.type === "tool") {
      let fresh = false;
      if (event.id) {
        if (!callIndexes.has(event.id)) { callIndexes.set(event.id, calls.length); calls.push({ id: event.id, type: "function", function: { name: "", arguments: "" } }); fresh = true; }
        current = callIndexes.get(event.id);
      }
      if (current < 0) throw failure(502, "Tool arguments arrived without a call ID.", "upstream_protocol_error");
      const call = calls[current]; call.function.name += event.name; call.function.arguments += event.arguments;
      if (!structured) onDelta({ tool_calls: [{ index: current, ...(fresh ? { id: call.id, type: "function" } : {}), function: { ...(event.name ? { name: event.name } : {}), arguments: event.arguments } }] });
    }
  }
  output(tail);
  if (finish === "tool_calls" && !calls.length) throw failure(502, `${label} reported a tool handoff without a function call.`, "upstream_protocol_error");
  if (finish === "length" && calls.length) throw failure(502, `${label} truncated function arguments; the incomplete call was not accepted.`, "invalid_tool_arguments");
  if (body.parallel_tool_calls === false && calls.length > 1) throw failure(502, `${label} returned parallel calls despite parallel_tool_calls=false.`, "tool_choice_mismatch");
  for (const call of calls) {
    const check = checks.get(call.function.name);
    let args;
    try { args = JSON.parse(call.function.arguments); } catch { throw failure(502, `${label} returned invalid function arguments.`, "invalid_tool_arguments"); }
    if (!body.tools.some(tool => tool.function.name === call.function.name) || !check?.(args).valid) throw failure(502, `${label} tool name or arguments do not match the supplied schema.`, "invalid_tool_arguments");
  }
  if ((body.tool_choice === "required" || object(body.tool_choice)) && !calls.length) throw failure(502, `${label} did not honor the required function call.`, "tool_choice_mismatch");
  const formatted = calls.find(call => call.function.name === structuredName);
  if (formatted) {
    if (calls.length !== 1) throw failure(502, "Structured response mixed with function calls.", "invalid_structured_output");
    const result = JSON.parse(formatted.function.arguments).result;
    if (!structured(result).valid) throw failure(502, `${label} output did not match the response schema.`, "invalid_structured_output");
    text = JSON.stringify(result); calls.length = 0; finish = "stop"; onDelta({ content: text });
  } else if (structured && calls.length) {
    if (text) onDelta({ content: text });
    onDelta({ tool_calls: calls.map((call, index) => ({ index, ...call })) });
  }
  if (!text && !calls.length && finish !== "length") throw failure(502, `${label} returned no text or function calls.`, "upstream_empty");
  return { message: { role: "assistant", content: text || null, ...(thinking ? { reasoning_content: thinking } : {}), ...(calls.length ? { tool_calls: calls } : {}) }, finish_reason: calls.length ? "tool_calls" : finish, usage };
}

export function createDevinServer({ apiKey, binary = "devin", prefixArgs = [], env = process.env, timeoutMs = 180000, maxConcurrent = 2, source = "devin" } = {}) {
  if (!["devin", "codebuddy", "workbuddy"].includes(source)) throw new Error("Unknown local API source");
  const buddy = source !== "devin" ? createCodeBuddyClient(env, source) : null, label = source === "workbuddy" ? "WorkBuddy" : buddy ? "CodeBuddy" : "Devin";
  if (typeof apiKey !== "string" || !apiKey.trim()) throw new Error("apiKey is required");
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || !Number.isSafeInteger(maxConcurrent) || maxConcurrent <= 0) throw new Error("Invalid timeout/concurrency");
  const active = new Set(), saved = new Map(); let savedBytes = 0, catalog, catalogTime = 0, version;
  const configRoot = process.platform === "win32" ? env.APPDATA : (env.XDG_CONFIG_HOME || join(env.HOME || homedir(), ".config"));
  async function credentials() {
    if (buddy) return buddy.credentials();
    let text;
    try { text = await readFile(join(configRoot, "devin/credentials.toml"), "utf8"); } catch { throw failure(503, "Run devin auth login first.", "devin_auth_error"); }
    const read = name => {
      const match = new RegExp(`^\\s*${name}\\s*=\\s*(\"(?:[^\"\\\\]|\\\\.)*\"|'[^']*')\\s*$`, "m").exec(text);
      if (!match) return undefined;
      try { return match[1][0] === '"' ? JSON.parse(match[1]) : match[1].slice(1,-1); } catch { return undefined; }
    };
    const key = read("windsurf_api_key"), url = read("api_server_url") || "https://server.codeium.com";
    if (!key) throw failure(503, "A Devin static API-key login is required.", "devin_auth_error");
    try { if (!["https:","http:"].includes(new URL(url).protocol)) throw new Error(); } catch { throw failure(503, "Invalid Devin API server URL."); }
    return { key, url };
  }
  async function models(signal) {
    if (buddy) return buddy.models(signal);
    if (!catalog || Date.now() - catalogTime > 60000) {
      try {
        const result = await runFile(binary, [...prefixArgs,"models","list","--format","json"], { env, windowsHide: true, signal, maxBuffer: LIMIT });
        const parsed = JSON.parse(result.stdout);
        if (!Array.isArray(parsed.families)) throw new Error();
        if (!version) { const v = await runFile(binary, [...prefixArgs,"version"], { env, windowsHide: true, signal }); version = v.stdout.match(/\d+\.\d+\.\d+/)?.[0]; if (!version) throw new Error(); }
        catalog = parsed.families.filter(family => family.slug !== "fusion"); catalogTime = Date.now();
      } catch { throw signal.reason || failure(502, "Cannot read Devin model catalog/version.", "devin_models_error"); }
    }
    return catalog;
  }
  async function resolveModel(requested, effort, signal) {
    if (buddy) return buddy.resolveModel(requested, effort, signal);
    const families = await models(signal);
    let id = requested;
    if (id === "devin") {
      let config = {};
      try { config = JSON.parse(await readFile(join(env.DEVIN_CONFIG_HOME || configRoot, "devin/config.json"), "utf8")); }
      catch (error) { if (error.code !== "ENOENT") throw failure(503, "Cannot read Devin model configuration."); }
      id = env.DEVIN_MODEL || config.agent?.model || "adaptive";
    }
    const variants = families.flatMap(family => family.variants || []);
    const family = families.find(family => family.slug === id || family.aliases?.includes(id));
    if (!variants.some(variant => variant.model_uid === id) && family) id = family.variants[0]?.model_uid;
    if (effort) {
      const candidate = id.replace(/-(none|minimal|low|medium|high|xhigh|max)(?=-priority$|$)/, `-${effort}`);
      if (candidate === id && !new RegExp(`-${effort}(?:-priority)?$`).test(id)) throw failure(400, "This model does not expose the requested reasoning_effort; choose a model variant.");
      id = candidate;
    }
    if (!variants.some(variant => variant.model_uid === id)) throw failure(400, "Unknown Devin model or reasoning variant.", "model_not_found");
    return id;
  }
  const json = (res, status, body) => { res.writeHead(status, { "content-type": "application/json; charset=utf-8" }); res.end(JSON.stringify(body)); };
  const server = createServer(async (req, res) => {
    const controller = new AbortController(); let timer, heartbeat, responses = false, sequence = 0;
    const disconnect = () => controller.abort(failure(499, "Client disconnected.", "cancelled")); res.once("close", disconnect);
    try {
      const expected = Buffer.from(`Bearer ${apiKey}`), actual = Buffer.from(req.headers.authorization || "");
      if (expected.length !== actual.length || !timingSafeEqual(expected, actual)) throw failure(401, "Invalid API key.", "invalid_api_key");
      const path = new URL(req.url, "http://localhost").pathname; responses = path === "/v1/responses";
      if (!(req.method === "GET" && path === "/v1/models") && !(req.method === "POST" && ["/v1/chat/completions","/v1/responses"].includes(path))) throw failure(404, "Unknown API route.", "not_found");
      if (active.size >= maxConcurrent) throw failure(429, "Too many active requests; retry later.", "rate_limit_exceeded");
      active.add(controller); timer = setTimeout(() => controller.abort(failure(504, `${label} request timed out.`, "timeout")), timeoutMs);
      if (req.method === "GET") {
        const families = await models(controller.signal);
        const defaultModel = buddy ? (await buddy.credentials()).defaultModel || families[0]?.slug : undefined;
        const variants = new Map([[source, { ...families.find(family => family.slug === defaultModel)?.variants[0], label: `${label} (CLI default)` }], ...families.flatMap(family => (family.variants || []).map(variant => [variant.model_uid, variant]))]);
        return json(res, 200, { object: "list", data: [...variants].map(([id, variant]) => ({
          id, object: "model", created: 0, owned_by: source, name: variant.label || id,
          context_length: variant.max_context_tokens,
          top_provider: { max_completion_tokens: Math.min(variant.max_output_tokens || 32000, buddy ? 1048576 : 128000) },
          // Thinking levels are separate model IDs; do not infer extra controls from another provider's catalog.
          reasoning_options: buddy && variant.model?.reasoning?.supportedEfforts ? [{ type: "effort", values: [...(variant.model.reasoning.canDisableThinking ? ["none"] : []), ...variant.model.reasoning.supportedEfforts] }] : [],
          ...(buddy ? { reasoning: variant.model?.supportsReasoning, supported_parameters: variant.model?.supportsToolCall === false ? [] : ["tools"], architecture: { input_modalities: variant.model?.supportsImages ? ["text", "image"] : ["text"] } } : {}),
        })) });
      }
      if (!/^application\/json(?:;|$)/i.test(req.headers["content-type"] || "")) throw failure(415, "Content-Type must be application/json.");
      const buffers = []; let size = 0;
      const abortUpload = () => req.destroy(); controller.signal.addEventListener("abort", abortUpload, { once: true });
      try { for await (const buffer of req.iterator({ destroyOnReturn: false })) { size += buffer.length; if (size > LIMIT) throw failure(413, "Request body exceeds 32 MiB."); buffers.push(buffer); } }
      finally { controller.signal.removeEventListener("abort", abortUpload); if (!req.readableEnded) req.resume(); }
      let original;
      try { original = JSON.parse(Buffer.concat(buffers).toString()); } catch { throw failure(400, "Invalid JSON body."); }
      requireThat(object(original), "Expected a JSON object.");
      for (const [id, entry] of saved) if (entry.expires < Date.now()) { saved.delete(id); savedBytes -= entry.bytes; }
      let body = { ...original }, history;
      if (responses) {
        const supported = new Set(["model","input","instructions","previous_response_id","tools","tool_choice","parallel_tool_calls","stream","temperature","top_p","max_output_tokens","reasoning","text","store","metadata","user","truncation","background"]);
        for (const key of Object.keys(original)) requireThat(supported.has(key), `Unsupported Responses parameter: ${key}`);
        requireThat(original.tools === undefined || Array.isArray(original.tools), "tools must be an array.");
        requireThat([undefined,true,false].includes(original.store), "store must be boolean.");
        requireThat(original.reasoning === undefined || (object(original.reasoning) && Object.keys(original.reasoning).every(key => key === "effort")), "reasoning currently supports effort only.");
        requireThat(original.text === undefined || (object(original.text) && Object.keys(original.text).every(key => key === "format")), "text currently supports format only.");
        requireThat([undefined,false].includes(original.background) && [undefined,"disabled"].includes(original.truncation), "Background execution and automatic truncation are unavailable.");
        const previous = original.previous_response_id ? saved.get(original.previous_response_id) : undefined;
        if (original.previous_response_id && !previous) throw failure(404, "previous_response_id has expired or is unknown.", "response_not_found");
        history = [...(previous?.history || []), ...responseInput(original.input)];
        body = { model: original.model || previous?.model || source, messages: history, stream: original.stream, temperature: original.temperature, top_p: original.top_p,
          max_completion_tokens: original.max_output_tokens, reasoning_effort: original.reasoning?.effort, parallel_tool_calls: original.parallel_tool_calls,
          tools: original.tools?.map(tool => { requireThat(tool?.type === "function", "Only function tools are supported."); return { type: "function", function: { name: tool.name, description: tool.description, parameters: tool.parameters, strict: tool.strict } }; }),
          tool_choice: object(original.tool_choice) ? { type: original.tool_choice.type, function: { name: original.tool_choice.name } } : original.tool_choice };
        if (original.instructions !== undefined) { requireThat(typeof original.instructions === "string", "instructions must be text."); body.messages = [{ role: "system", content: original.instructions }, ...history]; }
        if (original.text?.format) { const f = original.text.format; body.response_format = f.type === "json_schema" ? { type: f.type, json_schema: { name: f.name, schema: f.schema, strict: f.strict } } : f; }
      }
      const { checks, structured } = validate(body, source);
      body.model = await resolveModel(body.model, body.reasoning_effort, controller.signal);
      const auth = await credentials(), created = Math.floor(Date.now() / 1000), id = `${responses ? "resp" : "chatcmpl"}_${randomUUID()}`;
      const event = (payload, type) => {
        if (res.destroyed) throw failure(499, "Client disconnected.", "cancelled");
        if (res.writableLength > LIMIT) throw failure(502, "Client is consuming the stream too slowly.", "slow_consumer");
        if (!res.headersSent) {
          res.writeHead(200, { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-cache", "x-accel-buffering": "no" });
          heartbeat = setInterval(() => { if (!res.destroyed && res.writableLength < LIMIT) res.write(": keep-alive\n\n"); }, 15000);
        }
        res.write(`${type ? `event: ${type}\n` : ""}data: ${JSON.stringify(type ? { ...payload, type, sequence_number: sequence++ } : payload)}\n\n`);
      };
      const chatChunk = (index, delta, finish_reason = null) => event({ id, object: "chat.completion.chunk", created, model: body.model, choices: [{ index, delta, finish_reason }] });
      const output = [], responseCalls = new Map(); let messageIndex;
      const response = { id, object: "response", created_at: created, status: "in_progress", error: null, incomplete_details: null, model: body.model, output,
        parallel_tool_calls: body.parallel_tool_calls ?? true, tool_choice: original.tool_choice ?? "auto", tools: original.tools || [], temperature: body.temperature ?? 1, top_p: body.top_p ?? 0.95,
        max_output_tokens: original.max_output_tokens ?? null, previous_response_id: original.previous_response_id ?? null, instructions: original.instructions ?? null, store: original.store !== false, metadata: original.metadata || {}, usage: null };
      if (responses && body.stream) { event({ response: { ...response, output: [] } }, "response.created"); event({ response: { ...response, output: [] } }, "response.in_progress"); }
      const responseDelta = delta => {
        if (delta.content) {
          if (messageIndex === undefined) {
            messageIndex = output.length; const item = { id: `msg_${randomUUID()}`, type: "message", role: "assistant", status: "in_progress", content: [] }; output.push(item);
            if (body.stream) event({ output_index: messageIndex, item: { ...item, content: [] } }, "response.output_item.added");
            item.content.push({ type: "output_text", text: "", annotations: [] });
            if (body.stream) event({ item_id: item.id, output_index: messageIndex, content_index: 0, part: { ...item.content[0] } }, "response.content_part.added");
          }
          const item = output[messageIndex]; item.content[0].text += delta.content;
          if (body.stream) event({ item_id: item.id, output_index: messageIndex, content_index: 0, delta: delta.content }, "response.output_text.delta");
        }
        for (const call of delta.tool_calls || []) {
          if (!responseCalls.has(call.index)) {
            const index = output.length; responseCalls.set(call.index, index);
            const item = { id: `fc_${randomUUID()}`, type: "function_call", call_id: call.id, name: call.function.name || "", arguments: "", status: "in_progress" }; output.push(item);
            if (body.stream) event({ output_index: index, item: { ...item } }, "response.output_item.added");
          }
          const index = responseCalls.get(call.index), item = output[index];
          if (call.function.name) item.name = call.function.name;
          item.arguments += call.function.arguments || "";
          if (body.stream && call.function.arguments) event({ item_id: item.id, output_index: index, delta: call.function.arguments }, "response.function_call_arguments.delta");
        }
      };
      const choices = []; let totalUsage = null, missingUsage = false;
      for (let index = 0; index < (body.n || 1); index++) {
        let started = false;
        const choice = await complete(body, auth, version, controller.signal, checks, structured, delta => {
          if (responses) responseDelta(delta);
          else if (body.stream) { if (!started) { chatChunk(index, { role: "assistant", content: "" }); started = true; } chatChunk(index, delta); }
        }, buddy?.inference || inference, label);
        choices.push({ index, message: choice.message, finish_reason: choice.finish_reason });
        if (choice.usage) { totalUsage ??= { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, prompt_tokens_details: { cached_tokens: 0 } }; for (const key of ["prompt_tokens","completion_tokens","total_tokens"]) totalUsage[key] += choice.usage[key]; totalUsage.prompt_tokens_details.cached_tokens += choice.usage.prompt_tokens_details.cached_tokens; }
        else missingUsage = true;
        if (!responses && body.stream) chatChunk(index, {}, choice.finish_reason);
      }
      if (missingUsage) totalUsage = null;
      if (responses) {
        response.status = choices[0].finish_reason === "length" ? "incomplete" : "completed";
        if (response.status === "incomplete") response.incomplete_details = { reason: "max_output_tokens" };
        if (totalUsage) response.usage = { input_tokens: totalUsage.prompt_tokens, input_tokens_details: totalUsage.prompt_tokens_details, output_tokens: totalUsage.completion_tokens, total_tokens: totalUsage.total_tokens };
        for (let index = 0; index < output.length; index++) {
          const item = output[index]; item.status = response.status;
          if (body.stream) {
            if (item.type === "message") { event({ item_id: item.id, output_index: index, content_index: 0, text: item.content[0].text }, "response.output_text.done"); event({ item_id: item.id, output_index: index, content_index: 0, part: item.content[0] }, "response.content_part.done"); }
            else event({ item_id: item.id, output_index: index, arguments: item.arguments }, "response.function_call_arguments.done");
            event({ output_index: index, item }, "response.output_item.done");
          }
        }
        if (response.store) {
          const entry = { history: [...history, choices[0].message], model: body.model, expires: Date.now() + 3600000 };
          entry.bytes = Buffer.byteLength(JSON.stringify(entry));
          // ponytail: previous_response_id is process-local, bounded to 64 MiB/100
          // responses/one hour. Use an explicit durable store for cross-process history.
          while (saved.size && (saved.size >= 100 || savedBytes + entry.bytes > 64 * 1024 * 1024)) { const first = saved.keys().next().value; savedBytes -= saved.get(first).bytes; saved.delete(first); }
          if (entry.bytes <= 64 * 1024 * 1024) { saved.set(id, entry); savedBytes += entry.bytes; }
        }
        if (body.stream) { event({ response }, `response.${response.status}`); res.end(); } else json(res, 200, response);
      } else if (body.stream) {
        if (body.stream_options?.include_usage) event({ id, object: "chat.completion.chunk", created, model: body.model, choices: [], usage: totalUsage });
        res.end("data: [DONE]\n\n");
      } else json(res, 200, { id, object: "chat.completion", created, model: body.model, choices, usage: totalUsage });
    } catch (error) {
      const problem = controller.signal.reason || error, status = problem.status || 502;
      const payload = { error: { message: problem.status ? problem.message : `${label} request failed.`, type: status < 500 ? "invalid_request_error" : "server_error", param: null, code: problem.code || "upstream_error" } };
      if (!res.destroyed) {
        if (res.headersSent) res.end(responses ? `event: error\ndata: ${JSON.stringify({ ...payload.error, type: "error", sequence_number: sequence++ })}\n\n` : `data: ${JSON.stringify(payload)}\n\n`);
        else json(res, status, payload);
      }
    } finally { clearTimeout(timer); clearInterval(heartbeat); res.off("close", disconnect); active.delete(controller); }
  });
  server.shutdown = () => { for (const controller of active) controller.abort(failure(503, "Server shutting down.", "shutdown")); saved.clear(); server.close(); server.closeAllConnections(); };
  return server;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const managed = process.argv.includes("--managed");
  const source = process.argv.includes("--workbuddy") ? "workbuddy" : process.argv.includes("--codebuddy") ? "codebuddy" : "devin", label = source === "workbuddy" ? "WorkBuddy" : source === "codebuddy" ? "CodeBuddy" : "Devin";
  const setting = name => process.env[`NOVA_${source.toUpperCase()}_${name}`];
  if (process.env.HTTPS_PROXY || process.env.HTTP_PROXY || process.env.https_proxy || process.env.http_proxy) {
    if (!http.setGlobalProxyFromEnv) throw new Error("Proxy support requires a recent Node.js 24 release (verified with 24.19).");
    http.setGlobalProxyFromEnv();
  }
  const connectionPath = join(process.env.NOVA_DATA_DIR || join(process.env.HOME || process.env.USERPROFILE || homedir(), ".nova"), `alkaid/${source}-api.json`);
  let connection = {};
  try { if (!managed) connection = JSON.parse(await readFile(connectionPath, "utf8")); }
  catch (error) { if (error.code !== "ENOENT") throw new Error(`Cannot read ${label} local connection configuration.`); }
  if (!object(connection) || (connection.apiKey !== undefined && (typeof connection.apiKey !== "string" || !connection.apiKey.trim()))) throw new Error(`Invalid ${label} local connection configuration.`);
  const apiKey = managed ? randomBytes(24).toString("hex") : setting("API_KEY") || connection.apiKey || randomBytes(24).toString("hex");
  const host = managed ? "127.0.0.1" : setting("HOST") || "127.0.0.1", port = managed ? 0 : Number(setting("PORT") || (source === "workbuddy" ? 8789 : source === "codebuddy" ? 8788 : 8787));
  if (!Number.isInteger(port) || port < (managed ? 0 : 1) || port > 65535) throw new Error(`Invalid ${label} port`);
  const server = createDevinServer({ apiKey, binary: process.env.NOVA_DEVIN_BIN || "devin", timeoutMs: Number(setting("TIMEOUT_MS") || 180000), source });
  // Nova holds stdin open. EOF also cleans up after a parent crash; no detached service remains.
  if (managed) { process.stdin.once("end", () => server.shutdown()); process.stdin.resume(); }
  server.on("error", error => { console.error(`${label} API could not listen: ${error.code || "unknown error"}`); process.exitCode = 1; });
  server.listen(port, host, async () => {
    const hostname = host === "0.0.0.0" ? "127.0.0.1" : host === "::" ? "[::1]" : host.includes(":") ? `[${host}]` : host;
    const baseURL = `http://${hostname}:${server.address().port}/v1`;
    if (managed) { console.log(JSON.stringify({ baseURL, apiKey })); return; }
    try {
      await mkdir(dirname(connectionPath), { recursive: true });
      await writeFile(`${connectionPath}.tmp`, JSON.stringify({ baseURL, apiKey }), { mode: 0o600 });
      await rename(`${connectionPath}.tmp`, connectionPath);
      console.log(`${label} API: ${baseURL}\nLyra connection: ${connectionPath}`);
    } catch {
      console.error(`Cannot save ${label} local connection configuration.`); server.shutdown(); process.exitCode = 1;
    }
  });
  process.once("SIGINT", () => server.shutdown()); process.once("SIGTERM", () => server.shutdown());
}
