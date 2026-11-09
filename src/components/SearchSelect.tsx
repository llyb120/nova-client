import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "@solidjs/web";
import { modelFavoriteIds, toggleModelFavorite } from "../store";
import { IconCheck, IconChevron, IconChevronLeft, IconEye, IconStar } from "./icons";

const FAVORITE_GROUP = "收藏";

export interface SelectOption {
  value: string;
  label: string;
  /** 触发器文案；模型列表保留短名，默认档位在这里显示实际强度。 */
  selectedLabel?: string;
  title?: string;
  /** 分组名（模型名）；任一选项带分组时整个下拉切换为二级面板 */
  group?: string;
  /** 该项是「默认」（跟随 agent 默认模型）入口，分组面板里置顶直接可选 */
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
  /** 思考强度档位：第二屏直接按「模型 · 强度」展开成独立行，不再另设强度按钮条 */
  efforts?: SelectOption[];
}

/** 可搜索的下拉选择器（浮层向上弹出，适合放在 composer 工具条）。
 *  支持两种形态：扁平 / 二级（模型名 → 模型）；
 *  选项带 efforts 时在第二屏按模型 × 强度展开成独立行（「模型 · High」）。 */
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
  /** 禁用触发器（运行中禁止手动切换模型） */
  disabled?: boolean;
}) {
  const [opened, setOpened] = createSignal(false);
  const [query, setQuery] = createSignal("");
  /** 二级面板当前进入的模型名（展示它的完整模型）；null 表示停在模型名列表 */
  const [drill, setDrill] = createSignal<string | null>(null);
  /** 键盘光标在可见模型列里的位置；-1 表示尚未用键盘移动 */
  const [cursor, setCursor] = createSignal(-1);
  /** 弹层方向：空间不足时向下翻转 / 右对齐，避免溢出窗口 */
  const [place, setPlace] = createSignal({ down: false, right: false });
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

  /** 是否启用分组二级面板（搜索时退化为扁平列表） */
  const isGrouped = createMemo(() => props.options.some((o) => o.group));

  /** 含下一级强度项的全部可选值 */
  const flatOptions = createMemo(() => props.options.flatMap((o) => [o, ...(o.efforts ?? [])]));
  const currentOption = createMemo(() => flatOptions().find((o) => o.value === props.value));
  /** 该行被选中（第二屏的行已按强度拆开，逐行精确比较） */
  const isActive = (o: SelectOption) => o.value === activeValue();

  /** 高亮/选中的有效值：命中当前值则用它；值非空但未命中列表时保持原值（不高亮任何项），
   *  避免把列表第一项误标为已选；空值才允许「默认」或回退到第一项。 */
  const activeValue = createMemo(() => {
    if (currentOption()) return props.value;
    if (props.value) return props.value;
    if (props.allowDefault) return "";
    return firstSelectable()?.value ?? props.value;
  });

  const currentLabel = createMemo(() => {
    const hit = currentOption() ?? (!props.value && !props.allowDefault ? firstSelectable() : undefined);
    if (hit) return hit.selectedLabel ?? hit.label;
    // 值非空但未命中列表（如云端清单未就绪时的中间态）：用友好名/原值显示，
    // 不回退到第一项，否则触发器会显示成别的模型（显示与实际选择不一致）。
    if (props.value) return props.fallbackLabel ?? props.value;
    return defaultLabel();
  });

  const filtered = createMemo(() => props.options.filter((o) => !o.isDefault));
  // 旧版收藏的是带强度的项，父项任一强度被收藏也算收藏
  const favoriteIdsOf = (o: SelectOption) =>
    [o, ...(o.efforts ?? [])].flatMap((x) => (x.favoriteId ? [x.favoriteId] : []));
  const isFavorite = (o: SelectOption) =>
    favoriteIdsOf(o).some((id) => modelFavoriteIds().includes(id));
  const toggleFavorite = (o: SelectOption) => {
    if (!o.favoriteId) return;
    const marked = favoriteIdsOf(o).filter((id) => modelFavoriteIds().includes(id));
    for (const id of marked.length ? marked : [o.favoriteId]) toggleModelFavorite(id);
  };

  // ===== 二级（模型名 → 供应商+模型全名）=====
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
  /** 第一屏可见的模型名：搜索只过滤这一屏，点进去仍是同一套第二屏 */
  const shownGroups = createMemo(() => {
    const q = query().trim().toLowerCase();
    return q ? groups().filter((g) => g.name.toLowerCase().includes(q)) : groups();
  });
  /** 进入的模型名下的完整模型（供应商 + 全名） */
  const drilledItems = createMemo(
    () => groups().find((g) => g.name === drill())?.items ?? [],
  );

  /** 第二屏的行：不另设强度按钮条，直接按模型 × 强度展开成独立行（「模型 · High」），
   *  点哪一行就是哪一档；parent 让各行沿用该模型的收藏状态。 */
  const drilledRows = createMemo(() =>
    drilledItems().flatMap((o) =>
      o.efforts?.length
        ? o.efforts.map((effort) => ({ option: effort, parent: o }))
        : [{ option: o, parent: o }],
    ),
  );
  /** 当前可点的模型行：只在第二屏（搜索时第一屏的行是模型名，不是模型） */
  const visibleModels = createMemo(() => (drill() ? drilledRows().map((r) => r.option) : []));

  /** 键盘光标所在项；-1 时为 undefined，不影响鼠标路径 */
  const cursorValue = createMemo(() => visibleModels()[cursor()]?.value);
  /** 键盘上下移动：只走当前可见的模型列 */
  const moveCursor = (step: number) => {
    const items = visibleModels();
    if (!items.length) return;
    const next =
      cursor() < 0
        ? step > 0 ? 0 : items.length - 1
        : (cursor() + step + items.length) % items.length;
    setCursor(next);
    queueMicrotask(() =>
      modelsRef?.querySelector(".sel-item.cursor")?.scrollIntoView({ block: "nearest" }),
    );
  };

  let modelsRef: HTMLDivElement | undefined;
  // 换了搜索词或进入的模型名，可见列表从头开始
  createEffect(() => [opened(), query(), drill()], () => {
    setCursor(-1);
  });
  // 搜索只作用于第一屏：开始输入就退回模型名列表
  createEffect(() => query(), (q) => {
    if (q.trim() && drill()) setDrill(null);
  });

  const pick = (v: string) => {
    props.onChange(
      v,
      flatOptions().find((o) => o.value === v),
    );
    setOpened(false);
    setQuery("");
  };

  // 宽度恒定：面板不随进入第二屏而跳变（矩阵展开只是多出行，不改列宽）
  const popWidth = () => (isGrouped() ? 480 : 280);

  /** anchorTo 隐含 portal：对齐容器的浮层必须脱离 overflow 祖先才能不被裁剪 */
  const usePortal = () => props.portal || !!props.anchorTo;

  const computePlacement = () => {
    if (!rootRef) return;
    const r = rootRef.getBoundingClientRect();
    const width = popWidth();
    const big = isGrouped();
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
    // 以模型选择器为基点水平居中（面板中点对准按钮中点），再按视口夹取；垂直始终贴触发器
    const down = r.top < height + 8 && window.innerHeight - r.bottom > r.top;
    const right = r.left + width > window.innerWidth - 8;
    setPlace({ down, right });
    if (!usePortal()) return;
    let w = Math.min(width, window.innerWidth - 16);
    // 普通下拉宽度不超出容器；分组面板需要列宽，允许超出，由视口夹取兜底
    if (anchor && !big) w = Math.min(w, anchor.width);
    const left = Math.max(
      8,
      Math.min((r.left + r.right) / 2 - w / 2, window.innerWidth - w - 8),
    );
    if (down) {
      // 下方空间不足时整体上移，保证浮层完整可见
      const top = Math.max(8, Math.min(r.bottom + 8, window.innerHeight - height - 8));
      setCoords({ left, top, width: w });
    } else {
      const bottom = Math.max(
        8,
        Math.min(window.innerHeight - r.top + 8, window.innerHeight - height - 8),
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
      setDrill(null);
      if (props.searchable) queueMicrotask(() => searchRef?.focus());
    }
  };

  // portal 模式：浮层脱离 overflow 容器用 fixed 定位，需随滚动/缩放实时重算坐标
  createEffect(() => {
    if (!usePortal() || !opened()) return false;
    void props.options.length;
    return true;
  }, active => {
    if (!active) return;
    const onReflow = () => computePlacement();
    window.addEventListener("scroll", onReflow, true);
    window.addEventListener("resize", onReflow);
    return () => {
      window.removeEventListener("scroll", onReflow, true);
      window.removeEventListener("resize", onReflow);
    };
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

  const itemRow = (o: SelectOption, parent: SelectOption = o) => (
    <div
      class={`sel-item ${isActive(o) ? "active" : ""} ${
        cursorValue() === o.value ? "cursor" : ""
      }`}
      onClick={() => pick(o.value)}
      title={o.title ?? o.value}
    >
      <span class="sel-model-copy">
        <span class="sel-label">{o.label}</span>
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
      <Show when={props.favorites && parent.favoriteId}>
        <button
          type="button"
          class={`sel-favorite ${isFavorite(parent) ? "active" : ""}`}
          title={isFavorite(parent) ? "取消收藏" : "收藏模型"}
          onClick={(event) => {
            event.stopPropagation();
            toggleFavorite(parent);
          }}
        >
          <IconStar size={14} filled={isFavorite(parent)} />
        </button>
      </Show>
      <Show when={isActive(o)}>
        <IconCheck size={13} />
      </Show>
    </div>
  );

  const renderPop = () => (
    <div
      ref={popRef}
      class={`sel-pop ${isGrouped() ? "grouped" : ""} ${
        !usePortal() && place().down ? "down" : ""
      } ${!usePortal() && place().right ? "right" : ""} ${usePortal() ? "portal" : ""}`}
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
                if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                  e.preventDefault();
                  moveCursor(e.key === "ArrowDown" ? 1 : -1);
                  return;
                }
                if (e.key === "Enter") {
                  // 第二屏：选光标所在模型；第一屏：进入第一个匹配的模型名（搜索只筛第一屏）
                  const hit = visibleModels()[cursor()];
                  if (hit) pick(hit.value);
                  else if (!drill() && shownGroups().length > 0) setDrill(shownGroups()[0].name);
                  return;
                }
                if (e.key === "Escape") {
                  // 第二屏 → 第一屏 → 清搜索 → 关闭
                  if (drill()) setDrill(null);
                  else if (query().trim()) setQuery("");
                  else setOpened(false);
                }
              }}
            />
          </Show>

          {/* 二级面板：模型名 → 点进去换成该模型的完整模型（同一栏，可返回）；
              有思考强度时按「模型 · 强度」展开成行，点一行即选定。 */}
          <Show when={isGrouped()}>
            <div ref={modelsRef} class="sel-list sel-models">
              <Show
                when={drill()}
                fallback={
                  <>
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
                    <For each={shownGroups()}>
                      {(g) => (
                        <div
                          class={`sel-group-row ${currentGroup() === g.name ? "active" : ""}`}
                          onClick={() => setDrill(g.name)}
                        >
                          <span class="sel-label">{g.name}</span>
                          <span class="sel-count">{g.items.length}</span>
                          <IconChevron size={11} />
                        </div>
                      )}
                    </For>
                    <Show when={shownGroups().length === 0}>
                      <div class="sel-empty">无匹配项</div>
                    </Show>
                  </>
                }
              >
                <div class="sel-group-row sel-back" onClick={() => setDrill(null)}>
                  <IconChevronLeft size={11} />
                  <span class="sel-label">{drill()}</span>
                </div>
                <For each={drilledRows()}>{(r) => itemRow(r.option, r.parent)}</For>
              </Show>
            </div>
          </Show>

          {/* 扁平模式：未分组的下拉（分支机构等，没有第二屏） */}
          <Show when={!isGrouped()}>
            <div class="sel-list">
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
              <For each={filtered()}>{(o) => itemRow(o)}</For>
              <Show when={filtered().length === 0}>
                <div class="sel-empty">无匹配项</div>
              </Show>
            </div>
          </Show>
    </div>
  );

  return (
    <div class={`sel ${props.wide ? "wide" : ""}`} ref={rootRef}>
      <button class="pill" onClick={toggle} title={props.title} disabled={props.disabled}>
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
