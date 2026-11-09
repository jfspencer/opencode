// A seeded random source (mulberry32) for the case generators.
export class Random {
  private state: number
  constructor(seed: number) {
    this.state = seed >>> 0
  }
  next() {
    this.state = (this.state + 0x6d2b79f5) >>> 0
    let t = this.state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(n: number) {
    return Math.floor(this.next() * n)
  }
  bool(p = 0.5) {
    return this.next() < p
  }
  pick<T>(items: readonly T[]): T {
    return items[this.int(items.length)]
  }
}
