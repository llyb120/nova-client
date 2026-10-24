import assert from "node:assert";
import { configFromDraft, draftFromConfig, parseMcpServers, providerDraft, validate } from "../src/lyraConfig.ts";

const raw = {
  $schema: "x",
  model: "p/m/variant/high",
  provider: {
    p: {
      npm: "@ai-sdk/openai-compatible",
      options: { baseUrl: "https://e/v1", apiKey: "{env:K}", tool_stream: true, temperature: 0.5, headers: { "X-A": "1" } },
      models: {
        m: { name: "M", variants: { high: { reasoningEffort: "high" } }, limit: { context: 1000 }, modalities: { input: ["text", "image"], output: ["text"] }, options: { headers: { "X-A": null }, proxy: "" } },
        plain: {},
      },
    },
  },
};

// 未改动时往返等价（baseUrl 归一为 baseURL），未接管的键原样保留。
const draft = draftFromConfig(raw);
assert.equal(validate(draft.providers, []), null);
const out = configFromDraft(raw, draft);
assert.equal(out.$schema, "x");
assert.equal(out.model, "p/m/variant/high");
const p = out.provider.p;
assert.equal(p.npm, "@ai-sdk/openai-compatible");
assert.deepEqual(p.options, { baseURL: "https://e/v1", apiKey: "{env:K}", tool_stream: true, temperature: 0.5, headers: { "X-A": "1" } });
assert.deepEqual(p.models.m, raw.provider.p.models.m);
assert.deepEqual(p.models.plain, {});

// 编辑：自定义头/字段按类型写回，思考开关与默认不同时显式写出。
draft.providers[0].headers.push({ key: "X-B", value: "123" });
draft.providers[0].fields.push({ key: "thinking_budget", value: "2048" }, { key: "label", value: "abc" });
draft.providers[0].models[1].reasoning = true;
draft.providers[0].models[1].context = "64000";
const edited = configFromDraft(raw, draft).provider.p;
assert.deepEqual(edited.options.headers, { "X-A": "1", "X-B": "123" });
assert.equal(edited.options.thinking_budget, 2048);
assert.equal(edited.options.label, "abc");
assert.deepEqual(edited.models.plain, { reasoning: true, limit: { context: 64000 } });

draft.providers[0].models.push({ ...draft.providers[0].models[1], id: "plain" });
assert.match(validate(draft.providers, []) ?? "", /重复/);

const presets = [{ id: "commandcode", name: "Command Code", baseURL: "https://api.commandcode.ai/provider/v1" }, { id: "openai-compatible", name: "OpenAI 兼容", baseURL: "" }];
// 自适应预设：只写 preset + Key，空 Base URL / 协议 / 模型不写出，校验放行。
const presetRaw = { provider: { cc: { preset: "commandcode", options: { apiKey: "k" } } } };
const presetDraft = draftFromConfig(presetRaw);
assert.equal(presetDraft.providers[0].preset, "commandcode");
assert.equal(validate(presetDraft.providers, presets), null);
assert.deepEqual(configFromDraft(presetRaw, presetDraft).provider.cc, presetRaw.provider.cc);
presetDraft.providers[0].preset = "openai-compatible";
assert.match(validate(presetDraft.providers, presets) ?? "", /Base URL/);

// 本地 API 配置导入为普通 provider：保存时保留协议、认证头和思考档位。
const imported = { model: "local-claude-code/custom/variant/high", provider: { "local-claude-code": {
  name: "本地 Claude Code", api: "anthropic-messages",
  options: { baseURL: "https://gateway.example", apiKey: "", headers: { Authorization: "Bearer test-token" } },
  models: { custom: { reasoning: true, options: { reasoningEffort: "high" }, variants: { high: { reasoningEffort: "high" } } } },
} } };
const importedDraft = draftFromConfig(imported);
presets.push({ id: "local-claude-code", name: "本地 Claude Code", baseURL: "" });
assert.equal(importedDraft.providers[0].preset, "local-claude-code");
assert.equal(validate(importedDraft.providers, presets), null);
const savedImport = configFromDraft(imported, importedDraft);
assert.equal(savedImport.provider["local-claude-code"].api, "anthropic-messages");
assert.equal(savedImport.provider["local-claude-code"].options.headers.Authorization, "Bearer test-token");
assert.equal(savedImport.model, imported.model);
assert.deepEqual(savedImport.provider["local-claude-code"].models.custom.variants, imported.provider["local-claude-code"].models.custom.variants);

