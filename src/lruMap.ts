export class LruMap<K, V> {
  private readonly values = new Map<K, V>();
  private readonly limit: number;

  constructor(limit: number) {
    this.limit = limit;
  }

  get(key: K): V | undefined {
    const value = this.values.get(key);
    if (value === undefined) return undefined;
    this.values.delete(key);
    this.values.set(key, value);
    return value;
  }

  peek(key: K): V | undefined {
    return this.values.get(key);
  }

  set(key: K, value: V, protectedKey?: K): K[] {
    this.values.delete(key);
    this.values.set(key, value);
    const evicted: K[] = [];
    while (this.values.size > this.limit) {
      const oldest = this.values.keys().next().value as K | undefined;
      if (oldest === undefined) break;
      if (oldest === protectedKey) {
        const protectedValue = this.values.get(oldest)!;
        this.values.delete(oldest);
        this.values.set(oldest, protectedValue);
        continue;
      }
      this.values.delete(oldest);
      evicted.push(oldest);
    }
    return evicted;
  }

  has(key: K): boolean {
    return this.values.has(key);
  }

  delete(key: K): boolean {
    return this.values.delete(key);
  }

  keys(): IterableIterator<K> {
    return this.values.keys();
  }
}
