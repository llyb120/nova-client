/**
 * 用户手动滚动后的吸底判定（HtmlTranscript 与 ChatView 共用）：上滚（位置变小）立即
 * 解除吸底；下滚贴底 2px 内才恢复。
 *
 * 纯位置阈值（maxScroll - top <= 2）会把触控板/高精度滚轮慢扫的 1-2px 上滚误判为
 * 仍贴底，流式钉底随即把位置钉回新底部吞掉滚动量，表现为手动滚动偶发无法解除吸底。
 */
export function resolveUserScrollStick(prevTop: number, nextTop: number, maxScroll: number): boolean {
  return nextTop < prevTop ? false : maxScroll - nextTop <= 2;
}
