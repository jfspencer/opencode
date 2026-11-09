// Chooses the next step of a generated context case: the event stream the client context's connection opens is
// answered with server.connected and then events in chunks (sometimes cut, so it reconnects), the data layer's
// requests are answered as in data cases, and the TUI calls the contexts (location.set, location.sync, title
// generation, media reads).
import { Generator } from "./data-generator"

type Pending = { recorded: { method: string; url: string; body: string | null }; settled: boolean }
type Step = Record<string, unknown> & { op: string }

const sse = (event: unknown) => `data: ${JSON.stringify(event)}\n\n`

export class ContextGenerator {
  private data: Generator
  private remaining: number
  private stream = -1

  constructor(options: { seed: number; steps: number }) {
    this.data = new Generator({ seed: options.seed, steps: Number.MAX_SAFE_INTEGER })
    this.remaining = options.steps
  }

  next(requests: Pending[]): Step | undefined {
    if (this.remaining-- <= 0) return undefined
    const random = this.data.random
    const pending = requests.map((request, index) => ({ request, index })).filter((entry) => !entry.request.settled)
    const events = pending.find((entry) => new URL(entry.request.recorded.url).pathname === "/api/event")
    if (events && random.bool(0.6)) {
      this.stream = events.index
      return {
        op: "respond",
        request: events.index,
        status: 200,
        headers: [["content-type", "text/event-stream"]],
        chunks: [sse({ id: "evt_connected", type: "server.connected", data: {} })],
        end: "open",
      }
    }
    const roll = random.next()
    const open = this.stream >= 0 && requests[this.stream] && requests[this.stream].settled
    if (open && roll < 0.4) {
      const text = Array.from({ length: 1 + random.int(3) }, () => sse(this.data.event())).join("")
      return { op: "chunk", request: this.stream, data: text }
    }
    if (open && roll < 0.43) {
      const stream = this.stream
      this.stream = -1
      return { op: "end", request: stream, ...(random.bool() ? { error: { name: "TypeError", message: "closed" } } : {}) }
    }
    const others = pending.filter((entry) => entry !== events)
    if (others.length && roll < 0.75) return this.data.answer(others)
    if (roll < 0.83) return { op: "advance", ms: random.pick([10, 11, 150, 500, 1000, 2000]) }
    const session = random.pick(["ses_a", "ses_b", "ses_c"])
    const call = random.int(6)
    if (call === 0) return { op: "location.set", location: random.bool(0.2) ? null : { directory: random.pick(["/project", "/other", "/gone"]) } }
    if (call === 1) return { op: "data.location.sync", location: random.bool(0.5) ? null : { directory: "/other" } }
    if (call === 2) return { op: "title.generate", session }
    if (call === 3) return { op: "media.read", uri: random.pick([`media:${"a".repeat(64)}`, `media:${"b".repeat(64)}`, "file:///x"]) }
    if (call === 4) return { op: "data.session.sync", session }
    return { op: "data.session.message.sync", session }
  }
}
