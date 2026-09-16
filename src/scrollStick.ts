/**
 * rebuild 提交阶段的吸底判定与滚动位置收敛。
 *
 * computeLayout 分块布局会让出多个 rAF；await 期间滚轮/拖拽/钉底都会实时改写
 * scrollY 与 keepBottom。因此吸底判定必须取布局提交时的实时状态：若用 await 前的
 * 快照，切换会话后的冷布局窗口内用户滚离底部会被旧快照拽回（滚动条看似钉死底部）。
 */
export function resolveScrollAfterLayout(opts: {
  /** 布局提交时的实时吸底标记。 */
  keepBottom: boolean;
  /** 布局提交时的实时滚动位置（await 期间可能已被用户改写）。 */
  scrollY: number;
  /** 布局前的最大滚动量，用于判断实时位置是否仍贴着旧底部。 */
  maxScrollBefore: number;
  /** 本次布局产出的内容总高与视口高。 */
  totalHeight: number;
  viewH: number;
}): { scrollY: number; maxScroll: number } {
  const maxScroll = Math.max(0, opts.totalHeight - opts.viewH);
  const stick = opts.keepBottom || opts.maxScrollBefore - opts.scrollY <= 2;
  return {
    maxScroll,
    scrollY: stick ? maxScroll : Math.max(0, Math.min(maxScroll, opts.scrollY)),
  };
}

/**
 * 吸底时点开「展开」后的滚动落点。
 *
 * 展开把新内容插在头行下方：若头行仍在新布局的最后一屏内，钉住新底部——
 * 展开内容全部进入视野、滚动条留在底部；展开量把头行顶出屏外时退回头行
 * 锚定，避免头行飞出视口。收起不走此函数：直接锚定头行即可。
 * DOM 版（ChatView.handleTranscriptClick）的锚定等于 scrollTop 不动，
 * 只需同一条件 docTop >= maxScrollTop 判定是否钉底。
 */
export function resolveExpandScroll(opts: {
  /** 头行在新布局中的文档位置；元素已被移除时传 undefined 走兜底。 */
  headerTop: number | undefined;
  /** 点击瞬间头行距视口顶部的偏移。 */
  viewOffset: number;
  /** 布局提交前的滚动位置：头行缺失时 +viewOffset 反推出原位置。 */
  scrollBefore: number;
  /** 本次布局产出的最大滚动量。 */
  maxScroll: number;
  /** 是否允许钉底（点击时处于吸底且本次为展开）。 */
  pin: boolean;
}): number {
  const anchorTop = opts.headerTop ?? opts.scrollBefore + opts.viewOffset;
  const pin =
    opts.pin && opts.headerTop !== undefined && opts.headerTop >= opts.maxScroll;
  return Math.max(
    0,
    Math.min(opts.maxScroll, pin ? opts.maxScroll : anchorTop - opts.viewOffset),
  );
}

/**
 * 用户手动滚动后的吸底判定（canvas 与 ChatView 共用，语义对齐 DOM 版 wheel：
 * 上滚 cancelBottomFollow、下滚到底 enableBottomFollow）：上滚（位置变小）立即
 * 解除吸底；下滚贴底 2px 内才恢复。
 *
 * 纯位置阈值（maxScroll - top <= 2）会把触控板/高精度滚轮慢扫的 1-2px 上滚误判为
 * 仍贴底，流式 rebuild/pin 随即把 scrollY 钉回新底部吞掉滚动量，表现为手动滚动
 * 偶发无法解除吸底（快速滚动单帧 delta 超阈值，所以只是偶发）。
 */
export function resolveUserScrollStick(prevTop: number, nextTop: number, maxScroll: number): boolean {
  return nextTop < prevTop ? false : maxScroll - nextTop <= 2;
}
