import { createMemo } from "solid-js";
import {
  ALL_AGENT_KINDS,
  ensureModelOptions,
  lastUsed,
  modeChoices,
  modelChoices,
  normalizeUnifiedMode,
  state,
} from "../store";
import type { AgentKind, ModelCost, ModelOptions } from "../types";
import { agentLabel } from "../utils";
import { SearchSelect, type SelectOption } from "./SearchSelect";

/** 模型/模式选项来源：漫游时返回对端（host）的列表；返回 undefined 表示用本机全局列表。 */
export type ModelOptionsSource = (agentKind: AgentKind) => ModelOptions | null | undefined;

const PROVIDER_LABEL: Record<string, string> = {
  MODEL_PROVIDER_ANTHROPIC: "Claude",
  MODEL_PROVIDER_OPENAI: "GPT",
  MODEL_PROVIDER_GOOGLE: "Gemini",
  MODEL_PROVIDER_WINDSURF: "Windsurf",
  MODEL_PROVIDER_MOONSHOT: "Kimi",
  MODEL_PROVIDER_DEEPSEEK: "DeepSeek",
  MODEL_PROVIDER_XAI: "Grok",
  MODEL_PROVIDER_ZHIPU: "GLM",
};

/** 厂商分组：优先用后端 provider 字段，缺失时按命名启发式归类 */
function groupOf(value: string, name: string, cost?: ModelCost): string {
  const byProvider = cost?.provider && PROVIDER_LABEL[cost.provider];
  if (byProvider) return byProvider;
  const v = `${value} ${name}`.toLowerCase();
  if (v.includes("composer") || v.includes("cheetah")) return "Cursor";
  if (v.includes("claude") || v.includes("fable") || v.includes("opus") || v.includes("sonnet"))
    return "Claude";
  if (v.includes("gpt") || v.includes("codex") || v.includes("openai")) return "GPT";
  if (v.includes("gemini") || v.includes("google")) return "Gemini";
  if (v.includes("swe") || v.includes("adaptive") || v.includes("windsurf")) return "Windsurf";
  if (v.includes("kimi")) return "Kimi";
  if (v.includes("glm")) return "GLM";
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

/** 积分倍率文案；protobuf 省略零值，固定计费下倍率缺失即 0×（促销免费） */
function multiplierText(cost?: ModelCost): string | undefined {
  if (!cost) return undefined;
  if (cost.pricing && cost.pricing !== "MODEL_PRICING_TYPE_STATIC_CREDIT") return undefined;
  return `${cost.multiplier ?? 0}×`;
}

/** token 单价文案（输入/输出，美元每 1M tokens） */
function priceText(cost?: ModelCost): string | undefined {
  const p = cost?.prices;
  if (!p || (p.input == null && p.output == null)) return undefined;
  return `$${fmt(p.input ?? 0)}/$${fmt(p.output ?? 0)}`;
}

/** CodeBuddy 的费用（积分倍率）随 ACP 选项 description 下发，形如 "x0.79 credits"。
 *  解析成与 Devin 积分倍率一致的紧凑样式 "0.79×"，解析失败时回退去掉前缀的原文。 */
function creditsText(description?: string): string | undefined {
  if (!description || !/credits/i.test(description)) return undefined;
  const m = description.match(/x?\s*([\d.]+)\s*credits/i);
  if (!m) return description.trim();
  const n = Number(m[1]);
  return Number.isFinite(n) ? `${fmt(n)}×` : `${m[1]}×`;
}

/** 附注悬停说明：完整单价 + 倍率参考 */
function detailTitle(cost?: ModelCost): string | undefined {
  if (!cost) return undefined;
  const parts: string[] = [];
  const p = cost.prices;
  if (p && (p.input != null || p.output != null)) {
    parts.push(
      `每 1M tokens：输入 $${fmt(p.input ?? 0)} · 缓存命中 $${fmt(p.cached ?? 0)} · 输出 $${fmt(p.output ?? 0)}`,
    );
  }
  const mult = multiplierText(cost);
  if (mult) parts.push(`积分倍率：${mult}（参考）`);
  return parts.length ? parts.join("\n") : undefined;
}

/** 把某后端的模型列表映射为下拉选项；merged 时附带后端信息并编码 value。
 *  source 显式传入时用它（漫游用对端列表），不传则用本机全局列表。 */
export function modelOptionsOf(
  agentKind: AgentKind,
  merged: boolean,
  source?: ModelOptions | null,
): SelectOption[] {
  const costs = state.modelCosts;
  return modelChoices(agentKind, source).map((m) => {
    const cost = costs?.[m.value];
    const metaVision =
      m._meta?.["cognition.ai/supportsImages"] ?? m._meta?.["codex.ai/supportsImages"];
    const price = priceText(cost);
    const mult = multiplierText(cost);
    // CodeBuddy 无 windsurf 费用数据，改用 ACP 选项 description 里的积分倍率
    const credits = creditsText(m.description);
    return {
      value: merged ? encodeModelValue(agentKind, m.value) : m.value,
      label: m.name,
      title: m.value,
      group: groupOf(m.value, m.name, cost),
      backend: merged ? agentKind : undefined,
      backendLabel: merged ? agentLabel(agentKind) : undefined,
      // token 单价为主；没有单价的模型（促销/私有）退回显示倍率；CodeBuddy 退回积分倍率
      detail: price ?? mult ?? credits,
      detail2: price ? mult : undefined,
      detailTitle:
        detailTitle(cost) ??
        (credits && m.description ? `积分倍率：${m.description.trim()}` : undefined),
      vision: cost?.supportsImages ?? (typeof metaVision === "boolean" ? metaVision : false),
    };
  });
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
 *  Codex 思考强度已并入模型选项。工作模型与巡查/心跳模型共用此组件，保证体验一致。 */
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
}) {
  const kinds = createMemo(() => props.agentKinds ?? [props.agentKind]);
  const merged = createMemo(() => kinds().length > 1);
  const sourceOf = (k: AgentKind) => props.modelSource?.(k);

  const modelOptions = createMemo<SelectOption[]>(() => {
    if (!merged()) return modelOptionsOf(props.agentKind, false, sourceOf(props.agentKind));
    return kinds().flatMap((k) => modelOptionsOf(k, true, sourceOf(k)));
  });

  const effectiveModel = createMemo(() => {
    // 允许「默认」时空值合法（跟随默认），未命中列表的旧值也原样保留显示
    if (props.allowDefault) return props.model ?? "";
    const choices = modelChoices(props.agentKind, sourceOf(props.agentKind));
    if (props.model && (choices.length === 0 || choices.some((c) => c.value === props.model))) {
      return props.model;
    }
    return choices[0]?.value ?? props.model ?? "";
  });

  const modelValue = createMemo(() =>
    merged() ? encodeModelValue(props.agentKind, effectiveModel()) : effectiveModel(),
  );

  const onModelChange = (v: string) => {
    if (!merged()) {
      props.onPickModel(props.agentKind, v);
      return;
    }
    const decoded = decodeModelValue(v);
    if (decoded) props.onPickModel(decoded.agentKind, decoded.model);
  };

  const loadLocalOptions = () => {
    if (props.modelSource) return;
    for (const kind of kinds()) void ensureModelOptions(kind);
  };

  const fallbackLabel = createMemo(() => {
    if (props.modelSource) return undefined;
    const name = lastUsed.modelName(props.agentKind);
    return props.model && name ? name : undefined;
  });

  return (
    <SearchSelect
      prefix={props.prefix ?? "模型"}
      title={props.title ?? (merged() ? "后端 / 模型" : `模型（${agentLabel(props.agentKind)}）`)}
      value={modelValue()}
      options={modelOptions()}
      onOpen={loadLocalOptions}
      onChange={onModelChange}
      fallbackLabel={fallbackLabel()}
      searchable
      wide
      portal={props.portal}
      anchorTo={props.anchorTo}
      allowDefault={props.allowDefault}
      defaultLabel={props.defaultLabel}
    />
  );
}

