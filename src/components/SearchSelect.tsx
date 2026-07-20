import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { IconCheck, IconChevron, IconEye, IconStar } from "./icons";

const FAVORITE_GROUP = "收藏";
const FAVORITE_BACKEND = "__favorites__";
const MODEL_FAVORITES_KEY = "fd:modelFavorites";

function storedFavorites(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(MODEL_FAVORITES_KEY) ?? "[]");
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

export interface SelectOption {
  value: string;
  label: string;
  title?: string;
  /** 分组名（厂商）；任一选项带分组时整个下拉切换为二级面板 */
  group?: string;
  /** 后端（agentKind）；任一选项带后端时切换为三级面板：后端 → 厂商 → 模型 */
  backend?: string;
  /** 后端显示名（如 Devin / Codex / CodeBuddy），三级面板左栏使用 */
  backendLabel?: string;
  /** 该项是某后端的「默认」（跟随 agent 默认模型）入口，三级面板里置顶直接可选 */
  isDefault?: boolean;
  /** 右侧主附注，如 token 单价 "$5/$25" */
  detail?: string;
  /** 右侧次附注（参考信息），如积分倍率 "25×" */
  detail2?: string;
  /** 附注的悬停说明 */
  detailTitle?: string;
  /** 是否支持图片输入（显示眼睛标识） */
  vision?: boolean;
  /** 跨后端唯一的收藏标识；未提供时该选项不显示收藏按钮 */
  favoriteId?: string;
}

/** 可搜索的下拉选择器（浮层向上弹出，适合放在 composer 工具条）。
 *  支持三种形态：扁平 / 二级（厂商→模型）/ 三级（后端→厂商→模型）。 */
