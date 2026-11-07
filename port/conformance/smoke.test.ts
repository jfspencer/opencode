import { expect, test } from "bun:test"
import { OpenCode } from "@opencode/client"
import { Effect } from "effect"
import { ServerFetch } from "../../packages/server/src/fetch"

// Proves the conformance suite can drive the TS reference in-process (needed for coverage) from port/,
// without port/ being a member of upstream's bun workspace.
test("drives the TS reference server through the generated client", () =>
  Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const handler = yield* ServerFetch.make({
          app: { version: "conformance" },
          database: { path: ":memory:" },
          config: { project: false },
          models: { fetch: false },
          fs: { filewatcher: false },
        })
        const api = OpenCode.make({
          baseUrl: "http://opencode.local",
          fetch: Object.assign((input: string | URL | Request, init?: RequestInit) => handler(new Request(input, init)), {
            preconnect: fetch.preconnect,
          }),
        })
        const session = yield* Effect.promise(() => api.session.create({ title: "conformance smoke" }))
        expect(session.title).toBe("conformance smoke")
      }),
    ),
  ))
