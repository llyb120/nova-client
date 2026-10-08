// 图形化编辑 ~/.nova/alkaid/config.jsonc。只接管界面上的字段，其余键（npm、variants、
// modalities.output 等）经 raw 原样写回；options 中除 baseURL/apiKey/proxy/headers 外
// 的键都作为「自定义字段」展示（采样参数、厂商字段等），由后端透传或消费。

export type Kv = { key: string; value: string };
export type ModelDraft = {
  id: string;
  name: string;
  reasoning: boolean;
  image: boolean;
  context: string;
  output: string;
  headers: Kv[];
  fields: Kv[];
  raw: Record<string, any>;
};
export type ProviderDraft = {
  id: string;
  /** 自适应预设（见后端 lyra/presets.rs），为空表示手动配置。 */
  preset: string;
  name: string;
  api: string;
  baseURL: string;
  apiKey: string;
  proxy: string;
  headers: Kv[];
  fields: Kv[];
  models: ModelDraft[];
  raw: Record<string, any>;
};

export const APIS = ["openai-completions", "openai-responses", "anthropic-messages"];
/** 后端 lyra/presets.rs 的自适应预设：只填 Key 即自动拉取模型列表；baseURL 为空表示需自填。 */
export type Preset = { id: string; name: string; baseURL: string; local?: boolean };
export const isLocalPreset = (id: string) => id === "local-codex" || id === "local-claude-code";
const OPTION_KEYS = ["baseURL", "baseUrl", "apiKey", "proxy", "headers"];

export const isObject = (v: unknown): v is Record<string, any> =>
  !!v && typeof v === "object" && !Array.isArray(v);
const showValue = (v: unknown) => (typeof v === "string" ? v : JSON.stringify(v));
/** 字段值按 JSON 解析（数字、布尔、对象），失败则作为字符串。 */
const parseValue = (text: string): unknown => {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
};
const toKv = (obj: unknown): Kv[] =>
  isObject(obj) ? Object.entries(obj).map(([key, value]) => ({ key, value: showValue(value) })) : [];
const fieldsOf = (options: Record<string, any>, skip: string[]): Kv[] =>
  toKv(Object.fromEntries(Object.entries(options).filter(([key]) => !skip.includes(key))));
const fromKv = (rows: Kv[], header: boolean) => {
  const out: Record<string, unknown> = {};
  for (const row of rows) {
    const key = row.key.trim();
    // 请求头保持字符串；字面量 null 表示删除继承的同名头。
    if (key) out[key] = header ? (row.value.trim() === "null" ? null : row.value) : parseValue(row.value);
  }
  return out;
};
export const hasVariants = (raw: Record<string, any>) =>
  isObject(raw.variants) && Object.keys(raw.variants).length > 0;

export function modelDraft(id: string, raw: Record<string, any>): ModelDraft {
  const options = isObject(raw.options) ? raw.options : {};
  return {
    id,
    name: raw.name ?? "",
    reasoning: typeof raw.reasoning === "boolean" ? raw.reasoning : hasVariants(raw),
    image: Array.isArray(raw.modalities?.input) && raw.modalities.input.includes("image"),
    context: raw.limit?.context?.toString() ?? "",
    output: raw.limit?.output?.toString() ?? "",
    headers: toKv(options.headers),
    fields: fieldsOf(options, ["headers"]),
    raw,
  };
}

export function providerDraft(id: string, raw: Record<string, any>): ProviderDraft {
  const options = isObject(raw.options) ? raw.options : {};
  // 旧导入项接入统一预设，保留 ID、已选模型和用户覆盖项。
  const legacy = !raw.preset && ["local-codex", "local-claude-code"].find((prefix) =>
    (id === prefix || new RegExp(`^${prefix}-[0-9]+$`).test(id)) &&
    raw.name === (prefix === "local-codex" ? "本地 Codex" : "本地 Claude Code"));
  return {
    id,
    preset: typeof raw.preset === "string" ? raw.preset : legacy || "",
    name: raw.name ?? "",
    api: raw.api ?? "",
    baseURL: options.baseURL ?? options.baseUrl ?? "",
    apiKey: options.apiKey ?? "",
    proxy: options.proxy ?? "",
    headers: toKv(options.headers),
    fields: fieldsOf(options, OPTION_KEYS),
    models: Object.entries(isObject(raw.models) ? raw.models : {}).map(([mid, m]) =>
      modelDraft(mid, isObject(m) ? m : {}),
    ),
    raw,
  };
}

