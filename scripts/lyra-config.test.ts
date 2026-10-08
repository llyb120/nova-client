import assert from "node:assert";
import { configFromDraft, draftFromConfig, validate } from "../src/lyraConfig.ts";

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
assert.equal(validate(importedDraft.providers, presets), null);
const savedImport = configFromDraft(imported, importedDraft);
assert.equal(savedImport.provider["local-claude-code"].api, "anthropic-messages");
assert.equal(savedImport.provider["local-claude-code"].options.headers.Authorization, "Bearer test-token");
assert.equal(savedImport.model, imported.model);
assert.deepEqual(savedImport.provider["local-claude-code"].models.custom.variants, imported.provider["local-claude-code"].models.custom.variants);
console.log("ok");
