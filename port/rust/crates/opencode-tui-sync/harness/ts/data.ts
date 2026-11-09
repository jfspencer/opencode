// The data-layer cases: createData (packages/client/src/solid/data.ts) driven by event batches, calls into its API,
// HTTP responses to the requests it makes, connection status changes and virtual time. After each step the result
// records the new requests, the calls that settled, the store (JSON) and the internal sync state.
//
// A case either lists its steps, or asks to be generated (`generate: { seed, steps }`): then each next step is
// chosen at random from what the layer is doing (its pending requests are answered with plausible bodies), and the
// result carries the concrete steps, which both drivers then replay.
import path from "node:path"
import * as env from "./env"
import { Generator } from "../cases/data-generator"

type Step = Record<string, unknown> & { op: string }
type Case = Record<string, unknown> & { kind: string; name: string; steps?: Step[] }

let internals: { syncStates: Map<string, unknown>[] } | undefined

/** Exposes createData's store and internal state to the driver (the reference's source is not edited). */
export function instrument(reference: string) {
  if (internals) return internals
  internals = { syncStates: [] }
  const target = path.join(reference, "packages/client/src/solid/data.ts")
  ;(globalThis as Record<string, unknown>).__syncStates = internals.syncStates
  Bun.plugin({
    name: "opencode-tui-sync-harness",
    setup(build) {
      build.onLoad({ filter: /packages[/\\]client[/\\]src[/\\]solid[/\\]data\.ts$/ }, async (args) => {
        let source = await Bun.file(args.path).text()
        const sync = "const state = new Map<string, true | Pending>()"
        const end = "  return result\n}\n\nexport type Data"
        if (!source.includes(sync) || !source.includes(end)) throw new Error(`cannot instrument ${target}`)
        source = source.replace(sync, `${sync}\n  ;(globalThis as any).__syncStates.push(state)`)
        source = source.replace(
          end,
          "  ;(result as any).__internal = { store, outbox, sessionOutbox, defaultLocation }\n" + end,
        )
        return { contents: source, loader: "ts" }
      })
    },
  })
  return internals
}

function json(value: unknown) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value))
}

function syncSnapshot(state: Map<string, unknown>) {
  return Object.fromEntries(
    [...state].map(([key, entry]) => {
      if (entry === true) return [key, "done"]
      const pending = entry as { started: boolean; invalidated: boolean }
      return [key, !pending.started ? "waiting" : pending.invalidated ? "invalidated" : "pending"]
    }),
  )
}

/** The request a respond step names: an index, "last", or the first unsettled one matching `METHOD path`. */
export function requestIndex(target: unknown) {
  if (typeof target === "number") return target
  if (target === "last") return env.requests.length - 1
  if (target && typeof target === "object" && "match" in target) {
    const match = (target as { match: string }).match
    return env.requests.findIndex((request) => {
      if (request.settled) return false
      const url = new URL(request.recorded.url)
      return `${request.recorded.method} ${url.pathname}${url.search}`.startsWith(match)
    })
  }
  return -1
}

