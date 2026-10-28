import { createMemo } from "solid-js";
import {
  ALL_AGENT_KINDS,
  ensureModelOptions,
  lastUsed,
  modelChoices,
  resolveAvailableModel,
  state,
} from "../store";
import { isSupportedAgentKind, type AgentKind, type ModelOptions } from "../types";
import { agentLabel } from "../utils";
import { foldEfforts as foldModelEfforts, splitEffort } from "../modelEfforts";
import { SearchSelect, type SelectOption } from "./SearchSelect";

/** 模型/模式选项来源：漫游时返回对端（host）的列表；返回 undefined 表示用本机全局列表。 */
export type ModelOptionsSource = (agentKind: AgentKind) => ModelOptions | null | undefined;

/** 厂商分组：按模型名称归类，Lyra provider 在上一级展示。 */
function groupOf(value: string, name: string): string {
  const v = `${value} ${name}`.toLowerCase();
  if (v.includes("claude") || v.includes("fable") || v.includes("opus") || v.includes("sonnet"))
    return "Claude";
  if (v.includes("gpt") || v.includes("codex") || v.includes("openai")) return "GPT";
  if (v.includes("gemini") || v.includes("google")) return "Gemini";
  if (v.includes("swe") || v.includes("adaptive") || v.includes("windsurf")) return "Windsurf";
  if (v.includes("kimi")) return "Kimi";
  if (v.includes("glm")) return "GLM";
  if (v.includes("qwen") || v.includes("tongyi")) return "Qwen";
  // Meta Muse Spark 系列（含 meta/ 前缀的中转模型名），避免落进「其他」分组
  if (v.includes("muse") || v.includes("meta/")) return "Meta";
  if (v.includes("deepseek")) return "DeepSeek";
  if (v.includes("grok")) return "Grok";
  if (v.includes("minimax")) return "MiniMax";
  if (v.includes("hunyuan") || v.includes("hy")) return "混元";
  return "其他";
}

const fmt = (n: number) => String(Math.round(n * 100) / 100);

/** 把「后端 + 模型」编码进单个下拉 value，便于三级菜单一次性提交。 */
const encodeModelValue = (agentKind: AgentKind, value: string) =>
  `${agentKind}:${encodeURIComponent(value)}`;

function decodeModelValue(value: string): { agentKind: AgentKind; model: string } | null {
  const i = value.indexOf(":");
  if (i <= 0) return null;
  const agentKind = value.slice(0, i) as AgentKind;
  if (!ALL_AGENT_KINDS.includes(agentKind)) return null;
  return { agentKind, model: decodeURIComponent(value.slice(i + 1)) };
}

/** 本地 CodeBuddy / WorkBuddy provider 的积分倍率随 description 下发。 */
function creditsText(description?: string): string | undefined {
  if (!description) return undefined;
  const m = description.match(/^\s*[x×]\s*(\d+(?:\.\d+)?)\s*$/i)
    ?? description.match(/x?\s*([\d.]+)\s*credits/i);
  if (!m) return /credits/i.test(description) ? description.trim() : undefined;
  const n = Number(m[1]);
  return Number.isFinite(n) ? `${fmt(n)}×` : `${m[1]}×`;
}

/** 把某后端的模型列表映射为下拉选项；merged 时附带后端信息并编码 value。
 *  source 显式传入时用它（漫游用对端列表），不传则用本机全局列表。
 *  foldEfforts：同一模型的各强度收成一项，强度放进 efforts 作为下一级；
 *  直接选模型本身提交基础模型（后端默认强度，Claude 有 default 档时用它）。 */