// 本地 provider 无需手填地址/协议/Key；凭证跟随 CLI，不能把旧导入的 Key 固化回去。
for (const id of ["local-codex", "local-claude-code", "devin", "codebuddy", "workbuddy"]) {
  const provider = providerDraft(id, { preset: id, options: { apiKey: "old-key" } });
  assert.equal(validate([provider], [{ id, name: id, baseURL: "", local: true }]), null);
  const config = configFromDraft({}, { model: "", providers: [provider], mcpServers: "{}" });
  assert.deepEqual(config.provider[id], { preset: id, options: {} });
}
assert.equal(providerDraft("local-codex-2", { name: "本地 Codex" }).preset, "local-codex");
assert.equal(providerDraft("local-codex", { name: "我自己的网关" }).preset, "");
// MCP 是顶层独立配置；编辑/删除 provider 不得删除 MCP 或未知字段。
const mcpServers = {
  local: { command: "/usr/local/bin/mcp", args: ["--project", "a b"], env: { PROJECT: "demo", API_TOKEN: "{env:MY_MCP_TOKEN}" }, cwd: "/project/a b", enabled: true, readOnlyTools: ["lookup"], startup_timeout_ms: 10000, request_timeout_ms: 30000 },
  remote: { url: "https://mcp.example.test/mcp", headers: { Authorization: "{env:MCP_AUTHORIZATION}" }, enabled: false, readOnlyTools: [] },
};
const mcpRaw = { ...raw, extra: { keep: [1, 2] }, mcpServers };
const mcpBefore = JSON.stringify(mcpRaw);
const mcpDraft = draftFromConfig(mcpRaw);
assert.deepEqual(parseMcpServers(mcpDraft.mcpServers), mcpServers);
assert.deepEqual(configFromDraft(mcpRaw, mcpDraft).mcpServers, mcpServers);
mcpDraft.providers = [];
const mcpSaved = configFromDraft(mcpRaw, mcpDraft);
assert.deepEqual(mcpSaved.provider, {});
assert.deepEqual(mcpSaved.mcpServers, mcpServers);
assert.deepEqual(mcpSaved.extra, mcpRaw.extra);
assert.equal(mcpSaved.$schema, raw.$schema);
assert.equal(mcpSaved.model, raw.model);
mcpDraft.mcpServers = JSON.stringify({ ...mcpServers, local: { ...mcpServers.local, enabled: false } });
assert.equal(configFromDraft(mcpRaw, mcpDraft).mcpServers.local.enabled, false);
mcpDraft.mcpServers = "{}";
assert.deepEqual(configFromDraft(mcpRaw, mcpDraft).mcpServers, {});
assert.equal(JSON.stringify(mcpRaw), mcpBefore);
assert.equal(Object.hasOwn(configFromDraft(raw, draftFromConfig(raw)), "mcpServers"), false);
assert.deepEqual(configFromDraft({}, draftFromConfig({ mcpServers })).mcpServers, mcpServers);
// 授权按精确工具名保留；省略 readOnlyTools 不自动授权，也不展开环境变量引用。
const minimalMcp = { local: { command: "mcp", enabled: false }, remote: { url: "http://localhost:8080/mcp", enabled: true } };
assert.deepEqual(configFromDraft({}, draftFromConfig({ mcpServers: minimalMcp })).mcpServers, minimalMcp);
assert.deepEqual(mcpSaved.mcpServers.local.readOnlyTools, ["lookup"]);
assert.deepEqual(mcpSaved.mcpServers.remote.readOnlyTools, []);
assert.equal(mcpSaved.mcpServers.local.cwd, "/project/a b");
assert.equal(mcpSaved.mcpServers.local.env.API_TOKEN, "{env:MY_MCP_TOKEN}");
assert.equal(mcpSaved.mcpServers.remote.headers.Authorization, "{env:MCP_AUTHORIZATION}");
assert.equal(Object.hasOwn(mcpSaved.mcpServers.local, "readOnly"), false);

