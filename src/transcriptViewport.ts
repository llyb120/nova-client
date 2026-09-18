/** 二分定位轮次，offsets 可以包含文档末尾哨兵。 */
export function groupAtY(offsets: readonly number[], y: number): number {
  let low = 0, high = offsets.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (offsets[middle] <= y) low = middle + 1;
    else high = middle;
  }
  return Math.max(0, low - 1);
}

/** 前后各一屏缓冲；只按高度查询，不读取历史正文。end 不包含在范围内。 */
export function visibleGroupRange(offsets: readonly number[], top: number, height: number) {
  const count = Math.max(0, offsets.length - 1);
  return {
    start: Math.min(count, groupAtY(offsets, Math.max(0, top - height))),
    end: Math.min(count, groupAtY(offsets, top + height * 2) + 1),
  };
}
