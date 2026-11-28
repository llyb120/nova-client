import type { ModelOptions, ModelTarget, Settings } from "./types";

export const SERVER_SUPPORT_PROVIDER_ID = "server-support";

export function ensureServerSupportProvider(lyraConfig: Record<string, any>) {
  const providers = { ...lyraConfig.provider };
  const existing = Object.entries(providers).find(([, p]) => (p as any)?.preset === SERVER_SUPPORT_PROVIDER_ID);
  const providerId = existing?.[0] ?? SERVER_SUPPORT_PROVIDER_ID;
  const provider = providers[providerId] = existing?.[1] ?? { preset: SERVER_SUPPORT_PROVIDER_ID };
  return { config: { ...lyraConfig, provider: providers }, providerId, provider };
}

export function selectServerSceneModels(providerId: string, fetched: Record<string, any> | null | undefined) {
  const match = Object.entries(fetched ?? {}).find(([, model]) => model?.serverModel === true);
  if (!match) throw new Error("中转站未指定可用模型，请管理员检查 provider.json 的 model");
  const [id, model] = match;
  const variants = Object.keys(model.variants ?? {}).sort();
  const variant = model.serverModelVariant || [model.options?.reasoningEffort, "medium", "high", "niu-m"].find(v => variants.includes(v)) || variants.find(v => v !== "none") || variants[0];
  const target: ModelTarget = { agentKind: "lyra", model: `${providerId}/${id}${variant ? `/variant/${variant}` : ""}` };
  return { lightweight: target, coding: target, computer: target, displayName: model.name || id };
}

// 模型 ID 不变时中转路径仍可能已升级；安装会话开始前等待路由刷新落盘。
export async function fetchSimpleModeTarget(api: Pick<typeof import("./ipc").api, "getLyraConfig" | "fetchLyraModels">): Promise<ModelTarget> {
  const config = await api.getLyraConfig();
  const { providerId, provider } = ensureServerSupportProvider(config);
  if (!config.provider?.[providerId]) throw new Error("尚未配置团队中转，请先完成简易模式设置");
  return selectServerSceneModels(providerId, await api.fetchLyraModels(providerId, provider)).lightweight;
}

export function resolveSimpleModeTarget(scenes: ModelOptions["serverSceneModels"] | undefined): ModelTarget {
  const model = scenes?.lightweight?.trim();
  if (!model) throw new Error("服务端模型尚未就绪，请检查中转站连接与 model 配置后重试");
  return { agentKind: "lyra", model };
}

export function mergeSimpleModeServerScenes(settings: Settings | null | undefined, scenes: ModelOptions["serverSceneModels"] | undefined | null): Settings | null {
  if (!settings?.simpleMode || !scenes?.lightweight?.trim()) return null;
  const target = resolveSimpleModeTarget(scenes);
  if ((["lightweight", "coding", "computer"] as const).every(k => {
    const list = settings.sceneModels?.[k];
    return list?.length === 1 && list[0].agentKind === target.agentKind && list[0].model === target.model;
  })) return null;
  return { ...settings, sceneModels: { lightweight: [target], coding: [target], computer: [target] } };
}
