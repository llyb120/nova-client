import type { ModelChoice, ModelOptions, ModelTarget, Settings } from "./types";

export const SERVER_SUPPORT_PROVIDER_ID = "server-support";

export function ensureServerSupportProvider(lyraConfig: Record<string, any>): {
  config: Record<string, any>;
  providerId: string;
  provider: Record<string, any>;
} {
  const existingProviders =
    lyraConfig?.provider && typeof lyraConfig.provider === "object" && !Array.isArray(lyraConfig.provider)
      ? (lyraConfig.provider as Record<string, any>)
      : {};
  const providers: Record<string, any> = { ...existingProviders };
  const existing = Object.entries(providers).find(
    ([, value]) => value && typeof value === "object" && value.preset === SERVER_SUPPORT_PROVIDER_ID,
  );
  if (existing) {
    return {
      config: { ...lyraConfig, provider: providers },
      providerId: existing[0],
      provider: existing[1],
    };
  }
  const provider = { preset: SERVER_SUPPORT_PROVIDER_ID };
  providers[SERVER_SUPPORT_PROVIDER_ID] = provider;
  return {
    config: { ...lyraConfig, provider: providers },
    providerId: SERVER_SUPPORT_PROVIDER_ID,
    provider,
  };
}

export function rankServerModelId(modelId: string): number {
  const lower = modelId.trim().toLowerCase();
  const bare = lower.includes("/") ? lower.slice(lower.lastIndexOf("/") + 1) : lower;
  if (bare === "deepseek-v4.1-flash") return 0;
  if (bare.startsWith("deepseek-v4.1-flash")) return 1;
  if (bare.includes("deepseek") && bare.includes("v4.1") && bare.includes("flash")) return 2;
  if (bare.includes("deepseek") && bare.includes("flash")) return 3;
  if (bare.includes("deepseek")) return 4;
  return 5;
}

function stripVariant(value: string): string {
  const idx = value.lastIndexOf("/variant/");
  return idx >= 0 ? value.slice(0, idx) : value;
}

function variantSuffix(value: string): string {
  const idx = value.lastIndexOf("/variant/");
  return idx >= 0 ? value.slice(idx + "/variant/".length) : "";
}

function lyraModelChoices(lyraOptions?: ModelOptions | null): ModelChoice[] {
  const modelOpt = lyraOptions?.configOptions?.find((o) => o.id === "model");
  return (modelOpt?.options as ModelChoice[] | undefined) ?? [];
}

interface Candidate {
  modelId: string;
  supportsImages: boolean;
  serverModel: boolean;
  serverCoding: boolean;
  serverComputer: boolean;
  serverModelVariant?: string;
  serverCodingVariant?: string;
  serverComputerVariant?: string;
  name: string;
  variants: string[];
}

function compareCandidates(a: Candidate, b: Candidate): number {
  return rankServerModelId(a.modelId) - rankServerModelId(b.modelId);
}

