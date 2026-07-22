import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

export function alkaidDataRoot(home = homedir()) {
  return join(home, ".nova", "alkaid");
}

function stripJsonComments(text) {
  let result = "";
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    const next = text[index + 1];
    if (inString) {
      result += char;
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === '"') inString = false;
    } else if (char === '"') {
      inString = true;
      result += char;
    } else if (char === "/" && next === "/") {
      while (index < text.length && text[index] !== "\n") index += 1;
      result += "\n";
    } else if (char === "/" && next === "*") {
      index += 2;
      while (index < text.length && !(text[index] === "*" && text[index + 1] === "/")) {
        if (text[index] === "\n") result += "\n";
        index += 1;
      }
      index += 1;
    } else {
      result += char;
    }
  }
  return result;
}

function stripTrailingCommas(text) {
  let result = "";
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (inString) {
      result += char;
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === '"') inString = false;
      continue;
    }
    if (char === '"') inString = true;
    if (char === ",") {
      let next = index + 1;
      while (/\s/.test(text[next] ?? "")) next += 1;
      if (text[next] === "}" || text[next] === "]") continue;
    }
    result += char;
  }
  return result;
}

export function parseJsonc(text) {
  return JSON.parse(stripTrailingCommas(stripJsonComments(text)));
}