/** 会话模式 + 模型两个下拉。模型部分复用 ModelPicker（与新会话/巡查模型一致）。 */
export function ConfigSelects(props: {
  agentKind: AgentKind;
  /** 可切换的后端列表（已启用）；>1 时模型下拉合并为三级菜单 */
  agentKinds?: AgentKind[];
  model: string;
  mode: string;
  /** 模型/模式选项来源（漫游时用对端列表）；不传则用本机全局列表 */
  modelSource?: ModelOptionsSource;
  /** 一次性提交「后端 + 模型」；单后端时 agentKind 即当前后端 */
  onPickModel: (agentKind: AgentKind, model: string) => void;
  onMode: (v: string) => void;
  /** 浮层是否用 Portal 渲染到 body（在弹窗/受限容器里避免被裁剪） */
  portal?: boolean;
  /** 浮层水平对齐到最近的祖先容器（如 composer），见 SearchSelect.anchorTo */
  anchorTo?: string;
}) {
  const sourceOf = (k: AgentKind) => props.modelSource?.(k);

  const modeOptions = createMemo(() =>
    modeChoices(props.agentKind, sourceOf(props.agentKind)).map((m) => ({
      value: m.id,
      label: m.name,
    })),
  );

  // 有效模式值：命中当前值则用它；旧值（bypass 等）归一到统一模式；否则回退第一项（Build）
  const modeValue = createMemo(() => {
    const opts = modeChoices(props.agentKind, sourceOf(props.agentKind));
    if (props.mode && opts.some((m) => m.id === props.mode)) return props.mode;
    const norm = normalizeUnifiedMode(props.mode);
    if (norm && opts.some((m) => m.id === norm)) return norm;
    return opts[0]?.id ?? "";
  });

  return (
    <>
      <SearchSelect
        prefix="模式"
        title="会话模式"
        value={modeValue()}
        options={modeOptions()}
        onChange={props.onMode}
        portal={props.portal}
        anchorTo={props.anchorTo}
      />
      <ModelPicker
        agentKind={props.agentKind}
        agentKinds={props.agentKinds}
        model={props.model}
        modelSource={props.modelSource}
        onPickModel={props.onPickModel}
        portal={props.portal}
        anchorTo={props.anchorTo}
      />
    </>
  );
}
