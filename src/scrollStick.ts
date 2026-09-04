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
