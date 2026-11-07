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

// Diagnostics written by the JavaScript engine that opencode passes through inside its own messages: JSON.parse,
// URI and regular-expression syntax errors (JavaScriptCore's wording, e.g. "JSON Parse error: Expected '}'").
// The standard specifies opencode's message around them, not the engine's wording: a Rust implementation
// brings its own parsers. Each rule is anchored on opencode's text, so whatever an implementation writes there
// masks the same way. A diagnostic must be a single line.
const ENGINE_TAIL = /JSON\.parse received invalid JSON: |received malformed URI data: |Invalid regular expression (?=\\*")/g
const ENGINE_BETWEEN = [
  /(, which is not a valid regular expression pattern \()(.*?)(\)\. To match special characters)/g,
  /( received invalid flags \S+ \()(.*?)(\)\. Valid flags are)/g,
]
export const ENGINE_MESSAGE = "<engine message>"

/** Masks engine diagnostics; see ENGINE_TAIL. */
export function engine(input: string): string {
  let out = input
  for (const rule of ENGINE_BETWEEN) out = out.replace(rule, (_, before: string, _reason: string, after: string) => before + ENGINE_MESSAGE + after)
  // A tail runs to the end of the message that contains it: the quote that closes the enclosing string literal
  // (as escaped as the quote that opened it, when the message sits inside JSON text), a newline, or the end.
  const quoteLevel = (at: number): number => {
    const quote = out.lastIndexOf('"', at - 1)
    if (quote < 0) return -1
    let slashes = 0
    while (quote - slashes - 1 >= 0 && out[quote - slashes - 1] === "\\") slashes++
    return slashes
  }
  const closes = (at: number, level: number) => {
    if (out[at] === "\n") return true
    if (level < 0 || out[at] !== '"') return false
    let slashes = 0
    while (at - slashes - 1 >= 0 && out[at - slashes - 1] === "\\") slashes++
    return slashes === level
  }
  let from = 0
  for (;;) {
    ENGINE_TAIL.lastIndex = from
    const match = ENGINE_TAIL.exec(out)
    if (!match) break
    const level = quoteLevel(match.index)
    let start = match.index + match[0].length
    if (match[0].startsWith("Invalid regular expression")) {
      // opencode quotes the pattern (JSON.stringify), then ": " and the engine's diagnostic.
      let slashes = 0
      while (out[start + slashes] === "\\") slashes++
      let end = start + slashes + 1
      while (end < out.length && !(out[end] === '"' && out.slice(end - slashes, end) === "\\".repeat(slashes) && out[end - slashes - 1] !== "\\" && out.startsWith(": ", end + 1))) end++
      if (end >= out.length) {
        from = start
        continue
      }
      start = end + 3
    }
    let end = start
    while (end < out.length && !closes(end, level)) end++
    out = out.slice(0, start) + ENGINE_MESSAGE + out.slice(end)
    from = start + ENGINE_MESSAGE.length
  }
  return out
}

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
    return engine(out)
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
