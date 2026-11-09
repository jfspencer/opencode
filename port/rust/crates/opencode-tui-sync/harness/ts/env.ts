// The driver's deterministic JS environment: a virtual clock behind Date.now/setTimeout, a seeded
// crypto.getRandomValues, and a scripted fetch whose requests the case answers step by step.
//
// Installed before the reference modules run, so everything they schedule goes through it.

export const BASE_TIME = 1_700_000_000_000

type Timer = { id: number; at: number; seq: number; fn: () => void }
let now = BASE_TIME
let seq = 0
let nextID = 1
const timers = new Map<number, Timer>()

class FakeTimeout {
  constructor(readonly id: number) {}
  unref() {
    return this
  }
  ref() {
    return this
  }
  hasRef() {
    return true
  }
  [Symbol.toPrimitive]() {
    return this.id
  }
}

const realSetImmediate = globalThis.setImmediate

export function install() {
  Date.now = () => now
  globalThis.setTimeout = ((fn: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => {
    const id = nextID++
    const ms = typeof delay === "number" && delay > 0 ? delay : 0
    timers.set(id, { id, at: now + Math.floor(ms), seq: seq++, fn: () => fn(...args) })
    return new FakeTimeout(id)
  }) as unknown as typeof setTimeout
  globalThis.clearTimeout = ((timer: unknown) => {
    if (timer === undefined || timer === null) return
    const id = timer instanceof FakeTimeout ? timer.id : Number(timer)
    timers.delete(id)
  }) as typeof clearTimeout
  randomState = 0x9e3779b9
  crypto.getRandomValues = (<T extends ArrayBufferView | null>(array: T): T => {
    if (array) {
      const bytes = new Uint8Array(array.buffer, array.byteOffset, array.byteLength)
      for (let i = 0; i < bytes.length; i++) {
        randomState ^= randomState << 13
        randomState ^= randomState >>> 17
        randomState ^= randomState << 5
        randomState >>>= 0
        bytes[i] = randomState & 0xff
      }
    }
    return array
  }) as typeof crypto.getRandomValues
}

let randomState = 0x9e3779b9

export function reset() {
  randomState = 0x9e3779b9
  now = BASE_TIME
  timers.clear()
  seq = 0
}

export function clock() {
  return now
}

/** Lets every queued microtask (and promise chain) run. */
export async function drain() {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => realSetImmediate(resolve))
}

/** Runs the timers due within `ms`, in deadline order, draining after each. */
export async function advance(ms: number) {
  const target = now + ms
  while (true) {
    const due = [...timers.values()].filter((t) => t.at <= target).sort((a, b) => a.at - b.at || a.seq - b.seq)[0]
    if (!due) break
    timers.delete(due.id)
    now = due.at
    due.fn()
    await drain()
  }
  now = target
  await drain()
}

// ---- scripted fetch ----------------------------------------------------------------------------------------

export type Recorded = {
  method: string
  url: string
  headers: [string, string][]
  body: string | null
}

type Pending = {
  recorded: Recorded
  resolve: (response: Response) => void
  reject: (error: unknown) => void
  settled: boolean
  controller?: ReadableStreamDefaultController<Uint8Array>
  closed: boolean
  // The body's chunks not yet pulled, then how it ends ("done", or an error).
  chunks: Uint8Array[]
  ending?: { error?: unknown }
  wake?: () => void
  // The reader received the end (or error), or the body was cancelled or aborted.
  finished?: boolean
}

export const requests: Pending[] = []

export function resetRequests() {
  requests.length = 0
}