function resolveEnv(value, env) {
  if (typeof value !== "string") return value;
  return value.replace(/\{env:([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => {
    const resolved = env[name];
    if (resolved == null) throw new Error(`Alkaid 配置引用的环境变量 ${name} 未注入 Nova 进程`);
    return resolved;
  });
}

function providerApi(provider) {
  if (provider.api) return provider.api;
  const npm = provider.npm ?? "";
  if (npm.includes("anthropic")) return "anthropic-messages";
  if (npm.includes("google")) return "google-generative-ai";
  if (npm.includes("openai-compatible")) return "openai-completions";
  if (npm.includes("openai")) return "openai-responses";
  throw new Error("Alkaid provider 缺少 api，且无法从 npm 推导协议");
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** 服务端配置作基线，本地配置递归覆盖；数组与标量由本地整体替换。 */
export function mergeAlkaidConfig(serverConfig, localConfig) {
  if (!isPlainObject(serverConfig)) return structuredClone(localConfig ?? {});
  if (!isPlainObject(localConfig)) return structuredClone(serverConfig);
  const merged = structuredClone(serverConfig);
  for (const [key, localValue] of Object.entries(localConfig)) {
    merged[key] = isPlainObject(localValue) && isPlainObject(merged[key])
      ? mergeAlkaidConfig(merged[key], localValue)
      : structuredClone(localValue);
  }
  return merged;
}

export async function loadAlkaidConfig({ root = alkaidDataRoot(), env = process.env, serverConfig } = {}) {
  const path = join(root, "config.jsonc");
  let localConfig = {};
  try {
    localConfig = parseJsonc(await readFile(path, "utf8"));
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw new Error(`读取 Alkaid 配置失败：${error instanceof Error ? error.message : String(error)}`);
    }
    if (!isPlainObject(serverConfig)) throw new Error(`未找到 Alkaid 配置：${path}`);
  }
  const config = mergeAlkaidConfig(serverConfig, localConfig);
  if (!config?.provider || typeof config.provider !== "object") {
    throw new Error("Alkaid 配置缺少 provider");
  }
  return { ...config, root, env };
}

export function defaultAlkaidModel(config) {
  const options = alkaidModelOptions(config);
  let selection = config.model;
  if (selection && !options.some((option) => option.value === selection)) {
    const [providerId, ...modelParts] = selection.split("/");
    const model = config.provider[providerId]?.models?.[modelParts.join("/")];
    const effort = model?.options?.reasoningEffort;
    selection = options.find((option) => option.value === `${config.model}/variant/${effort}`)?.value;
  }
  selection ??= options[0]?.value;
  if (!selection) throw new Error("Alkaid 配置没有可用模型");
  return selection;
}

/**
 * Fill cache/routing compat defaults without overriding explicit user/server values.
 * Inspired by pi-cache-optimizer guidance for OpenAI-compatible proxies and reasoning models.
 */
export function mergeAlkaidCompatDefaults(api, modelId, baseUrl, existing = undefined) {
  const compat = isPlainObject(existing) ? { ...existing } : {};
  const id = String(modelId ?? "").toLowerCase();
  const url = String(baseUrl ?? "").toLowerCase();
  const isOfficialOpenAI = url.includes("api.openai.com");

  if (api === "openai-completions" && !isOfficialOpenAI && compat.sendSessionAffinityHeaders === undefined) {
    compat.sendSessionAffinityHeaders = true;
  }
  if (api === "anthropic-messages" && !url.includes("api.anthropic.com") && compat.sendSessionAffinityHeaders === undefined) {
    compat.sendSessionAffinityHeaders = true;
  }

  if (/\bdeepseek\b/.test(id)) {
    if (compat.thinkingFormat === undefined) compat.thinkingFormat = "deepseek";
    if (compat.requiresReasoningContentOnAssistantMessages === undefined) {
      compat.requiresReasoningContentOnAssistantMessages = true;
    }
  }

  if (/\bk3\b|kimi-for-coding|kimi-k3/.test(id)) {
    if (compat.forceAdaptiveThinking === undefined) compat.forceAdaptiveThinking = true;
    if (compat.allowEmptySignature === undefined) compat.allowEmptySignature = true;
  }

  // Claude adaptive-thinking models (opus/sonnet 4.6+, fable-5+, sonnet-5)
  if (
    /claude/.test(id)
    && (
      /opus-4(?:\.|-)6/.test(id)
      || /sonnet-4(?:\.|-)6/.test(id)
      || /sonnet-5/.test(id)
      || /fable-5/.test(id)
      || /claude-sonnet-5/.test(id)
    )
    && compat.forceAdaptiveThinking === undefined
  ) {
    compat.forceAdaptiveThinking = true;
  }

  return Object.keys(compat).length ? compat : existing;
}

export function resolveAlkaidModel(config, selection = defaultAlkaidModel(config)) {
  if (!selection || !selection.includes("/")) throw new Error("Alkaid model 必须是 provider/model 格式");
  const marker = "/variant/";
  const variantIndex = selection.lastIndexOf(marker);
  const variant = variantIndex >= 0 ? selection.slice(variantIndex + marker.length) : undefined;
  const baseSelection = variantIndex >= 0 ? selection.slice(0, variantIndex) : selection;
  const [providerId, ...modelParts] = baseSelection.split("/");
  const modelId = modelParts.join("/");
  const provider = config.provider[providerId];
  const model = provider?.models?.[modelId];
  if (!provider) throw new Error(`Alkaid provider 不存在：${providerId}`);
  if (!model) throw new Error(`Alkaid model 不存在：${baseSelection}`);
  if (variant && !Object.hasOwn(model.variants ?? {}, variant)) {
    throw new Error(`Alkaid model 不支持思考强度：${selection}`);
  }
  const options = provider.options ?? {};
  const baseUrl = resolveEnv(options.baseURL ?? options.baseUrl, config.env);
  if (!baseUrl) throw new Error(`Alkaid provider 缺少 options.baseURL：${providerId}`);
  const variants = Object.fromEntries(Object.entries(model.variants ?? {}).map(([level, value]) => [
    level,
    value?.reasoningEffort ?? null,
  ]));
  const api = providerApi(provider);
  return {
    apiKey: resolveEnv(options.apiKey, config.env),
    thinkingLevel: variant
      ? model.variants[variant]?.reasoningEffort ?? variant
      : model.options?.reasoningEffort,
    model: {
      id: modelId,
      name: model.name ?? modelId,
      api,
      provider: providerId,
      baseUrl,
      reasoning: model.reasoning ?? Object.keys(model.variants ?? {}).length > 0,
      thinkingLevelMap: Object.keys(variants).length ? variants : undefined,
      input: model.modalities?.input?.filter((value) => value === "text" || value === "image") ?? ["text"],
      cost: model.cost ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: model.limit?.context ?? 128000,
      maxTokens: model.limit?.output ?? 32000,
      headers: options.headers,
      compat: mergeAlkaidCompatDefaults(api, modelId, baseUrl, model.compat ?? provider.compat),
    },
  };
}

function variantLabel(variant) {
  const labels = { minimal: "Minimal", low: "Low", medium: "Medium", high: "High", xhigh: "XHigh", max: "Max" };
  return labels[variant] ?? variant;
}

export function alkaidModelOptions(config) {
  return Object.entries(config.provider).flatMap(([providerId, provider]) =>
    Object.entries(provider.models ?? {}).flatMap(([modelId, model]) => {
      const value = `${providerId}/${modelId}`;
      const name = `${provider.name ?? providerId} / ${model.name ?? modelId}`;
      const meta = { "codex.ai/supportsImages": model.modalities?.input?.includes("image") ?? false };
      const variants = Object.keys(model.variants ?? {});
      if (variants.length === 0) return [{ value, name, _meta: meta }];
      return variants.map((variant) => ({
        value: `${value}/variant/${variant}`,
        name: `${name} · ${variantLabel(variant)}`,
        _meta: meta,
      }));
    }),
  );
}