// 不自动修复/清空非法原始配置；保存前拒绝含糊授权、错类型与混合传输。
for (const value of [null, [], "bad", 42]) {
  const invalidDraft = draftFromConfig({ mcpServers: value });
  assert.equal(invalidDraft.mcpServers, JSON.stringify(value, null, 2));
  assert.throws(() => configFromDraft({}, invalidDraft), /MCP.*JSON 对象/);
}
for (const text of ["", " ", "{", '{"a":', '{/* comment */}']) {
  assert.throws(() => parseMcpServers(text), /有效 JSON/);
}
for (const server of [
  null, [], "command",
  { command: "mcp" },
  { ...mcpServers.local, enabled: "false" },
  { ...mcpServers.local, enabled: null },
  { ...mcpServers.local, readOnly: true },
  { ...mcpServers.local, readOnly: false },
  { ...mcpServers.local, readOnly: "true" },
  { ...mcpServers.local, readOnlyTools: true },
  { ...mcpServers.local, readOnlyTools: "lookup" },
  { ...mcpServers.local, readOnlyTools: {} },
  { ...mcpServers.local, readOnlyTools: null },
  { ...mcpServers.local, readOnlyTools: [1] },
  { ...mcpServers.local, readOnlyTools: [""] },
  { ...mcpServers.local, readOnlyTools: [" "] },
  { ...mcpServers.local, cwd: "" },
  { ...mcpServers.local, cwd: 123 },
  { ...mcpServers.local, cwd: null },
  { enabled: false },
  { ...mcpServers.local, command: " " },
  { ...mcpServers.local, command: 123 },
  { ...mcpServers.local, url: "https://example.test/mcp" },
  { ...mcpServers.local, headers: {} },
  { ...mcpServers.local, args: "--foo" },
  { ...mcpServers.local, args: [1] },
  { ...mcpServers.local, env: [] },
  { ...mcpServers.local, env: { SECRET: null } },
  { ...mcpServers.remote, url: "file:///tmp/mcp" },
  { ...mcpServers.remote, url: "/mcp" },
  { ...mcpServers.remote, url: "" },
  { ...mcpServers.remote, headers: { Authorization: 123 } },
  { ...mcpServers.remote, args: [] },
  { ...mcpServers.remote, env: {} },
  { ...mcpServers.remote, cwd: "/project" },
]) {
  const invalidDraft = { ...mcpDraft, mcpServers: JSON.stringify({ bad: server }) };
  assert.throws(() => configFromDraft(mcpRaw, invalidDraft), /MCP/);
  assert.equal(JSON.stringify(mcpRaw), mcpBefore);
}
assert.throws(() => parseMcpServers(JSON.stringify({ " ": mcpServers.local })), /名称不能为空/);
for (const key of ["startup_timeout_ms", "request_timeout_ms"]) {
  for (const value of [0, -1, 1.5, 600001, "30000", true, null]) {
    assert.throws(() => parseMcpServers(JSON.stringify({ local: { ...mcpServers.local, [key]: value } })), /毫秒|null/);
  }
  for (const value of [1, 600000]) {
    assert.equal(parseMcpServers(JSON.stringify({ local: { ...mcpServers.local, [key]: value } })).local[key], value);
  }
}