export const fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const signal = init?.signal ?? undefined
  if (signal?.aborted) throw signal.reason
  const headers = [...new Headers(init?.headers)].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
  const body = init?.body === undefined || init?.body === null ? null : String(init.body)
  const pending = await new Promise<Response>((resolve, reject) => {
    const entry: Pending = {
      recorded: { method: init?.method ?? "GET", url: String(input), headers, body },
      resolve,
      reject,
      settled: false,
      closed: false,
      chunks: [],
    }
    requests.push(entry)
    signal?.addEventListener(
      "abort",
      () => {
        if (!entry.settled) {
          entry.closed = true
          entry.settled = true
          reject(signal.reason)
          return
        }
        if (entry.finished) return
        entry.closed = entry.chunks.length > 0 || !entry.ending
        entry.finished = true
        try {
          entry.controller?.error(signal.reason)
        } catch {}
      },
      { once: true },
    )
  })
  return pending
}) as typeof globalThis.fetch

export type ResponseSpec = {
  status: number
  headers: [string, string][]
  chunks: string[]
  /** "done" ends the body after the chunks, "open" keeps it open, "error" fails it. */
  end?: "done" | "open" | "error"
  error?: { name: string; message: string }
}

export function chunk(text: string): Uint8Array {
  if (text.startsWith("hex:")) return Uint8Array.from(text.slice(4).match(/../g) ?? [], (h) => parseInt(h, 16))
  return new TextEncoder().encode(text)
}

export function errorValue(error: { name: string; message: string }) {
  const value = new Error(error.message)
  value.name = error.name
  return value
}

// A pull source: each read takes the next chunk; the end (or error) comes after the last chunk was read.
export function respond(index: number, spec: ResponseSpec) {
  const entry = requests[index]
  if (!entry || entry.settled) throw new Error(`request ${index} is not pending`)
  entry.settled = true
  entry.chunks.push(...(spec.chunks ?? []).map(chunk))
  if ((spec.end ?? "done") === "done") entry.ending = {}
  if (spec.end === "error") entry.ending = { error: errorValue(spec.error ?? { name: "TypeError", message: "failed" }) }
  const stream = new ReadableStream<Uint8Array>(
    {
      start(controller) {
        entry.controller = controller
      },
      pull(controller) {
        return new Promise<void>((resolve) => {
          const serve = () => {
            if (entry.finished) return resolve()
            const next = entry.chunks.shift()
            if (next) {
              controller.enqueue(next)
              return resolve()
            }
            if (entry.ending) {
              entry.finished = true
              try {
                if (entry.ending.error) controller.error(entry.ending.error)
                else controller.close()
              } catch {}
              return resolve()
            }
            entry.wake = () => {
              entry.wake = undefined
              serve()
            }
          }
          serve()
        })
      },
      cancel() {
        if (entry.finished) return
        entry.finished = true
        entry.closed = entry.chunks.length > 0 || !entry.ending
      },
    },
    { highWaterMark: 0 },
  )
  entry.resolve(new Response(stream, { status: spec.status, headers: spec.headers }))
}

export function push(index: number, text: string) {
  const entry = requests[index]
  if (!entry || entry.ending) return
  entry.chunks.push(chunk(text))
  entry.wake?.()
}

export function end(index: number, error?: { name: string; message: string }) {
  const entry = requests[index]
  if (!entry || entry.ending) return
  entry.ending = error ? { error: errorValue(error) } : {}
  entry.wake?.()
}

export function fail(index: number, error: { name: string; message: string }) {
  const entry = requests[index]
  if (!entry || entry.settled) throw new Error(`request ${index} is not pending`)
  entry.settled = true
  entry.finished = true
  entry.reject(errorValue(error))
}

/** A settled promise's outcome, as the driver reports it. */
export function outcome(value: unknown) {
  if (value === undefined) return { undefined: true }
  if (value instanceof Uint8Array) return { bytes: [...value] }
  return { value }
}

export function failure(error: unknown) {
  if (error instanceof Error) {
    const tag = (error as { _tag?: unknown })._tag
    return { name: error.name, message: String(error.message), ...(typeof tag === "string" ? { tag } : {}) }
  }
  return { thrown: JSON.stringify(error) ?? String(error) }
}