export function modelOptionsOf(
  agentKind: AgentKind,
  merged: boolean,
  source?: ModelOptions | null,
  foldEfforts = false,
): SelectOption[] {
  if (!isSupportedAgentKind(agentKind)) return [];
  const encode = (value: string) => (merged ? encodeModelValue(agentKind, value) : value);
  const config = (source !== undefined ? source : state.modelOptions[agentKind])?.configOptions;
  const effortConfig = config?.find((o) => o.id === "thought_level");
  // 与 ACP 展开模型档位时的默认解析一致；漫游/共享使用传入的配置。
  const acpEffort = effortConfig
    ? ["low", "high", "max"].includes(effortConfig.currentValue ?? "") ? effortConfig.currentValue : "high"
    : undefined;
  const defaultEfforts = new Map<string, string>();
  const flat = modelChoices(agentKind, source).map((m): SelectOption => {
    const effort = m._meta?.["nova.ai/defaultReasoningEffort"] ?? acpEffort;
    if (typeof effort === "string" && effort) {
      defaultEfforts.set(splitEffort(m.value, agentKind === "lyra")?.[0] ?? m.value, effort);
    }
    const metaVision =
      m._meta?.["cognition.ai/supportsImages"] ?? m._meta?.["codex.ai/supportsImages"];
    // 本地 provider 使用模型 description 里的积分倍率
    const credits = creditsText(m.description);
    return {
      value: encode(m.value),
      label: m.name,
      title: m.value,
      group: groupOf(m.value, m.name),
      // 在折叠强度前展开 provider，确保父模型与各档位指向同一个一级分组。
      backend: merged ? `lyra:${m.value.split("/")[0]}` : undefined,
      backendLabel: merged ? agentLabel(agentKind) : undefined,
      detail: credits,
      detailTitle: credits && m.description ? `积分倍率：${m.description.trim()}` : undefined,
      vision: typeof metaVision === "boolean" ? metaVision : false,
      favoriteId: `${agentKind}:${encodeURIComponent(m.value)}`,
    };
  });
  if (!foldEfforts) return flat;
  return foldModelEfforts(flat, agentKind === "lyra", encode, (v) => `${agentKind}:${encodeURIComponent(v)}`, defaultEfforts);
}

/** 把某后端的模型选项按厂商分组，供原生 <select><optgroup> 使用（弹窗里不被 overflow 裁剪） */
export function groupedModelOptions(
  agentKind: AgentKind,
  source?: ModelOptions | null,
): { group: string; items: SelectOption[] }[] {
  const order: string[] = [];
  const map = new Map<string, SelectOption[]>();
  for (const o of modelOptionsOf(agentKind, false, source)) {
    const g = o.group ?? "其他";
    if (!map.has(g)) {
      map.set(g, []);
      order.push(g);
    }
    map.get(g)!.push(o);
  }
  return order.map((group) => ({ group, items: map.get(group)! }));
}

/** 单独的「模型（含后端）」下拉——与新会话完全一致的选择器。
 *  - 单后端：二级（厂商 → 模型）。
 *  - 多后端（已启用 >1）：三级菜单（后端 → 厂商 → 模型），选中即同时提交后端与模型。
 *  思考强度在模型之后再多一级（不选则用默认强度）。工作模型与巡查/心跳模型共用此组件。 */
