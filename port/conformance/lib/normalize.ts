// Equality rules for comparing observations across runs and implementations. Every rule here is a statement
// that the difference it hides is not observable behavior; keep the list short and explicit.

// Generated IDs (ses_…, msg_…), including forked copies that carry a sequence suffix (msg_…_3).
const ID = /\b([a-z]{2,8})_([0-9A-Za-z]{16,40})(_\d+)?\b/g
const UUID = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi
const HASH = /\b(?:[0-9a-f]{64}|[0-9a-f]{40})\b/g
const B64_JSON = /^[A-Za-z0-9_-]{16,}={0,2}$/
// Paging cursors inside URLs (?cursor=…).
const URL_CURSOR = /([?&]cursor=)([A-Za-z0-9_-]{16,}={0,2})/g
// Today's date as the system prompt prints it ("Today's date: Sat Nov 07 2026"). The format must match exactly.
export const DATE = /\b(Mon|Tue|Wed|Thu|Fri|Sat|Sun) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{2} \d{4}\b/g
// Operating-system process IDs.
export const PID_KEY = /^pid$/
const HOSTNAME_KEY = /^hostname$/
const ISO = /\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z\b/g
const TIME_KEY = /^(time|created|updated|completed|started|ended|start|end|timestamp|at|lastUsed|modified|mtime|expires(At)?|.*At)$/i
const DURATION_KEY = /(duration|elapsed|latency)(Ms)?$/i

export interface Normalizer {
  readonly value: (input: unknown) => unknown
}

/** One normalizer per scenario run, so IDs map to stable placeholders by order of first appearance. */
export function normalizer(paths: Record<string, string>): Normalizer {
  const ids = new Map<string, string>()
  const counters = new Map<string, number>()
  const placeholder = (kind: string, raw: string) => {
    const existing = ids.get(raw)
    if (existing) return existing
    const next = (counters.get(kind) ?? 0) + 1
    counters.set(kind, next)
    const name = `<${kind}_${next}>`
    ids.set(raw, name)
    return name
  }
  // Longest paths first so nested roots are replaced before their parents.
  const roots = Object.entries(paths)
    .filter(([, root]) => root.length > 0)
    .sort((a, b) => b[1].length - a[1].length)
  const decode = (input: string): unknown => {
    try {
      const decoded = JSON.parse(Buffer.from(input, "base64url").toString("utf8"))
      return decoded && typeof decoded === "object" ? decoded : undefined
    } catch {
      return undefined
    }
  }
  const string = (input: string): string => {
    let out = input
    for (const [name, root] of roots) out = out.split(root).join(`<${name}>`)
    out = out.replace(URL_CURSOR, (raw, prefix: string, encoded: string) => {
      const decoded = decode(encoded)
      return decoded ? `${prefix}<base64-json:${JSON.stringify(value(decoded))}>` : raw
    })
    out = out.replace(ID, (raw, prefix: string) => placeholder(prefix, raw))
    out = out.replace(UUID, (raw) => placeholder("uuid", raw.toLowerCase()))
    out = out.replace(HASH, (raw) => placeholder("hash", raw))
    out = out.replace(ISO, "<iso-time>")
    out = out.replace(DATE, "<date>")
    return out
  }
  // Paging cursors are base64url JSON that embeds IDs: compare their decoded, normalized content.
  const cursor = (input: string): unknown => {
    if (!B64_JSON.test(input)) return undefined
    const decoded = decode(input)
    return decoded ? { "<base64-json>": value(decoded) } : undefined
  }
  const value = (input: unknown, key = "", parent?: Record<string, unknown>): unknown => {
    if (typeof input === "string") {
      // Session slugs ("gentle-falcon") are random: each session's slug is named after the session, so
      // consistency is still checked but two sessions drawing the same words don't collide.
      if (key === "slug") {
        const owner = typeof parent?.id === "string" ? parent.id : undefined
        return owner?.startsWith("ses_") ? `<slug-of-${string(owner).slice(1, -1)}>` : "<slug>"
      }
      if (HOSTNAME_KEY.test(key)) return "<hostname>"
      return cursor(input) ?? string(input)
    }
    if (typeof input === "number") {
      // Epoch milliseconds between 2020 and 2040 are wall-clock times wherever they appear.
      if ((TIME_KEY.test(key) && input > 1e12) || (Number.isInteger(input) && input > 1.58e12 && input < 2.21e12))
        return "<time>"
      if (DURATION_KEY.test(key)) return "<duration>"
      if (PID_KEY.test(key)) return "<pid>"
      return input
    }
    if (Array.isArray(input)) return input.map((item) => value(item, key))
    if (input && typeof input === "object") {
      const record = input as Record<string, unknown>
      const out: Record<string, unknown> = {}
      for (const k of Object.keys(record).sort()) out[string(k)] = value(record[k], k, record)
      return out
    }
    return input
  }
  return { value: (input) => value(input) }
}
