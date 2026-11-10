import { createMemo } from "solid-js";
import {
  ALL_AGENT_KINDS,
  ensureModelOptions,
  lastUsed,
  modelChoices,
  modelFavoriteIds,
  resolveAvailableModel,
  state,
} from "../store";
import { isSupportedAgentKind, type AgentKind, type ModelOptions } from "../types";
import { agentLabel } from "../utils";
import { foldEfforts as foldModelEfforts, splitEffort } from "../modelEfforts";
import { SearchSelect, type SelectOption } from "./SearchSelect";

/** 模型/模式选项来源：漫游时返回对端（host）的列表；返回 undefined 表示用本机全局列表。 */
export type ModelOptionsSource = (agentKind: AgentKind) => ModelOptions | null | undefined;

/** `供应商 / 模型全名 · 强度` 拆成 [供应商, 模型全名]（供应商名本身可能带 " / "，从右往左找）；
 *  没有 " / " 时供应商为空。 */
function splitVendor(name: string): [string, string] {
  const cut = name.lastIndexOf(" · ");
  const base = cut > 0 ? name.slice(0, cut) : name;
  const i = base.lastIndexOf(" / ");
  return i > 0 ? [base.slice(0, i), base.slice(i + 3)] : ["", base];
}

/** 模型短名：取模型部分并去掉思考强度后缀；作为选择器左栏的分组。不同供应商的同一模型写法不一
 *  （Kimi-K3.1-Pro / Kimi K3.1 Pro），把连字符当空格合并成同一组。 */
function modelNameOf(name: string): string {
  return splitVendor(name)[1].replace(/[-_]+/g, " ").replace(/\s+/g, " ").trim();
}

/** 不改变型号身份的修饰词：等级（Pro/Max/Plus）与包装形式（1M 上下文 / Thinking / Preview / latest）。
 *  带不带都算同一个模型的不同版本。Flash / Fast / Mini / Lite / Code 这类是另一条产品线，不在此列。 */
const NEUTRAL_WORDS = new Set([
  "pro", "max", "ultra", "plus", "turbo", "high", "low", "xhigh", "highspeed", "premium",
  "m", "thinking", "preview", "latest", "exp",
]);

/** 同一模型只留最高版本（有 Claude Haiku 5.5 就不显示 4.5）：把名字里的数字和修饰词
 *  剥掉后完全一致才算同一模型的版本（Claude Sonnet 4.6 Thinking / 1M → 5.5、Kimi K3 → K3.1 Pro、
 *  DeepSeek V4 Flash → V4.1 Flash）；GPT-5.4 Mini、Gemini 3.5 Flash Lite、Opus 5.5 Fast
 *  这些是另一条线，各自保留。版本取名字里第一段数字。
 *  当前选中和收藏的模型不隐藏，否则选择会凭空消失。 */
export function latestModels(options: SelectOption[], keep?: string): SelectOption[] {
  /** a 版本是否高于 b（逐段比较，缺段按 0） */
  const higher = (a: number[], b: number[]) => {
    for (let i = 0; i < Math.max(a.length, b.length); i++) {
      const d = (a[i] ?? 0) - (b[i] ?? 0);
      if (d) return d > 0;
    }
    return false;
  };
  const pinned = (o: SelectOption) =>
    (!!keep &&
      (o.title === keep ||
        o.value === keep ||
        !!o.efforts?.some((e) => e.title === keep || e.value === keep))) ||
    [o, ...(o.efforts ?? [])].some(
      (x) => !!x.favoriteId && modelFavoriteIds().includes(x.favoriteId),
    );
  const identity = (o: SelectOption) => {
    const name = o.group ?? o.label;
    const version = /\d+(?:\.\d+)*/.exec(name)?.[0];
    const core = name
      .toLowerCase()
      .replace(/\d+(?:\.\d+)*/g, " ") // 含 1M / 27B 里的数字
      .split(/[^\p{L}]+/u)
      .filter((w) => w && !NEUTRAL_WORDS.has(w))
      .join(" ");
    return { core, version: version ? version.split(".").map(Number) : null };
  };
  const parsed = options.map((o) => ({ o, ...identity(o) }));
  return parsed
    .filter(
      ({ o, core, version }) =>
        !version ||
        pinned(o) ||
        !parsed.some(
          (other) =>
            other.core === core &&
            other.version !== null &&
            higher(other.version, version),
        ),
    )
    .map(({ o }) => o);
}

