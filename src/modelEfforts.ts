import type { SelectOption } from "./components/SearchSelect";

/** 强度档位由弱到强，与后端 backend_migration::EFFORTS 一致；未知档位（厂商自定义）
 *  排在已知档位之后并保持原有相对次序。 */
const EFFORT_RANK = ["none", "minimal", "default", "low", "medium", "high", "xhigh", "max", "ultra"];

/** 排序用：后端给的顺序不稳定（JSON 对象无键序、各家自排），展示时统一按档位强弱排。 */
export const effortRank = (effort: string) => {
  const i = EFFORT_RANK.indexOf(effort);
  return i < 0 ? EFFORT_RANK.length : i;
};

/** 各后端把思考强度折进模型 value：`<model>:<effort>`（Codex / ACP）或
 *  `<provider>/<model>/variant/<name>`（Lyra，模型 ID 可能自带 `:max` 之类，不按冒号拆）。
 *  拆出 [基础模型, 强度]，不是强度选项返回 null。 */
export function splitEffort(value: string, lyra: boolean): [string, string] | null {
  const m = lyra
    ? value.match(/^(.+)\/variant\/([^/]+)$/)
    : value.match(/^(.+):(none|minimal|default|low|medium|high|xhigh|max|ultra)$/);
  return m ? [m[1], m[2]] : null;
}

/** 沿当前模型的强度档走一步（delta > 0 更强），返回新的模型 value；
 *  没有可切换的档位或已在端点时返回 null。裸模型（未含强度）从选择器标注的默认档起算。 */
export function stepEffort(
  value: string,
  options: readonly SelectOption[],
  delta: number,
): string | null {
  if (!delta) return null;
  const parent = options.find(
    (o) => o.title === value || o.efforts?.some((e) => e.title === value),
  );
  const efforts = parent?.efforts ?? [];
  // 单档没有可切换目标；默认档不在列表（selectedLabel 带后缀）时 from = -1。
  if (!parent || efforts.length < 2) return null;
  const chosen = efforts.findIndex((e) => e.title === value);
  const from = chosen >= 0 ? chosen : efforts.findIndex((e) => e.label === parent.selectedLabel);
  const next = from + (delta > 0 ? 1 : -1);
  return next >= 0 && next < efforts.length ? efforts[next].title ?? null : null;
}

/** 按住 Alt 转动滚轮时逐档回调 delta（1 = 更强）；触摸板的小步累积到一格才回调。
 *  enabled 为 false 时不拦截，交给原本的滚动。返回注销函数，供 onSettled 返回。 */
export function watchAltWheel(
  onStep: (delta: number) => void,
  enabled: () => boolean = () => true,
): () => void {
  let acc = 0;
  const onWheel = (event: WheelEvent) => {
    if (!event.altKey || !enabled()) {
      acc = 0;
      return;
    }
    // Alt+滚轮没有需要保留的默认行为，拦下来免得同时滚 transcript。
    event.preventDefault();
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 400 : 1;
    if (acc && Math.sign(acc) !== Math.sign(event.deltaY)) acc = 0;
    acc += event.deltaY * unit;
    if (Math.abs(acc) < 50) return;
    const delta = acc > 0 ? 1 : -1;
    acc = 0;
    onStep(delta);
  };
  window.addEventListener("wheel", onWheel, { passive: false, capture: true });
  return () => window.removeEventListener("wheel", onWheel, { capture: true });
}

/** 同一模型的各强度收成一项，强度放进 efforts 作为下一级；直接选模型本身提交基础模型
 *  （后端默认强度，有 default 档时用它）。flat 的 title 须为原始 value。 */
export function foldEfforts(
  flat: SelectOption[],
  lyra: boolean,
  encode: (value: string) => string,
  favoriteId: (value: string) => string,
  defaultEfforts: ReadonlyMap<string, string> = new Map(),
): SelectOption[] {
  const out: SelectOption[] = [];
  const parents = new Map<string, SelectOption>();
  for (const option of flat) {
    const raw = option.title!;
    const split = splitEffort(raw, lyra);
    if (!split) {
      // 裸模型与其强度项并存时由父项代表
      if (!parents.has(raw)) out.push(option);
      continue;
    }
    const [base, effort] = split;
    const cut = option.label.lastIndexOf(" · ");
    let parent = parents.get(base);
    if (!parent) {
      parent = {
        ...option,
        value: encode(base),
        label: cut > 0 ? option.label.slice(0, cut) : option.label,
        title: base,
        favoriteId: favoriteId(base),
        efforts: [],
      };
      parents.set(base, parent);
      const bare = out.findIndex((o) => o.title === base);
      if (bare >= 0) out.splice(bare, 1, parent);
      else out.push(parent);
    }
    if (effort === "default") parent.value = option.value;
    // 行在第二屏展开显示，label 必须自带强度后缀才能区分同一模型的各档
    parent.efforts!.push({ ...option, label: cut > 0 ? option.label : `${option.label} · ${effort}` });
  }
  for (const [base, parent] of parents) {
    const rankOf = (o: SelectOption) => effortRank(splitEffort(o.title!, lyra)?.[1] ?? "");
    parent.efforts!.sort((a, b) => rankOf(a) - rankOf(b));
    const effort = defaultEfforts.get(base);
    const selected = parent.efforts!.find((o) => splitEffort(o.title!, lyra)?.[1] === effort);
    parent.selectedLabel = selected?.label ?? (effort ? `${parent.label} · ${effort}` : parent.label);
  }
  return out;
}