export function SearchSelect(props: {
  value: string;
  options: SelectOption[];
  onChange: (v: string, option?: SelectOption) => void;
  /** 触发器前缀，如 "模型" */
  prefix: string;
  /** value 为 "" 时显示的选项名 */
  defaultLabel?: string;
  /** 当前 value 不在 options 里时的触发器文案（如 lastModelName，避免闪裸 id） */
  fallbackLabel?: string;
  /** 用户打开下拉时按需准备选项。 */
  onOpen?: () => void;
  searchable?: boolean;
  title?: string;
  /** 触发器完整显示文本、不压缩（用于模型名等需要看全名的场景） */
  wide?: boolean;
  /** 是否提供「默认」（value=""）入口。默认关闭：不显示「默认」，空值时回退到第一项。 */
  allowDefault?: boolean;
  /** 通过 Portal + fixed 定位浮层，避免被 overflow 容器（如设置弹窗滚动区）裁剪 */
  portal?: boolean;
  /** 浮层水平对齐到最近的祖先容器（CSS 选择器，如 ".home-composer"）：
   *  宽度不超过容器、贴容器边缘对齐，观感与输入框浑然一体；隐含 portal 行为。 */
  anchorTo?: string;
  /** 为模型项显示持久化收藏按钮，并在分组首项展示收藏模型。 */
  favorites?: boolean;
}) {
  const [opened, setOpened] = createSignal(false);
  const [query, setQuery] = createSignal("");
  /** 二级面板当前悬停/选中的分组；null 表示跟随当前值所在分组 */
  const [activeGroup, setActiveGroup] = createSignal<string | null>(null);
  /** 三级面板当前悬停/选中的后端；null 表示跟随当前值所在后端 */
  const [activeBackend, setActiveBackend] = createSignal<string | null>(null);
  /** 弹层方向：空间不足时向下翻转 / 右对齐，避免溢出窗口 */
  const [place, setPlace] = createSignal({ down: false, right: false });
  const [favoriteIds, setFavoriteIds] = createSignal(storedFavorites());
  /** portal 模式下的浮层 fixed 坐标（相对视口） */
  const [coords, setCoords] = createSignal<{
    left: number;
    top?: number;
    bottom?: number;
    width: number;
  }>({ left: 0, width: 280 });
  let rootRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;

  const defaultLabel = () => props.defaultLabel ?? "默认";

  /** 第一个可选项（跳过「默认」入口）。空值且不提供「默认」时作为回退默认项。 */
  const firstSelectable = createMemo(() => props.options.find((o) => !o.isDefault));

  /** 是否启用三级面板（后端 → 厂商 → 模型） */
  const isThreeLevel = createMemo(() => props.options.some((o) => o.backend));
  /** 是否启用分组二级面板（搜索时退化为扁平列表） */
  const isGrouped = createMemo(() => props.options.some((o) => o.group));

  const currentOption = createMemo(() => props.options.find((o) => o.value === props.value));

  /** 高亮/选中的有效值：命中当前值则用它；否则允许「默认」时为 ""，不允许则回退到第一项。 */
  const activeValue = createMemo(() => {
    if (currentOption()) return props.value;
    if (props.allowDefault) return "";
    return firstSelectable()?.value ?? props.value;
  });

  const currentLabel = createMemo(() => {
    // 不提供「默认」时，空值/未命中回退到第一项作为默认显示
    const opt = currentOption() ?? (props.allowDefault ? undefined : firstSelectable());
    if (opt) {
      // 三级（合并后端）模式下，触发器带上后端名便于区分
      if (isThreeLevel() && opt.backendLabel) {
        return opt.isDefault ? `${opt.backendLabel} 默认` : `${opt.backendLabel} · ${opt.label}`;
      }
      return opt.label;
    }
    if (props.fallbackLabel && props.value) return props.fallbackLabel;
    return props.value ? props.value : defaultLabel();
  });

  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    if (!q) return props.options.filter((o) => !o.isDefault);
    return props.options.filter(
      (o) =>
        !o.isDefault &&
        (o.label.toLowerCase().includes(q) ||
          o.value.toLowerCase().includes(q) ||
          (o.backendLabel?.toLowerCase().includes(q) ?? false)),
    );
  });

  const isFavorite = (o: SelectOption) => !!o.favoriteId && favoriteIds().includes(o.favoriteId);
  const toggleFavorite = (o: SelectOption) => {
    if (!o.favoriteId) return;
    const next = isFavorite(o)
      ? favoriteIds().filter((id) => id !== o.favoriteId)
      : [...favoriteIds(), o.favoriteId];
    setFavoriteIds(next);
    localStorage.setItem(MODEL_FAVORITES_KEY, JSON.stringify(next));
  };

  // ===== 二级（厂商→模型）=====
  const groups = createMemo(() => {
    const order: string[] = [];
    const map = new Map<string, SelectOption[]>();
    for (const o of props.options) {
      if (o.isDefault) continue;
      const g = o.group ?? "其他";
      if (!map.has(g)) {
        map.set(g, []);
        order.push(g);
      }
      map.get(g)!.push(o);
    }
    const result = order.map((name) => ({ name, items: map.get(name)! }));
    const favorites = props.favorites ? props.options.filter(isFavorite) : [];
    return favorites.length ? [{ name: FAVORITE_GROUP, items: favorites }, ...result] : result;
  });

  const currentGroup = () => currentOption()?.group;
  const shownGroup = createMemo(() => {
    const active = activeGroup();
    if (active && groups().some((group) => group.name === active)) return active;
    return currentGroup() ?? groups()[0]?.name;
  });
  const shownItems = createMemo(() => groups().find((g) => g.name === shownGroup())?.items ?? []);

  // ===== 三级（后端→厂商→模型）=====
  const backends = createMemo(() => {
    const order: string[] = [];
    const labels = new Map<string, string>();
    for (const o of props.options) {
      if (!o.backend) continue;
      if (!labels.has(o.backend)) {
        labels.set(o.backend, o.backendLabel ?? o.backend);
        order.push(o.backend);
      }
    }
    const result = order.map((id) => ({ id, label: labels.get(id)! }));
    const hasFavorites = props.favorites && props.options.some(isFavorite);
    return hasFavorites
      ? [{ id: FAVORITE_BACKEND, label: FAVORITE_GROUP }, ...result]
      : result;
  });
  const currentBackend = () => currentOption()?.backend;
  const shownBackend = createMemo(() => {
    const active = activeBackend();
    if (active && backends().some((backend) => backend.id === active)) return active;
    return currentBackend() ?? backends()[0]?.id;
  });
  const showingFavorites = () => shownBackend() === FAVORITE_BACKEND;
  const chooseBackend = (id: string) => {
    setActiveBackend(id);
    setActiveGroup(null);
  };
  /** 当前后端下的「默认」入口（跟随 agent 默认模型） */
  const defaultOfBackend = createMemo(() =>
    props.options.find((o) => o.backend === shownBackend() && o.isDefault),
  );
  /** 当前后端下、按厂商分组的模型 */
  const providersOfBackend = createMemo(() => {
    const b = shownBackend();
    const order: string[] = [];
    const map = new Map<string, SelectOption[]>();
    for (const o of props.options) {
      if (o.backend !== b || o.isDefault) continue;
      const g = o.group ?? "其他";
      if (!map.has(g)) {
        map.set(g, []);
        order.push(g);
      }
      map.get(g)!.push(o);
    }
    return order.map((name) => ({ name, items: map.get(name)! }));
  });
  const shownProvider = createMemo(() => {
    const active = activeGroup();
    if (active && providersOfBackend().some((group) => group.name === active)) return active;
    if (shownBackend() === currentBackend() && currentGroup()) return currentGroup();
    return providersOfBackend()[0]?.name;
  });
  const shownModels = createMemo(() => {
    if (showingFavorites()) return props.options.filter((o) => !o.isDefault && isFavorite(o));
    return providersOfBackend().find((g) => g.name === shownProvider())?.items ?? [];
  });

  const pick = (v: string) => {
    props.onChange(
      v,
      props.options.find((o) => o.value === v),
    );
    setOpened(false);
    setQuery("");
  };

  const popWidth = () => (isThreeLevel() ? 640 : isGrouped() ? 480 : 280);

  /** anchorTo 隐含 portal：对齐容器的浮层必须脱离 overflow 祖先才能不被裁剪 */
  const usePortal = () => props.portal || !!props.anchorTo;

  const computePlacement = () => {
    if (!rootRef) return;
    const r = rootRef.getBoundingClientRect();
    const width = popWidth();
    const big = isThreeLevel() || isGrouped();
    // 与 CSS 实际高度保持一致（大面板 min(320px,50vh) + 搜索框；扁平列表按选项数），
    // 估得过大将导致方向误翻 / 位置漂移
    const flatCount =
      props.options.filter((o) => !o.isDefault).length + (props.allowDefault ? 1 : 0);
    const height = big
      ? Math.min(320, window.innerHeight * 0.5) + (props.searchable ? 48 : 8)
      : Math.min(flatCount * 33 + 12, 272) + (props.searchable ? 46 : 8);
    const anchor = props.anchorTo
      ? rootRef.closest(props.anchorTo)?.getBoundingClientRect()
      : undefined;
    // 大面板锚定容器时贴容器上/下沿整体出现；
    // 小的扁平下拉仍贴触发器，避免两行选项孤零零飘在容器另一侧
    const box = big && anchor ? anchor : r;
    const spaceBelow = window.innerHeight - box.bottom;
    // 容器锚定优先向下弹（不遮住输入区），放不下再向上；其余沿用向上优先
    const down =
      big && anchor
        ? spaceBelow >= height + 8 || (box.top < height + 8 && spaceBelow > box.top)
        : r.top < height && window.innerHeight - r.bottom > r.top;
    const right = r.left + width > window.innerWidth - 8;
    setPlace({ down, right });
    if (!usePortal()) return;
    let w = Math.min(width, window.innerWidth - 16);
    let left: number;
    if (anchor) {
      // 对齐容器（composer）：宽度不超过容器，优先与触发器左对齐、右缘不越出容器，
      // 撑满时与容器边缘齐平，观感与输入框成一体
      w = Math.min(w, anchor.width);
      left = Math.max(anchor.left, Math.min(r.left, anchor.right - w));
    } else {
      left = right ? Math.max(8, r.right - w) : r.left;
    }
    left = Math.max(8, Math.min(left, window.innerWidth - w - 8));
    if (down) {
      // 下方空间不足时整体上移，保证浮层完整可见
      const top = Math.max(8, Math.min(box.bottom + 8, window.innerHeight - height - 8));
      setCoords({ left, top, width: w });
    } else {
      const bottom = Math.max(
        8,
        Math.min(window.innerHeight - box.top + 8, window.innerHeight - height - 8),
      );
      setCoords({ left, bottom, width: w });
    }
  };

  const toggle = () => {
    const willOpen = !opened();
    if (willOpen) {
      props.onOpen?.();
      computePlacement();
    }
    setOpened(willOpen);
    if (willOpen) {
      setQuery("");
      setActiveGroup(null);
      setActiveBackend(null);
      if (props.searchable) queueMicrotask(() => searchRef?.focus());
    }
  };

  // portal 模式：浮层脱离 overflow 容器用 fixed 定位，需随滚动/缩放实时重算坐标
  createEffect(() => {
    if (!usePortal() || !opened()) return;
    void props.options.length;
    const onReflow = () => computePlacement();
    window.addEventListener("scroll", onReflow, true);
    window.addEventListener("resize", onReflow);
    onCleanup(() => {
      window.removeEventListener("scroll", onReflow, true);
      window.removeEventListener("resize", onReflow);
    });
  });

  let popRef: HTMLDivElement | undefined;
  const onDocClick = (e: MouseEvent) => {
    const target = e.target as Node;
    // portal 模式下浮层不在 rootRef 内，需同时排除浮层自身，否则点浮层会误关
    if (
      rootRef &&
      !rootRef.contains(target) &&
      (!popRef || !popRef.contains(target))
    ) {
      setOpened(false);
    }
  };
  document.addEventListener("mousedown", onDocClick);
  onCleanup(() => document.removeEventListener("mousedown", onDocClick));

  const itemRow = (o: SelectOption, showBackend = false) => (
    <div
      class={`sel-item ${showBackend ? "with-source" : ""} ${
        o.value === activeValue() ? "active" : ""
      }`}
      onClick={() => pick(o.value)}
      title={o.title ?? o.value}
    >
      <span class="sel-model-copy">
        <span class="sel-label">{o.label}</span>
        <Show when={showBackend && o.backendLabel}>
          <span class="sel-model-source">{o.backendLabel}</span>
        </Show>
      </span>
      <Show when={o.vision}>
        <span class="sel-vision" title="支持图片输入">
          <IconEye size={12} />
        </span>
      </Show>
      <Show when={o.detail}>
        <span class="sel-detail" title={o.detailTitle}>
          {o.detail}
        </span>
      </Show>
      <Show when={o.detail2}>
        <span class="sel-detail dim" title={o.detailTitle}>
          {o.detail2}
        </span>
      </Show>
      <Show when={props.favorites && o.favoriteId}>
        <button
          type="button"
          class={`sel-favorite ${isFavorite(o) ? "active" : ""}`}
          title={isFavorite(o) ? "取消收藏" : "收藏模型"}
          onClick={(event) => {
            event.stopPropagation();
            toggleFavorite(o);
          }}
        >
          <IconStar size={14} filled={isFavorite(o)} />
        </button>
      </Show>
      <Show when={!showBackend && o.value === activeValue()}>
        <IconCheck size={13} />
      </Show>
    </div>
  );

  const renderPop = () => (
    <div
      ref={popRef}
      class={`sel-pop ${isThreeLevel() ? "three" : ""} ${
        isGrouped() || isThreeLevel() ? "grouped" : ""
      } ${!usePortal() && place().down ? "down" : ""} ${
        !usePortal() && place().right ? "right" : ""
      } ${usePortal() ? "portal" : ""}`}
      style={
        usePortal()
          ? {
              position: "fixed",
              left: `${coords().left}px`,
              top: coords().top !== undefined ? `${coords().top}px` : "auto",
              bottom: coords().bottom !== undefined ? `${coords().bottom}px` : "auto",
              width: `${coords().width}px`,
            }
          : undefined
      }
    >
      <Show when={props.searchable}>
            <input
              ref={searchRef}
              class="sel-search"
              placeholder={`搜索${props.prefix}`}
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && filtered().length > 0) pick(filtered()[0].value);
                if (e.key === "Escape") setOpened(false);
              }}
            />
          </Show>

          {/* 三级面板：后端 | 厂商 | 模型 */}
          <Show when={isThreeLevel() && !query().trim()}>
            <div class="sel-panes sel-panes-3">
              <div class="sel-groups sel-backends">
                <For each={backends()}>
                  {(b) => (
                    <div
                      class={`sel-group-row ${shownBackend() === b.id ? "open" : ""} ${
                        currentBackend() === b.id ? "active" : ""
                      }`}
                      onMouseEnter={() => chooseBackend(b.id)}
                      onClick={() => chooseBackend(b.id)}
                    >
                      <span class="sel-label">{b.label}</span>
                      <IconChevron size={11} />
                    </div>
                  )}
                </For>
              </div>
              <Show when={!showingFavorites()}>
                <div class="sel-groups sel-providers">
                  <Show when={props.allowDefault && defaultOfBackend()}>
                    {(d) => (
                      <div
                        class={`sel-item ${props.value === d().value ? "active" : ""}`}
                        onClick={() => pick(d().value)}
                      >
                        <span class="sel-label">{defaultLabel()}</span>
                        <Show when={props.value === d().value}>
                          <IconCheck size={13} />
                        </Show>
                      </div>
                    )}
                  </Show>
                  <For each={providersOfBackend()}>
                    {(g) => (
                      <div
                        class={`sel-group-row ${shownProvider() === g.name ? "open" : ""} ${
                          currentGroup() === g.name && currentBackend() === shownBackend()
                            ? "active"
                            : ""
                        }`}
                        onMouseEnter={() => setActiveGroup(g.name)}
                        onClick={() => setActiveGroup(g.name)}
                      >
                        <span class="sel-label">{g.name}</span>
                        <span class="sel-count">{g.items.length}</span>
                        <IconChevron size={11} />
                      </div>
                    )}
                  </For>
                </div>
              </Show>
              <div class="sel-list sel-models">
                <For each={shownModels()}>{(o) => itemRow(o, showingFavorites())}</For>
              </div>
            </div>
          </Show>

          {/* 二级面板：左侧厂商、右侧该厂商的模型 */}
          <Show when={isGrouped() && !isThreeLevel() && !query().trim()}>
            <div class="sel-panes">
              <div class="sel-groups">
                <Show when={props.allowDefault}>
                  <div
                    class={`sel-item ${props.value === "" ? "active" : ""}`}
                    onClick={() => pick("")}
                  >
                    <span class="sel-label">{defaultLabel()}</span>
                    <Show when={props.value === ""}>
                      <IconCheck size={13} />
                    </Show>
                  </div>
                </Show>
                <For each={groups()}>
                  {(g) => (
                    <div
                      class={`sel-group-row ${shownGroup() === g.name ? "open" : ""} ${
                        currentGroup() === g.name ? "active" : ""
                      }`}
                      onMouseEnter={() => setActiveGroup(g.name)}
                      onClick={() => setActiveGroup(g.name)}
                    >
                      <span class="sel-label">{g.name}</span>
                      <span class="sel-count">{g.items.length}</span>
                      <IconChevron size={11} />
                    </div>
                  )}
                </For>
              </div>
              <div class="sel-list sel-models">
                <For each={shownItems()}>{(o) => itemRow(o)}</For>
              </div>
            </div>
          </Show>

          {/* 扁平模式：未分组的下拉，或分组/三级下拉的搜索结果 */}
          <Show when={(!isGrouped() && !isThreeLevel()) || query().trim()}>
            <div class="sel-list">
              <Show when={props.allowDefault && !query().trim()}>
                <div
                  class={`sel-item ${props.value === "" ? "active" : ""}`}
                  onClick={() => pick("")}
                >
                  <span class="sel-label">{defaultLabel()}</span>
                  <Show when={props.value === ""}>
                    <IconCheck size={13} />
                  </Show>
                </div>
              </Show>
              <For each={filtered()}>{(o) => itemRow(o, isThreeLevel())}</For>
              <Show when={filtered().length === 0}>
                <div class="sel-empty">无匹配项</div>
              </Show>
            </div>
          </Show>
    </div>
  );

  return (
    <div class={`sel ${props.wide ? "wide" : ""}`} ref={rootRef}>
      <button class="pill" onClick={toggle} title={props.title}>
        <span class="pill-text">
          {props.prefix}：{currentLabel()}
        </span>
        <IconChevron size={12} open={opened()} />
      </button>
      <Show when={opened()}>
        {usePortal() ? <Portal mount={document.body}>{renderPop()}</Portal> : renderPop()}
      </Show>
    </div>
  );
}