export async function runData(spec: Case, deps: { OpenCode: any; solid: any; reference: string }) {
  const { syncStates } = instrument(deps.reference)
  const client = path.join(deps.reference, "packages/client/src")
  const { createData } = await import(path.join(client, "solid/data.ts"))
  const { withMediaRefs } = await import(path.join(client, "promise/index.ts"))
  const { createGlobalEmitter } = await import(
    Bun.resolveSync("@solid-primitives/event-bus", path.join(deps.reference, "packages/tui/src/context"))
  )
  const solid = deps.solid
  const api = deps.OpenCode.make({ baseUrl: "http://127.0.0.1:4096", fetch: env.fetch })
  const generator = spec.generate ? new Generator(spec.generate as { seed: number; steps: number }) : undefined
  const steps: Step[] = generator ? [] : [...(spec.steps ?? [])]

  const { createSessionRetention } = await import(path.join(client, "solid/retention.ts"))
  const { lastAssistantWithUsage } = await import(path.join(client, "solid/usage.ts"))
  return solid.createRoot(async (dispose: () => void) => {
    const root = solid.getOwner()
    const [status, setStatus] = solid.createSignal((spec.status as string) ?? "connected")
    let retentionRuns = 0
    const [view, setView] = solid.createSignal<{ current?: string; keep: string[] }>({ keep: [] })
    const events = createGlobalEmitter()
    const errors: string[] = []
    syncStates.length = 0
    const data = createData({
      api: () => withMediaRefs(api),
      directory: (spec.directory as string) ?? "/project",
      initialMessageLimit: spec.initialMessageLimit === undefined ? undefined : () => spec.initialMessageLimit,
      event: { on: events.on, listen: events.listen },
      connection: { status },
      onError: spec.onError ? (error: unknown) => errors.push(env.failure(error).message ?? "?") : undefined,
      omitToolFileData: spec.omitToolFileData === true,
    })
    const state = syncStates.at(-1)!
    const internal = (data as any).__internal
    const settled: unknown[] = []
    let seen = 0
    const results: unknown[] = []
    const snapshot = (step: Step, value?: unknown) => {
      const requests = env.requests.slice(seen).map((request) => request.recorded)
      seen = env.requests.length
      results.push({
        op: step.op,
        ...(value === undefined ? {} : { value }),
        requests,
        settled: settled.splice(0).sort((a: any, b: any) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
        errors: errors.splice(0),
        retentionRuns,
        store: json(internal.store),
        internal: {
          sync: syncSnapshot(state),
          outbox: [...internal.outbox],
          sessionOutbox: [...internal.sessionOutbox],
          defaultLocation: json(internal.defaultLocation()),
        },
      })
    }
    const track = (id: string, promise: Promise<unknown>) =>
      promise.then(
        (value) => settled.push({ id, ok: json(value) ?? null }),
        (error) => settled.push({ id, error: env.failure(error) }),
      )
    let calls = 0
    for (let index = 0; ; index++) {
      const step = generator ? generator.next(env.requests, internal.store, status()) : steps[index]
      if (!step) break
      if (generator) steps.push(step)
      let value: unknown
      switch (step.op) {
        case "events":
          solid.batch(() => (step.events as { type: string }[]).forEach((event) => events.emit(event.type, event)))
          break
        case "call": {
          const id = (step.id as string) ?? `c${calls++}`
          const target = (step.path as string).split(".").reduce((node: any, key) => node?.[key], data)
          if (typeof target !== "function") throw new Error(`no data method ${step.path}`)
          const owner = (step.path as string).split(".").slice(0, -1).reduce((node: any, key) => node?.[key], data)
          let returned: unknown
          try {
            returned = target.apply(owner, (step.args as unknown[]) ?? [])
          } catch (error) {
            value = { threw: env.failure(error) }
            break
          }
          if (returned instanceof Promise) {
            track(id, returned)
            value = { promise: id }
          } else if (returned && typeof returned === "object" && "request" in returned && (returned as any).request instanceof Promise) {
            track(id, (returned as any).request)
            value = { id: (returned as any).id, promise: id }
          } else value = returned === undefined ? { undefined: true } : { value: json(returned) }
          break
        }
        case "respond": {
          const target = requestIndex(step.request)
          if (target >= 0 && env.requests[target] && !env.requests[target].settled) env.respond(target, step as unknown as env.ResponseSpec)
          break
        }
        case "fail": {
          const target = requestIndex(step.request)
          if (target >= 0 && env.requests[target] && !env.requests[target].settled) env.fail(target, step.error as never)
          break
        }
        case "status":
          setStatus(step.value as string)
          break
        case "retain":
          solid.runWithOwner(root, () =>
            createSessionRetention({
              session: data.session,
              current: () => (retentionRuns++, view().current),
              keep: () => view().keep,
              limit: step.limit as number,
            }),
          )
          break
        case "view":
          setView({ current: (step.current as string | null) ?? undefined, keep: (step.keep as string[]) ?? [] })
          break
        case "usage": {
          const found = lastAssistantWithUsage(data.session.message.list(step.session as string), (step.boundary as string | null) ?? undefined)
          value = found === undefined ? { undefined: true } : { value: json(found) }
          break
        }
        case "advance":
          await env.advance(step.ms as number)
          break
        case "dispose":
          dispose()
          break
      }
      await env.drain()
      snapshot(step, value)
    }
    dispose()
    return generator ? { steps, results } : { results }
  })
}
