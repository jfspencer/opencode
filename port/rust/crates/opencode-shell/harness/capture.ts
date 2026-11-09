// Capture cases for the differential harness: the TS Shell service runs `cat` on generated output files (real
// processes, so output arrives in the chunks a pipe delivers) under several tool_output limits, and Shell.result's
// capture is the expected answer for the Rust `result::capture` of the same bytes.
import { mkdirSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { pick, type Random } from "./generate"

const LIMITS: Array<[number | undefined, number | undefined]> = [
  [undefined, undefined],
  [3, 40],
  [1, 1],
  [5, 1000],
  [100, 7],
  [2, 2000],
]

function content(rand: Random): Uint8Array {
  const pieces: number[] = []
  const lines = Math.floor(rand() * 12)
  for (let i = 0; i < lines; i++) {
    const length = Math.floor(rand() * (rand() < 0.2 ? 80 : 12))
    for (let j = 0; j < length; j++) pieces.push(pick(rand, [0x61, 0x62, 0x20, 0x09, 0xc3, 0xa9, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x98, 0x80, 0xff, 0x80, 0x0d]))
    if (rand() < 0.85) pieces.push(0x0a)
  }
  if (rand() < 0.1) for (let i = 0; i < 3000; i++) pieces.push(i % 50 === 49 ? 0x0a : 0x78)
  return new Uint8Array(pieces)
}

export async function captureCases(core: string, out: string, rand: Random, count: number) {
  const load = (relative: string) => import(path.join(core, relative))
  const { Effect, References } = await import(Bun.resolveSync("effect", core))
  const { AppNodeBuilder } = await load("src/effect/app-node-builder.ts")
  const { Config } = await load("src/config.ts")
  const { Environment } = await load("src/environment/index.ts")
  const { Location } = await load("src/location.ts")
  const { Shell } = await load("src/shell.ts")
  const { Global } = await import(Bun.resolveSync("@opencode/util/global", core))
  const { Document, Info } = await import(Bun.resolveSync("@opencode/schema/config", core))
  const { ConfigToolOutput } = await import(Bun.resolveSync("@opencode/schema/config/tool-output", core))
  const { hostEnvironmentLayer } = await load("test/fixture/environment.ts")
  const { tempGlobalLayer } = await load("test/fixture/global.ts")
  const { tempLocationLayer } = await load("test/fixture/location.ts")

  const directory = path.join(out, "capture")
  rmSync(directory, { recursive: true, force: true })
  mkdirSync(directory, { recursive: true })
  const results: Array<{ case: Record<string, unknown>; answer: string }> = []
  const perLimit = Math.ceil(count / LIMITS.length)
  for (const [maxLines, maxBytes] of LIMITS) {
    const entries =
      maxLines === undefined
        ? []
        : [new Document({ type: "document", info: new Info({ tool_output: new ConfigToolOutput.Info({ max_lines: maxLines, max_bytes: maxBytes }) }) })]
    const inputs = Array.from({ length: perLimit }, (_, index) => {
      const bytes = content(rand)
      const file = path.join(directory, `${maxLines}-${maxBytes}-${index}.out`)
      writeFileSync(file, bytes)
      return { bytes, file }
    })
    const program = Effect.gen(function* () {
      const shell = yield* Shell.Service
      for (const input of inputs) {
        const info = yield* shell.create({ shell: "/bin/sh", command: `cat '${input.file}'`, timeout: 0 })
        const result = yield* shell.result(info)
        const size = input.bytes.length
        const limitBytes = maxBytes ?? 50 * 1024
        const tail = Array.from(input.bytes.slice(Math.max(0, size - limitBytes)))
        results.push({
          case: {
            fn: "capture",
            tail,
            size,
            newlines: input.bytes.filter((byte) => byte === 0x0a).length,
            maxLines: maxLines ?? 2000,
            maxBytes: limitBytes,
            file: info.file,
          },
          answer: JSON.stringify({ output: result.capture.output, truncated: result.capture.truncated }),
        })
        // A page of the same output, as the shell API and session shells read it.
        const request = Object.fromEntries(
          [
            ["cursor", rand() < 0.3 ? undefined : pick(rand, [0, 1, 2, 3, size - 1, size, size + 5, Math.floor(size / 2)].filter((n) => n >= 0))],
            ["limit", rand() < 0.3 ? undefined : pick(rand, [0, 1, 2, 3, 7, 100, 65536])],
          ].filter(([, value]) => value !== undefined),
        )
        const page = yield* shell.output(info.id, request)
        results.push({
          case: { fn: "page", bytes: Array.from(input.bytes), size, ...request },
          answer: JSON.stringify(page),
        })
      }
    }).pipe(
      Effect.provide(
        AppNodeBuilder.build(Shell.node, [
          Location.node.replace(tempLocationLayer),
          Global.node.replace(tempGlobalLayer),
          Config.node.replace(Config.testLayer(entries)),
          Environment.node.replace(hostEnvironmentLayer),
        ]),
      ),
    )
    await Effect.runPromise(Effect.scoped(program).pipe(Effect.provideService(References.MinimumLogLevel, "Error")))
  }
  rmSync(directory, { recursive: true, force: true })
  return results
}
