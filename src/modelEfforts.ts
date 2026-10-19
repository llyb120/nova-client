import type { SelectOption } from "./components/SearchSelect";

/** 各后端把思考强度折进模型 value：`<model>:<effort>`（Codex / ACP）或
 *  `<provider>/<model>/variant/<name>`（Lyra，模型 ID 可能自带 `:max` 之类，不按冒号拆）。
 *  拆出 [基础模型, 强度]，不是强度选项返回 null。 */
export function splitEffort(value: string, lyra: boolean): [string, string] | null {
  const m = lyra
    ? value.match(/^(.+)\/variant\/([^/]+)$/)
    : value.match(/^(.+):(none|minimal|default|low|medium|high|xhigh|max|ultra)$/);
  return m ? [m[1], m[2]] : null;
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
    parent.efforts!.push({ ...option, short: cut > 0 ? option.label.slice(cut + 3) : effort });
  }
  for (const [base, parent] of parents) {
    const effort = defaultEfforts.get(base);
    const selected = parent.efforts!.find((o) => splitEffort(o.title!, lyra)?.[1] === effort);
    parent.selectedLabel = selected?.label ?? effort ? `${parent.label} · ${effort}` : parent.label;
  }
  return out;
}
