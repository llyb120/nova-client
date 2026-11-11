import { createEffect, createSignal, For, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { TranscriptHeights, virtualWindow } from "../transcriptHeights";

/** 行高估算：折叠态的一行消息约这么高，实测后覆盖。 */
const ESTIMATE = 36;

/**
 * 变高虚拟滚动：只挂可见窗口内的行，其余高度用容器上下 padding 占位。
 * 滚动容器自动取最近的 overflow 祖先。未实测过的行按 ESTIMATE 撑开，滚到位后实测
 * 校正；估算偏差大时内容会轻微跳动。
 */
export default function VirtualRows<T>(props: {
  items: readonly T[];
  row: (item: T) => JSX.Element;
}) {
  let host!: HTMLDivElement;
  let scroller: HTMLElement | null = null;
  let observer: ResizeObserver | undefined;
  let frame = 0;
  const rows = new Map<Element, number>();
  const heights = new TranscriptHeights(0, ESTIMATE);
  const [range, setRange] = createSignal<readonly [number, number]>([0, 0]);
  const [padding, setPadding] = createSignal<readonly [number, number]>([0, 0]);

  /** 滚动容器视口顶边在 host 内容坐标里的位置（向上滚为负）与视口高度。 */
  function viewport() {
    const box = scroller ?? (document.scrollingElement as HTMLElement | null);
    return {
      first: (box?.getBoundingClientRect().top ?? 0) - host.getBoundingClientRect().top,
      height: box?.clientHeight || window.innerHeight,
    };
  }

  function sync() {
    frame = 0;
    heights.resize(props.items.length);
    if (!props.items.length) {
      setRange(previous => (previous[1] ? [0, 0] : previous));
      setPadding(previous => (previous[0] || previous[1] ? [0, 0] : previous));
      return;
    }
    const view = viewport();
    const [start, end] = virtualWindow(heights, view.first, view.height, props.items.length);
    setRange(previous => (previous[0] === start && previous[1] === end ? previous : [start, end]));
    // 下面占位的长度就是 total 减去窗口末行的偏移；不能夹到 padding-top 上，否则往下滚内容会一直变长。
    const top = heights.offset(start), bottom = heights.total - heights.offset(end);
    setPadding(previous => (previous[0] === top && previous[1] === bottom ? previous : [top, bottom]));
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(sync);
  }

  /** 只观察当前挂着的行，实测到的高度写回索引。 */
  function observe() {
    observer ??= new ResizeObserver(changes => {
      let changed = false;
      for (const change of changes) {
        const index = rows.get(change.target);
        if (index !== undefined && heights.set(index, change.borderBoxSize[0]?.blockSize ?? 0)) changed = true;
      }
      if (!changed) return;
      // 一屏里量到的高度和 ESTIMATE 差得远时（展开的长消息）按中位数重估没量过的行，
      // 否则滚动条长度和实际内容差一大截。
      const median = heights.sampleMedian(viewport().height * 4);
      if (median != null) heights.reestimate(median);
      sync();
    });
    observer.disconnect();
    rows.clear();
    const start = range()[0];
    Array.from(host.children).forEach((element, index) => {
      rows.set(element, start + index);
      observer!.observe(element, { box: "border-box" });
    });
  }

  onSettled(() => {
    for (let node = host.parentElement; node; node = node.parentElement) {
      const overflow = getComputedStyle(node).overflowY;
      if (overflow === "auto" || overflow === "scroll") { scroller = node; break; }
    }
    const box = scroller ?? document.scrollingElement ?? document.documentElement;
    box.addEventListener("scroll", schedule, { passive: true });
    const resize = new ResizeObserver(schedule);
    resize.observe(box);
    sync();
    // onSettled 里不能 onCleanup，只能返回清理函数。
    return () => {
      cancelAnimationFrame(frame);
      box.removeEventListener("scroll", schedule);
      resize.disconnect();
      observer?.disconnect();
    };
  });

  // 数据变了（新的一份上下文/记忆）就按新长度重算窗口。
  createEffect(() => props.items.length, () => sync());
  // 窗口变了就重新绑定实测。
  createEffect(() => range(), () => observe());

  return (
    <div
      ref={host}
      style={{ "padding-top": `${padding()[0]}px`, "padding-bottom": `${padding()[1]}px` }}
    >
      <For each={props.items.slice(range()[0], range()[1])}>{item => props.row(item)}</For>
    </div>
  );
}
