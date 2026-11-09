// The context cases: the TUI's real providers (context/log.tsx, client.tsx, data.tsx, location.tsx and event.ts)
// mounted without a renderer, the client's connection reading a scripted event stream, and the TUI's calls into
// the contexts. Each step reports the requests, settled calls, the connection, the data store and the location.
import path from "node:path"
import * as env from "./env"
import { instrument, requestIndex } from "./data"
import { ContextGenerator } from "../cases/context-generator"

type Step = Record<string, unknown> & { op: string }
type Case = Record<string, unknown> & { kind: string; name: string; steps?: Step[] }

function json(value: unknown) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value))
}

export async function runContext(spec: Case, deps: { OpenCode: any; solid: any; reference: string }) {
  const { syncStates } = instrument(deps.reference)
  const context = path.join(deps.reference, "packages/tui/src/context")
  const { LogProvider } = await import(path.join(context, "log.tsx"))
  const { ClientProvider, useClient } = await import(path.join(context, "client.tsx"))
  const { DataProvider, useData } = await import(path.join(context, "data.tsx"))
  const { LocationProvider, useLocation } = await import(path.join(context, "location.tsx"))
  const { useEvent } = await import(path.join(context, "event.ts"))
  const solid = deps.solid
  const api = deps.OpenCode.make({ baseUrl: "http://127.0.0.1:4096", fetch: env.fetch })
  const generator = spec.generate ? new ContextGenerator(spec.generate as { seed: number; steps: number }) : undefined
  const steps: Step[] = generator ? [] : [...(spec.steps ?? [])]
  const logs: unknown[] = []
  const seenEvents: string[] = []
  const captured: Record<string, any> = {}
  syncStates.length = 0
  let disposeRoot = () => {}
  solid.createRoot((dispose: () => void) => {
    disposeRoot = dispose
    solid.createComponent(LogProvider, {
      log: (level: string, message: string, tags: unknown) => logs.push([level, message, json(tags)]),
      get children() {
        return solid.createComponent(ClientProvider, {
          api,
          url: "http://127.0.0.1:4096",
          get children() {
            return solid.createComponent(DataProvider, {
              directory: (spec.directory as string) ?? "/project",
              get children() {
                return solid.createComponent(LocationProvider, {
                  get children() {
                    captured.client = useClient()
                    captured.data = useData()
                    captured.location = useLocation()
                    useEvent().subscribe((event: { type: string }, metadata: { directory?: string }) =>
                      seenEvents.push(`${event.type}@${metadata.directory ?? ""}`),
                    )
                    return null
                  },
                })
              },
            })
          },
        })
      },
    })
  })
  const internal = (captured.data as any).__internal ?? undefined
  const state = syncStates.at(-1)
  const settled: unknown[] = []
  const track = (id: string, promise: Promise<unknown>) =>
    promise.then(
      (value) => settled.push({ id, ok: value instanceof Uint8Array ? { bytes: [...value] } : (json(value) ?? null) }),
      (error) => settled.push({ id, error: env.failure(error) }),
    )
  const results: unknown[] = []
  let seen = 0
  let calls = 0
  const snapshot = (step: Step) => {
    const requests = env.requests.slice(seen).map((request) => request.recorded)
    seen = env.requests.length
    const location = captured.location
    const error = location.error
    const store = json(internal?.store) ?? json((captured.data as any).__store)
    results.push({
      op: step.op,
      requests,
      settled: settled.splice(0).sort((a: any, b: any) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
      connection: {
        status: captured.client.connection.status(),
        attempt: captured.client.connection.attempt(),
        error: captured.client.connection.error() ?? null,
      },
      location: {
        ref: json(location.ref) ?? null,
        current: json(location.current) ?? null,
        error: error ? { location: json(error.location), cause: env.failure(error.cause) } : null,
      },
      titles: ["ses_a", "ses_b", "ses_c"].filter((id) => captured.data.session.title.pending(id)),
      events: seenEvents.splice(0),
      logs: logs.splice(0),
      store,
      sync: state ? Object.fromEntries([...state].map(([k, v]: [string, any]) => [k, v === true ? "done" : !v.started ? "waiting" : v.invalidated ? "invalidated" : "pending"])) : null,
    })
  }
  await env.drain()
  snapshot({ op: "mount" })
  for (let index = 0; ; index++) {
    const step = generator ? generator.next(env.requests) : steps[index]
    if (!step) break
    if (generator) steps.push(step)
    const target = requestIndex(step.request)
    const id = `c${calls}`
    switch (step.op) {
      case "respond":
        if (target >= 0 && env.requests[target] && !env.requests[target].settled) env.respond(target, step as unknown as env.ResponseSpec)
        break
      case "chunk":
        env.push(target, step.data as string)
        break
      case "end":
        env.end(target, step.error as { name: string; message: string } | undefined)
        break
      case "fail":
        if (target >= 0 && env.requests[target] && !env.requests[target].settled) env.fail(target, step.error as never)
        break
      case "advance":
        await env.advance(step.ms as number)
        break
      case "location.set":
        captured.location.set((step.location as object | null) ?? undefined)
        break
      case "data.location.sync":
        calls++
        track(id, captured.data.location.sync((step.location as object | null) ?? undefined))
        break
      case "title.generate":
        calls++
        track(id, captured.data.session.title.generate(step.session))
        break
      case "media.read":
        calls++
        track(id, captured.client.media.read(step.uri))
        break
      case "data.session.sync":
        calls++
        track(id, captured.data.session.sync(step.session))
        break
      case "data.session.message.sync":
        calls++
        track(id, captured.data.session.message.sync(step.session))
        break
    }
    await env.drain()
    snapshot(step)
  }
  disposeRoot()
  return generator ? { steps, results } : { results }
}