export function selectServerSceneModels(
  providerId: string,
  fetchedModels: Record<string, any> | null | undefined,
  lyraOptions?: ModelOptions | null,
): { lightweight: ModelTarget; coding: ModelTarget; computer: ModelTarget; displayName: string } {
  const choices = lyraModelChoices(lyraOptions);
  const providerPrefix = `${providerId}/`;
  const fetchedEntries =
    fetchedModels && typeof fetchedModels === "object"
      ? Object.entries(fetchedModels).filter(([, meta]) => meta && typeof meta === "object")
      : [];
  const providerChoices = choices.filter((c) => c.value.startsWith(providerPrefix));

  if (fetchedEntries.length === 0 && providerChoices.length === 0) {
    throw new Error("服务端未返回可用模型，请联系管理员检查中转站配置");
  }

  const candidates: Candidate[] = [];
  if (fetchedEntries.length > 0) {
    for (const [modelId, meta] of fetchedEntries) {
      const prefix = `${providerId}/${modelId}`;
      const matchingChoices = providerChoices.filter(
        (c) => c.value === prefix || c.value.startsWith(`${prefix}/variant/`),
      );
      const supportsImages =
        (Array.isArray(meta?.modalities?.input) && meta.modalities.input.includes("image")) ||
        matchingChoices.some((c) => c._meta?.["codex.ai/supportsImages"] === true);
      const name = typeof meta?.name === "string" && meta.name.trim() ? meta.name.trim() : modelId;
      const variants =
        meta?.variants && typeof meta.variants === "object" && !Array.isArray(meta.variants)
          ? Object.keys(meta.variants)
          : [];
      const optVariant = (key: string) =>
        typeof meta?.[key] === "string" && meta[key].trim() ? meta[key].trim() : undefined;
      candidates.push({
        modelId,
        supportsImages,
        serverModel: meta?.serverModel === true,
        serverCoding: meta?.serverCoding === true,
        serverComputer: meta?.serverComputer === true,
        serverModelVariant: optVariant("serverModelVariant"),
        serverCodingVariant: optVariant("serverCodingVariant"),
        serverComputerVariant: optVariant("serverComputerVariant"),
        name,
        variants,
      });
    }
  } else {
    const seen = new Map<string, Candidate>();
    for (const choice of providerChoices) {
      const base = stripVariant(choice.value);
      const modelId = base.slice(providerPrefix.length);
      if (!modelId) continue;
      const supportsImages = choice._meta?.["codex.ai/supportsImages"] === true;
      const existing = seen.get(modelId);
      if (existing) {
        if (supportsImages) existing.supportsImages = true;
      } else {
        const displayName = (choice.name || modelId).split(" · ")[0];
        seen.set(modelId, {
          modelId,
          supportsImages,
          serverModel: false,
          serverCoding: false,
          serverComputer: false,
          name: displayName,
          variants: [],
        });
      }
    }
    candidates.push(...seen.values());
  }

  if (candidates.length === 0) {
    throw new Error("服务端未返回可用模型，请联系管理员检查中转站配置");
  }

  const pickPreferredVariant = (variantNames: string[]): string | null => {
    if (variantNames.length === 0) return null;
    for (const preferred of ["medium", "high", "niu-m"]) {
      if (variantNames.includes(preferred)) return preferred;
    }
    return variantNames.find((v) => v !== "none") ?? variantNames[0];
  };

  const resolveOptionValue = (candidate: Candidate, explicitVariant?: string): string => {
    const prefix = `${providerId}/${candidate.modelId}`;
    if (explicitVariant) {
      return `${prefix}/variant/${explicitVariant}`;
    }
    if (providerChoices.some((c) => c.value === prefix)) return prefix;
    const variants = providerChoices.filter((c) => c.value.startsWith(`${prefix}/variant/`));
    if (variants.length > 0) {
      const defEffort = variants[0]?._meta?.["nova.ai/defaultReasoningEffort"];
      if (typeof defEffort === "string" && defEffort.trim()) {
        const defVal = `${prefix}/variant/${defEffort.trim()}`;
        if (variants.some((c) => c.value === defVal)) return defVal;
      }
      const preferred = pickPreferredVariant(variants.map((c) => variantSuffix(c.value)));
      if (preferred) {
        const match = variants.find((c) => variantSuffix(c.value) === preferred);
        if (match) return match.value;
      }
      return variants[0].value;
    }
    const fetchedVariant = pickPreferredVariant(candidate.variants);
    if (fetchedVariant) {
      return `${prefix}/variant/${fetchedVariant}`;
    }
    return prefix;
  };

  const sorted = [...candidates].sort(compareCandidates);
  const specifiedGeneral = candidates.find((c) => c.serverModel);
  const specifiedCoding = candidates.find((c) => c.serverCoding);
  const specifiedComputer = candidates.find((c) => c.serverComputer);

  const lightweightCandidate = specifiedGeneral ?? sorted[0];
  const codingCandidate = specifiedCoding ?? lightweightCandidate;
  const imageCandidates = sorted.filter((c) => c.supportsImages);
  const computerCandidate =
    specifiedComputer ??
    specifiedGeneral ??
    (imageCandidates.length > 0 ? imageCandidates[0] : lightweightCandidate);

  return {
    lightweight: {
      agentKind: "lyra",
      model: resolveOptionValue(lightweightCandidate, lightweightCandidate.serverModelVariant),
    },
    coding: {
      agentKind: "lyra",
      model: resolveOptionValue(
        codingCandidate,
        specifiedCoding ? codingCandidate.serverCodingVariant : lightweightCandidate.serverModelVariant,
      ),
    },
    computer: {
      agentKind: "lyra",
      model: resolveOptionValue(
        computerCandidate,
        specifiedComputer
          ? computerCandidate.serverComputerVariant
          : specifiedGeneral
            ? lightweightCandidate.serverModelVariant
            : undefined,
      ),
    },
    displayName: lightweightCandidate.name,
  };
}