const fmt = (n: number) => String(Math.round(n * 100) / 100);

/** 把「后端 + 模型」编码进单个下拉 value，多后端时一次提交后端与模型。 */
const encodeModelValue = (agentKind: AgentKind, value: string) =>
  `${agentKind}:${encodeURIComponent(value)}`;

function decodeModelValue(value: string): { agentKind: AgentKind; model: string } | null {
  const i = value.indexOf(":");
  if (i <= 0) return null;
  const agentKind = value.slice(0, i) as AgentKind;
  if (!ALL_AGENT_KINDS.includes(agentKind)) return null;
  return { agentKind, model: decodeURIComponent(value.slice(i + 1)) };
}

/** 本地 CodeBuddy provider 的积分倍率随 description 下发。 */
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
 *  直接选模型本身提交基础模型（后端默认强度，Claude 有 default 档时用它）。
 *  keep：当前选中的原始模型，版本去重时保留，避免选中项从列表里消失。 */
export function modelOptionsOf(
  agentKind: AgentKind,
  merged: boolean,
  source?: ModelOptions | null,
  foldEfforts = false,
  keep?: string,
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
  const flat = modelChoices(agentKind, source)
    // 供应商的「默认模型」占位项（含 Devin 的「跟随 CLI 默认模型」）是别名，不与具体型号并列；
    // gpt-live / gpt-image 不是对话型号、gpt-5.5 已被更新版本取代，都不列；正选着时仍保留，避免选中项消失
    .filter((m) => !/默认模型）|gpt-?5[.-]5|gpt-?(?:live|image)/i.test(m.name) || m.value === keep)
    .map((m): SelectOption => {
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
      // 一级分组是模型名，供应商只体现在模型全名里；折叠强度时父模型与各档位沿用同一分组。
      group: modelNameOf(m.name),
      vendor: splitVendor(m.name)[0],
      detail: credits,
      detailTitle: credits && m.description ? `积分倍率：${m.description.trim()}` : undefined,
      vision: typeof metaVision === "boolean" ? metaVision : false,
      favoriteId: `${agentKind}:${encodeURIComponent(m.value)}`,
    };
  });
  if (!foldEfforts) return latestModels(flat, keep);
  return latestModels(
    foldModelEfforts(flat, agentKind === "lyra", encode, (v) => `${agentKind}:${encodeURIComponent(v)}`, defaultEfforts),
    keep,
  );
}

/** 把某后端的模型选项按一级分组（模型名）归拢，供原生 <select><optgroup> 使用（弹窗里不被 overflow 裁剪） */
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
 *  二级：模型名 → 供应商+模型全名，收藏置顶；同系列只留最高版本。
 *  思考强度在模型之后再多一级（不选则用默认强度）。多后端时值里仍编码 agentKind。
 *  工作模型与巡查/心跳模型共用此组件。 */
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
  // 调用方显式给出后端列表时，模型值里编码 agentKind 以便一次提交后端与模型。
  const merged = createMemo(() => props.agentKinds !== undefined);
  const sourceOf = (k: AgentKind) => props.modelSource?.(k);

  const modelOptions = createMemo<SelectOption[]>(() => {
    if (!merged()) {
      return modelOptionsOf(props.agentKind, false, sourceOf(props.agentKind), true, props.model);
    }
    return kinds().flatMap((k) => modelOptionsOf(k, true, sourceOf(k), true, props.model));
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
        (merged() ? "模型" : `模型（${agentLabel(props.agentKind)}）`)
      }
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
