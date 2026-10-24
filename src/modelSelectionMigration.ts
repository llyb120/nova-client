export interface ModelSelectionMigrationTarget {
  agentKind: string;
  model: string | null;
  reasoningEffort: string | null;
}

export interface ModelSelectionMigrationResult {
  model?: string | null;
  error?: string | null;
}

/** alkaid 是 Lyra 的旧名，只改名不经后端迁移。 */
export const normalizeAgentKind = (kind: string) => (kind === "alkaid" ? "lyra" : kind);
const orphanWarnedKey = "fd:orphanModelWarned";

const selectionKey = /^fd:(?:lastAgentKind|lastUsedModel(?:Name)?|modelFavorites|[^:]+:last(?:Model(?:Name)?|ReasoningEffort))$/;

/** Rust owns model/provider mapping. Only the active selection becomes the current
 * Lyra model; retire inactive per-kind caches only after their mapping succeeds. */
export async function migrateStoredModelSelections(
  storage: Storage,
  migrate: (targets: ModelSelectionMigrationTarget[]) => Promise<ModelSelectionMigrationResult[]>,
): Promise<string[]> {
  const keys = Array.from({ length: storage.length }, (_, i) => storage.key(i)!)
    .filter(key => selectionKey.test(key));
  const before = new Map(keys.map(key => [key, storage.getItem(key)]));
  const get = (key: string) => before.get(key) ?? null;
  const rawFavorites = get("fd:modelFavorites");
  const favorites: unknown = JSON.parse(rawFavorites ?? "[]");
  if (!Array.isArray(favorites) || favorites.some(value => typeof value !== "string")) {
    throw new Error("模型收藏格式错误，已保留原始存储");
  }
  const targets: ModelSelectionMigrationTarget[] = [];
  const indices = new Map<string, number>();
  const errors: string[] = [];
  const add = (agentKind: string, model: string | null, reasoningEffort: string | null) => {
    // An empty kind has no known source backend; the backend could only reject it.
    if (!agentKind || ["lyra", "alkaid", "kimi", "cursor"].includes(agentKind)) return -1;
    const key = JSON.stringify([agentKind, model, reasoningEffort]);
    if (indices.has(key)) return indices.get(key)!;
    const index = targets.push({ agentKind, model, reasoningEffort }) - 1;
    indices.set(key, index);
    return index;
  };
  const rawKind = get("fd:lastAgentKind");
  const orphan = rawKind === null ? get("fd:lastUsedModel") : null;
  // An orphan global model has no known source backend; never attach it to Lyra or
  // send it to migration. Warn once per value instead of on every launch.
  if (orphan !== null && storage.getItem(orphanWarnedKey) !== orphan) {
    errors.push(`最近使用的模型 ${orphan} 缺少来源后端，未迁移；请在模型选择器中重新选择 Lyra 模型`);
    storage.setItem(orphanWarnedKey, orphan);
  }
  const currentKind = rawKind === null ? (orphan !== null ? "" : "lyra") : normalizeAgentKind(rawKind);
  const writes = new Map<string, string | null>();
  if (rawKind === "alkaid") {
    for (const suffix of ["lastModel", "lastModelName", "lastReasoningEffort"]) {
      const value = get(`fd:alkaid:${suffix}`);
      if (value === null) continue;
      if (get(`fd:lyra:${suffix}`) === null) writes.set(`fd:lyra:${suffix}`, value);
      writes.set(`fd:alkaid:${suffix}`, null);
    }
  }
  const current = add(currentKind, get("fd:lastUsedModel") ?? get(`fd:${currentKind}:lastModel`), get(`fd:${currentKind}:lastReasoningEffort`));
  const cachedTargets = new Map<string, number>();
  for (const key of keys) {
    const match = key.match(/^fd:([^:]+):last(?:Model|ReasoningEffort)$/);
    if (match && !["lyra", "alkaid"].includes(match[1])) cachedTargets.set(match[1], add(match[1], get(`fd:${match[1]}:lastModel`), get(`fd:${match[1]}:lastReasoningEffort`)));
  }
  const favoriteTargets = favorites.map((value: string) => {
    const colon = value.indexOf(":");
    try {
      if (colon <= 0) throw new Error();
      return add(value.slice(0, colon), decodeURIComponent(value.slice(colon + 1)), null);
    } catch {
      errors.push(`收藏格式错误：${value}`);
      return -1;
    }
  });
  const alkaidFavorites = favorites.some((value: string) => value.startsWith("alkaid:"));
  if (targets.length === 0 && rawKind !== "alkaid" && !alkaidFavorites) return errors;
  const results = targets.length ? await migrate(targets) : [];
  if (!Array.isArray(results) || results.length !== targets.length || results.some(result => !result ||
    (result.error != null && typeof result.error !== "string") ||
    (result.model != null && typeof result.model !== "string") ||
    !(typeof result.error === "string" && result.error.trim()) && !(typeof result.model === "string" && /^[^/]+\/.+/.test(result.model)))) {
    throw new Error("模型迁移返回结果无效，已保留原始存储");
  }
  const models = results.map((result, i) => {
    if (result.error?.trim()) {
      const target = targets[i];
      errors.push(`${target.agentKind || "未指定后端"} / ${target.model ?? "未指定模型"}${target.reasoningEffort ? ` (${target.reasoningEffort})` : ""}：${result.error}`);
      return null;
    }
    return result.model!;
  });
  // Other windows may update preferences while the backend saves providers.
  if (keys.some(key => storage.getItem(key) !== get(key)) || Array.from({ length: storage.length }, (_, i) => storage.key(i)!)
    .some(key => selectionKey.test(key) && !before.has(key))) {
    throw new Error("模型选择在迁移期间发生变化，未覆盖新选择，请重启后重试");
  }
  for (const [kind, index] of cachedTargets) {
    if (index < 0 || models[index] === null || (kind === currentKind && models[current] === null)) continue;
    for (const suffix of ["lastModel", "lastModelName", "lastReasoningEffort"]) {
      writes.set(`fd:${kind}:${suffix}`, null);
    }
  }
  if (current >= 0 && models[current] !== null) {
    writes.set("fd:lastUsedModel", models[current]);
    writes.set("fd:lyra:lastModel", models[current]);
    writes.set("fd:lastUsedModelName", null);
    writes.set("fd:lyra:lastModelName", null);
    // The mapped model already includes the effort variant; an independent stale
    // Lyra effort must not override it or leak into a later model selection.
    writes.set("fd:lyra:lastReasoningEffort", null);
  }
  const mappedFavorites = [...new Set(favorites.map((value: string, i: number) => {
    const model = models[favoriteTargets[i]];
    if (model != null) return `lyra:${encodeURIComponent(model)}`;
    return value.startsWith("alkaid:") ? `lyra:${value.slice("alkaid:".length)}` : value;
  }))];
  if (rawFavorites !== null && JSON.stringify(mappedFavorites) !== JSON.stringify(favorites)) {
    writes.set("fd:modelFavorites", JSON.stringify(mappedFavorites));
  }
  // Commit the kind last so a model write failure cannot pair legacy data with Lyra.
  if ((current >= 0 && models[current] !== null) || rawKind === "alkaid") writes.set("fd:lastAgentKind", "lyra");
  // localStorage has no transaction. Roll back a partial write (e.g. quota failure)
  // so a legacy model cannot become paired with a Lyra kind after a failed commit.
  try {
    for (const [key, value] of writes) {
      if (value === null) storage.removeItem(key);
      else storage.setItem(key, value);
    }
  } catch (error) {
    for (const key of writes.keys()) storage.removeItem(key);
    for (const key of writes.keys()) {
      const value = get(key);
      if (value === null) storage.removeItem(key);
      else storage.setItem(key, value);
    }
    throw error;
  }
  return errors;
}