export function resolveSimpleModeTarget(
  settings: Settings | null,
  lyraChoices: ModelChoice[],
): ModelTarget {
  const sceneTargets = [
    ...(settings?.sceneModels?.lightweight ?? []),
    ...(settings?.sceneModels?.coding ?? []),
    ...(settings?.sceneModels?.computer ?? []),
  ];
  for (const target of sceneTargets) {
    if (target.agentKind !== "lyra" || !target.model.trim()) continue;
    if (
      lyraChoices.length === 0 ||
      lyraChoices.some((c) => c.value === target.model || c.value.startsWith(`${target.model}/variant/`))
    ) {
      return { agentKind: "lyra", model: target.model };
    }
  }

  const serverChoices = lyraChoices.filter((c) => c.value.startsWith(`${SERVER_SUPPORT_PROVIDER_ID}/`));
  const pool = serverChoices.length > 0 ? serverChoices : lyraChoices;
  if (pool.length > 0) {
    const ranked = [...pool].sort((a, b) => {
      const aBase = stripVariant(a.value);
      const bBase = stripVariant(b.value);
      const aId = aBase.includes("/") ? aBase.slice(aBase.indexOf("/") + 1) : aBase;
      const bId = bBase.includes("/") ? bBase.slice(bBase.indexOf("/") + 1) : bBase;
      const rankDiff = rankServerModelId(aId) - rankServerModelId(bId);
      if (rankDiff !== 0) return rankDiff;
      const aNone = variantSuffix(a.value) === "none" ? 1 : 0;
      const bNone = variantSuffix(b.value) === "none" ? 1 : 0;
      return aNone - bNone;
    });
    return { agentKind: "lyra", model: ranked[0].value };
  }

  return {
    agentKind: "lyra",
    model: settings?.sceneModels?.lightweight?.[0]?.model ?? "",
  };
}

/** 简易模式开启且服务端明确指定了场景模型时，生成同步后的 Settings；无变化返回 null。 */
export function mergeSimpleModeServerScenes(
  settings: Settings | null | undefined,
  serverScenes: NonNullable<ModelOptions["serverSceneModels"]> | null | undefined,
): Settings | null {
  if (!settings?.simpleMode || !serverScenes) return null;
  const lightweight = serverScenes.lightweight?.trim() ?? "";
  const coding = serverScenes.coding?.trim() || lightweight;
  const computer = serverScenes.computer?.trim() || lightweight;
  if (!lightweight) return null;

  const sameScene = (list: ModelTarget[] | undefined, model: string) =>
    list?.length === 1 && list[0]?.agentKind === "lyra" && list[0]?.model === model;
  if (
    sameScene(settings.sceneModels?.lightweight, lightweight) &&
    sameScene(settings.sceneModels?.coding, coding) &&
    sameScene(settings.sceneModels?.computer, computer)
  ) {
    return null;
  }

  return {
    ...settings,
    sceneModels: {
      lightweight: [{ agentKind: "lyra", model: lightweight }],
      coding: [{ agentKind: "lyra", model: coding }],
      computer: [{ agentKind: "lyra", model: computer }],
    },
  };
}
