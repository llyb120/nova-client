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