// 对齐后端 save-time 边界：禁用项也校验，不忽略未知字段，不读取环境变量。
for (const server of [
  { ...mcpServers.local, custom: { keep: 1 } },
  { ...minimalMcp.local, "": true },
  { ...minimalMcp.local, env_from: {} },
  { ...minimalMcp.local, transport: { type: "stdio" } },
  { ...minimalMcp.local, command: "a".repeat(4097) },
  { ...minimalMcp.local, command: "界".repeat(1366) },
  { ...minimalMcp.local, command: "mcp\0" },
  { ...minimalMcp.local, cwd: "/tmp\0" },
  { ...minimalMcp.local, args: Array(129).fill("") },
  { ...minimalMcp.local, args: ["a".repeat(65537)] },
  { ...minimalMcp.local, args: ["\0"] },
  { ...minimalMcp.local, readOnlyTools: Array.from({ length: 65 }, (_, i) => `t${i}`) },
  { ...minimalMcp.local, readOnlyTools: ["界".repeat(43)] },
  { ...minimalMcp.local, readOnlyTools: ["lookup\n"] },
  { ...minimalMcp.local, env: { "": "x" } },
  { ...minimalMcp.local, env: { "A=B": "x" } },
  { ...minimalMcp.local, env: { ["a".repeat(257)]: "x" } },
  { ...minimalMcp.local, env: Object.fromEntries(Array.from({ length: 65 }, (_, i) => [`E${i}`, ""])) },
  ...["\0", "x".repeat(65537), "{env:}", "{env:BAD-NAME}", "{env:MISSING", `{env:${"A".repeat(257)}}`].map((value) => ({ ...minimalMcp.local, env: { KEY: value } })),
  ...["https://u:p@example.test/mcp", "https://example.test/mcp#", "https://example.test/mcp#fragment"].map((url) => ({ ...minimalMcp.remote, url })),
  { ...minimalMcp.remote, headers: { "Bad Header": "x" } },
  { ...minimalMcp.remote, headers: { Authorization: "a", authorization: "b" } },
  { ...minimalMcp.remote, headers: { "X-A": "a\r\nb" } },
  { ...minimalMcp.remote, headers: { "X-A": "中文" } },
  { ...minimalMcp.remote, headers: { "X-A": "a\x7fb" } },
  { ...minimalMcp.remote, url: "http://example.test/mcp" },
  { ...minimalMcp.remote, url: "http://10.0.0.1/mcp" },
  { enabled: true, command: null, url: "https://example.test/mcp" },
  { ...minimalMcp.local, cwd: "relative/dir" },
  { ...minimalMcp.local, cwd: "./x" },
  { ...minimalMcp.remote, headers: { Authorization: "Bearer {env:BAD NAME}" } },
  { ...minimalMcp.remote, headers: Object.fromEntries(Array.from({ length: 65 }, (_, i) => [`X-${i}`, ""])) },
  ...["Host", "Content-Length", "Transfer-Encoding", "Connection", "Content-Type", "Accept", "Mcp-Session-Id", "Mcp-Protocol-Version"].map((key) => ({ ...minimalMcp.remote, headers: { [key]: "x" } })),
]) {
  assert.throws(() => parseMcpServers(JSON.stringify({ bad: server })), /MCP/);
}
for (const name of ["a".repeat(129), "中文", "bad/name", "bad\n"]) {
  assert.throws(() => parseMcpServers(JSON.stringify({ [name]: minimalMcp.local })), /MCP/);
}
const boundary = {
  ["a".repeat(128)]: {
    enabled: false, command: "a".repeat(4096), args: ["界".repeat(21845) + "x", ...Array(127).fill("")],
    env: Object.fromEntries(Array.from({ length: 64 }, (_, i) => [`E${i}`, "{env:UNSET_TEST_MCP_VAR}" ])),
    readOnlyTools: [...Array.from({ length: 64 }, (_, i) => `t${i}`), "t0"],
  },
  http: { ...minimalMcp.remote, headers: { Authorization: "Bearer {env:UNSET_TEST_MCP_VAR}", "X-Trace": "{env:A}-{env:B}" } },
};
assert.deepEqual(parseMcpServers(JSON.stringify(boundary)), boundary);
for (const url of ["http://127.0.0.1:9/mcp", "http://[::1]:9/mcp", "http://LOCALHOST/mcp", "https://example.test/mcp"]) {
  assert.doesNotThrow(() => parseMcpServers(JSON.stringify({ ok: { ...minimalMcp.remote, url } })), url);
}
for (const cwd of ["/abs", "C:\\abs", "D:/abs", "\\\\server\\share"]) {
  assert.doesNotThrow(() => parseMcpServers(JSON.stringify({ ok: { ...minimalMcp.local, cwd } })), cwd);
}
const services = Object.fromEntries(Array.from({ length: 64 }, (_, i) => [`s${i}`, minimalMcp.local]));
assert.deepEqual(parseMcpServers(JSON.stringify(services)), services);
assert.throws(() => parseMcpServers(JSON.stringify({ ...services, extra: minimalMcp.local })), /64/);

// tokenizer/续传沿用 options KV；模型覆盖 provider，不增设表单或搬到 MCP。
const optionsRaw = { ...raw, provider: { p: { ...raw.provider.p, options: {
  ...raw.provider.p.options, tokenizer: "o200k_base", responsesContinuation: "auto", supportsPreviousResponseId: true, store: true,
} } } };
const optionsDraft = draftFromConfig(optionsRaw);
assert.deepEqual(configFromDraft(optionsRaw, optionsDraft).provider.p.options, { ...p.options,
  tokenizer: "o200k_base", responsesContinuation: "auto", supportsPreviousResponseId: true, store: true,
});
optionsDraft.providers[0].models[0].fields.push({ key: "responsesContinuation", value: '"off"' }, { key: "tokenizer", value: '"cl100k_base"' });
optionsDraft.providers[0].fields.find((row) => row.key === "supportsPreviousResponseId")!.value = "false";
const optionsSaved = configFromDraft(optionsRaw, optionsDraft);
assert.equal(optionsSaved.provider.p.options.supportsPreviousResponseId, false);
assert.equal(optionsSaved.provider.p.models.m.options.responsesContinuation, "off");
assert.equal(optionsSaved.provider.p.models.m.options.tokenizer, "cl100k_base");
assert.equal(optionsSaved.provider.p.options.responsesContinuation, "auto");
assert.equal(Object.hasOwn(optionsSaved, "mcpServers"), false);
console.log("ok");