export function ModelPicker(props: {
  agentKind: AgentKind;
  agentKinds?: AgentKind[];
  model: string;
  modelSource?: ModelOptionsSource;
  onPickModel: (agentKind: AgentKind, model: string) => void;
  title?: string;
  prefix?: string;
  portal?: boolean;
  /** 浮层水平对齐到最近的祖先容器（如 composer），见 SearchSelect.anchorTo */
  anchorTo?: string;
  /** 提供「默认」入口（value=""，如标题/分享模型的「跟随默认」）；仅单后端形态使用 */
  allowDefault?: boolean;
  /** 「默认」入口的显示名 */
  defaultLabel?: string;
  /** 显示模型收藏入口。 */
  favorites?: boolean;
  /** 运行中禁止手动切换模型 */
  disabled?: boolean;
}) {
  const kinds = createMemo(() => (props.agentKinds ?? [props.agentKind]).filter(isSupportedAgentKind));
  // 调用方显式给出后端列表时始终保留「后端」一级，即使只有一个后端或模型尚未加载。
  // 这样设置里启用的后端不会因为模型探测失败而从选择器中完全消失。
  const merged = createMemo(() => props.agentKinds !== undefined);
  // Lyra 的每个 provider 单独占一行（不再有「Lyra」一行），避免 provider 多时挤在二级里。
  const lyraProviders = (o: SelectOption) => {
    const id = (o.title ?? "").split("/")[0];
    return { id: `lyra:${id}`, label: o.label.split(" / ")[0] || id };
  };
  const backendOptions = createMemo(() =>
    kinds().flatMap((kind) => {
      const seen = new Map<string, string>();
      for (const o of modelOptionsOf("lyra", true, props.modelSource?.("lyra"), true)) {
        const p = lyraProviders(o);
        if (!seen.has(p.id)) seen.set(p.id, p.label);
      }
      return seen.size
        ? [...seen].map(([id, label]) => ({ id, label }))
        : [{ id: kind, label: agentLabel(kind) }];
    }),
  );
  const sourceOf = (k: AgentKind) => props.modelSource?.(k);

  const modelOptions = createMemo<SelectOption[]>(() => {
    if (!merged()) return modelOptionsOf(props.agentKind, false, sourceOf(props.agentKind), true);
    return kinds().flatMap((k) => modelOptionsOf(k, true, sourceOf(k), true));
  });

  const effectiveModel = createMemo(() => {
    // 允许「默认」时空值合法（跟随默认），未命中列表的旧值也原样保留显示
    if (props.allowDefault) return props.model ?? "";
    // 已有明确选择：即使暂不在列表也保留（用 fallbackLabel 显示友好名），避免中间态闪成第一项/Auto
    if (props.model) return props.model;
    return resolveAvailableModel(props.agentKind, "", sourceOf(props.agentKind));
  });

  const modelValue = createMemo(() => {
    // allowDefault 时空值代表「默认/关闭」，保持 ""，避免触发器显示编码残片（如 "lyra:"）
    if (props.allowDefault && !effectiveModel()) return "";
    return merged() ? encodeModelValue(props.agentKind, effectiveModel()) : effectiveModel();
  });

  const onModelChange = (v: string) => {
    if (!merged()) {
      props.onPickModel(props.agentKind, v);
      return;
    }
    if (!v && props.allowDefault) {
      // 「默认/关闭」入口：merged 模式下空值无需解码，直接提交空模型
      props.onPickModel(props.agentKind, "");
      return;
    }
    const decoded = decodeModelValue(v);
    if (!decoded) return;
    props.onPickModel(decoded.agentKind, decoded.model);
  };

  const loadLocalOptions = () => {
    if (props.modelSource) return;
    for (const kind of kinds()) void ensureModelOptions(kind);
  };

  const fallbackLabel = createMemo(() => {
    if (!isSupportedAgentKind(props.agentKind)) return `${agentLabel(props.agentKind)}${props.model ? ` / ${props.model}` : ""}`;
    if (props.modelSource) return undefined;
    const name = lastUsed.modelName(props.agentKind);
    return props.model && props.model === lastUsed.model(props.agentKind) && name ? name : undefined;
  });

  return (
    <SearchSelect
      prefix={props.prefix ?? "模型"}
      title={
        props.title ??
        (merged() ? "后端 / 模型" : `模型（${agentLabel(props.agentKind)}）`)
      }
      value={modelValue()}
      options={modelOptions()}
      backendOptions={merged() ? backendOptions() : undefined}
      onOpen={loadLocalOptions}
      onChange={onModelChange}
      fallbackLabel={fallbackLabel()}
      searchable
      wide
      portal={props.portal}
      anchorTo={props.anchorTo}
      allowDefault={props.allowDefault}
      defaultLabel={props.defaultLabel}
      favorites={props.favorites}
      disabled={props.disabled}
    />
  );
}

/** 会话模式 + 模型两个下拉。模型部分复用 ModelPicker（与新会话/巡查模型一致）。 */
export function ConfigSelects(props: {
  agentKind: AgentKind;
  /** 可切换的后端列表（已启用）；提供后始终展示后端一级，再按需加载其模型 */
  agentKinds?: AgentKind[];
  model: string;
  /** 模型选项来源（漫游时用对端列表）；不传则用本机全局列表 */
  modelSource?: ModelOptionsSource;
  /** 一次性提交「后端 + 模型」；单后端时 agentKind 即当前后端 */
  onPickModel: (agentKind: AgentKind, model: string) => void;
  /** 浮层是否用 Portal 渲染到 body（在弹窗/受限容器里避免被裁剪） */
  portal?: boolean;
  /** 浮层水平对齐到最近的祖先容器（如 composer），见 SearchSelect.anchorTo */
  anchorTo?: string;
  /** 显示模型收藏入口。 */
  favorites?: boolean;
  /** 运行中禁止手动切换模型 */
  disabled?: boolean;
}) {
  return (
    <ModelPicker
      agentKind={props.agentKind}
      agentKinds={props.agentKinds}
      model={props.model}
      modelSource={props.modelSource}
      onPickModel={props.onPickModel}
      portal={props.portal}
      anchorTo={props.anchorTo}
      favorites={props.favorites}
      disabled={props.disabled}
    />
  );
}
