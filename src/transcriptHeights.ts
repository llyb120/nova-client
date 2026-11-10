/** Fenwick heights in content coordinates (excluding transcript top padding).
 * Finite heights are clamped to >= 1; non-finite measurements are ignored.
 * Invalid item indices read as 0 / cannot be set; offsets clamp to [0, length].
 */
export class TranscriptHeights {
  private heights: number[] = [];
  private tree: number[] = [0];
  /** 已经实测过的行：重估估算行高时不能覆盖它们。 */
  private measured = new Set<number>();
  private estimate: number;

  constructor(count = 0, estimate = 240) {
    this.estimate = Number.isFinite(estimate) ? Math.max(1, estimate) : 240;
    this.reset(count);
  }

  /** Discard measurements, e.g. after a session switch. O(count).
   *  estimate 默认沿用当前估算；切会话时显式传 240，不把上个会话的估算带进新会话。
   */
  reset(count: number, estimate = this.estimate): void {
    if (Number.isFinite(estimate)) this.estimate = Math.max(1, estimate);
    this.resize(count);
    this.heights.fill(this.estimate);
    this.measured.clear();
    for (let i = 1; i <= count; i++) this.tree[i] = (i & -i) * this.estimate;
  }

  resize(count: number): void {
    // ponytail: bitwise indices cap count at 2^31-1; use arithmetic lowbits beyond that.
    if (!Number.isInteger(count) || count < 0 || count > 0x7fffffff) {
      throw new RangeError("count must be an integer in [0, 2^31-1]");
    }
    if (count < this.length) {
      this.heights.length = count;
      this.tree.length = count + 1;
      for (const index of this.measured) if (index >= count) this.measured.delete(index);
    }
    while (this.length < count) {
      const i = this.length + 1;
      const start = i - (i & -i);
      let sum = this.estimate;
      // Merge only the new node's children: O(log n) worst, O(1) amortized per append.
      for (let j = i - 1; j > start; j -= j & -j) sum += this.tree[j];
      this.heights.push(this.estimate);
      this.tree.push(sum);
    }
  }

  set(index: number, height: number): boolean {
    const previous = this.get(index);
    if (previous === 0 || !Number.isFinite(height)) return false;
    this.measured.add(index);
    height = Math.max(1, height);
    if (height === previous) return false;
    this.heights[index] = height;
    const delta = height - previous;
    for (let i = index + 1; i <= this.length; i += i & -i) this.tree[i] += delta;
    return true;
  }

  /** Re-estimate the rows that were never measured, e.g. right after the first window of a session
   *  switch is measured: the scrollable total (and the scrollbar) is then right on the first frame
   *  instead of sitting on the default estimate until those rows happen to mount. Returns whether
   *  anything changed. O(count) — a per-row Fenwick update would cost O(count log count).
   */
  reestimate(estimate: number): boolean {
    if (!Number.isFinite(estimate)) return false;
    this.estimate = Math.max(1, estimate);
    let changed = false;
    for (let index = 0; index < this.heights.length; index++) {
      if (this.measured.has(index) || this.heights[index] === this.estimate) continue;
      this.heights[index] = this.estimate;
      changed = true;
    }
    if (!changed) return false;
    this.tree.length = this.heights.length + 1;
    for (let i = 1; i <= this.heights.length; i++) {
      const start = i - (i & -i);
      let sum = this.heights[i - 1];
      for (let j = i - 1; j > start; j -= j & -j) sum += this.tree[j];
      this.tree[i] = sum;
    }
    return true;
  }

  get(index: number): number {
    return Number.isInteger(index) ? this.heights[index] ?? 0 : 0;
  }

  /** Sum of the first index items; fractional indices round down, NaN maps to 0. */
  offset(index: number): number {
    if (!(index > 0)) return 0;
    let sum = 0;
    for (let i = Math.min(Math.floor(index), this.length); i > 0; i -= i & -i) {
      sum += this.tree[i];
    }
    return sum;
  }

  /** Half-open item intervals: an exact boundary belongs to the following item. */
  at(y: number): number {
    if (this.length === 0 || !(y > 0)) return 0;
    let index = 0;
    let sum = 0;
    for (let step = 2 ** Math.floor(Math.log2(this.length)); step >= 1; step /= 2) {
      const next = index + step;
      if (next <= this.length && sum + this.tree[next] <= y) {
        sum += this.tree[next];
        index = next;
      }
    }
    return Math.min(index, this.length - 1);
  }

  get total(): number {
    return this.offset(this.length);
  }

  get length(): number {
    return this.heights.length;
  }
}