function setOrDelete(target: Record<string, any>, key: string, value: unknown) {
  if (value === "" || value === undefined || (isObject(value) && !Object.keys(value).length)) delete target[key];
  else target[key] = value;
}

function serializeModel(m: ModelDraft) {
  const out: Record<string, any> = { ...m.raw };
  setOrDelete(out, "name", m.name.trim());
  // 与后端一致：未显式声明时，有 variants 即视为支持思考；仅与默认不同才写出。
  if (m.reasoning === hasVariants(m.raw)) delete out.reasoning;
  else out.reasoning = m.reasoning;
  if (m.image || isObject(m.raw.modalities)) {
    out.modalities = { ...m.raw.modalities, input: m.image ? ["text", "image"] : ["text"] };
  }
  const limit: Record<string, any> = { ...m.raw.limit };
  for (const key of ["context", "output"] as const) {
    const n = Number(m[key]);
    if (m[key].trim() && Number.isFinite(n) && n > 0) limit[key] = Math.floor(n);
    else delete limit[key];
  }
  setOrDelete(out, "limit", limit);
  setOrDelete(out, "options", { ...fromKv(m.fields, false), ...withHeaders(m.headers) });
  return out;
}

const withHeaders = (rows: Kv[]) => {
  const headers = fromKv(rows, true);
  return Object.keys(headers).length ? { headers } : {};
};

export function serializeProvider(p: ProviderDraft) {
  const out: Record<string, any> = { ...p.raw };
  setOrDelete(out, "preset", p.preset);
  setOrDelete(out, "name", p.name.trim());
  setOrDelete(out, "api", p.api);
  const options: Record<string, any> = { ...fromKv(p.fields, false), baseURL: p.baseURL.trim() };
  // 预设的 Base URL 留空即用默认值，不写空串。
  if (p.preset) setOrDelete(options, "baseURL", options.baseURL);
  setOrDelete(options, "apiKey", isLocalPreset(p.preset) ? "" : p.apiKey.trim());
  setOrDelete(options, "proxy", p.proxy.trim());
  out.options = { ...options, ...withHeaders(p.headers) };
  const models = Object.fromEntries(p.models.map((m) => [m.id.trim(), serializeModel(m)]));
  // 预设的模型自动拉取，手写模型只用于补充/覆盖，没有就不写。
  if (p.preset) setOrDelete(out, "models", models);
  else out.models = models;
  return out;
}

export function validate(providers: ProviderDraft[], presets: Preset[]): string | null {
  const ids = new Set<string>();
  for (const p of providers) {
    const id = p.id.trim();
    if (!id || id.includes("/")) return "Provider ID 不能为空且不能包含 /";
    if (ids.has(id)) return `Provider ID 重复：${id}`;
    ids.add(id);
    const preset = presets.find((x) => x.id === p.preset);
    if (!p.baseURL.trim() && !preset?.baseURL && !preset?.local) return `Provider ${id} 缺少 Base URL`;
    if (!p.api && !p.raw.npm && !preset) return `Provider ${id} 需要选择协议`;
    const models = new Set<string>();
    for (const m of p.models) {
      const mid = m.id.trim();
      if (!mid) return `Provider ${id} 存在空的模型 ID`;
      if (models.has(mid)) return `Provider ${id} 模型 ID 重复：${mid}`;
      models.add(mid);
    }
  }
  return null;
}

export type LyraDraft = { model: string; providers: ProviderDraft[] };

export function draftFromConfig(raw: Record<string, any>): LyraDraft {
  const providers = isObject(raw.provider) ? raw.provider : {};
  return {
    model: typeof raw.model === "string" ? raw.model : "",
    providers: Object.entries(providers).map(([id, p]) => providerDraft(id, isObject(p) ? p : {})),
  };
}

/** raw 为加载时的完整配置，未接管的顶层键原样保留。 */
export function configFromDraft(raw: Record<string, any>, draft: LyraDraft) {
  const config: Record<string, any> = { ...raw };
  setOrDelete(config, "model", draft.model);
  config.provider = Object.fromEntries(draft.providers.map((p) => [p.id.trim(), serializeProvider(p)]));
  return config;
}
