// Runs a reactive script (see gen.ts for the language) on solid-js 1.9.17 and prints the log, one event per line.
// usage: bun solid.ts <script file>
import { readFileSync } from "node:fs"
import path from "node:path"

const reference = process.env.OPENCODE_REFERENCE!
const solid = await import(
  path.join(reference, "node_modules/.bun/solid-js@1.9.17/node_modules/solid-js/dist/solid.js")
)
const { createRoot, createSignal, createMemo, createEffect, createRenderEffect, createComputed, batch, onCleanup, onMount, on, mapArray, untrack } =
  solid

type Line = { words: string[]; block?: Line[] }

function parse(text: string): Line[] {
  const lines = text.split("\n").map((l) => l.trim()).filter((l) => l && !l.startsWith("#"))
  let i = 0
  function block(): Line[] {
    const out: Line[] = []
    while (i < lines.length) {
      const words = lines[i++].split(/\s+/)
      if (words[0] === "end") return out
      const line: Line = { words }
      if (words[words.length - 1] === "{") {
        words.pop()
        line.block = block()
      }
      out.push(line)
    }
    return out
  }
  return block()
}

const log: string[] = []
const signals = new Map<string, [() => any, (v: any) => void]>()
const read = (name: string) => signals.get(name)![0]()
const values = (deps: string[]) => deps.map(read)
const sum = (deps: string[]) => values(deps).reduce((a: number, b: number) => a + b, 0)

function setup(lines: Line[]) {
  for (const line of lines) {
    const [op, name, ...rest] = line.words
    switch (op) {
      case "signal": {
        signals.set(name, createSignal(Number(rest[0])))
        break
      }
      case "list": {
        signals.set(name, createSignal(rest.map(Number)))
        break
      }
      case "memo": {
        const memo = createMemo(() => {
          const v = sum(rest)
          log.push(`memo ${name} = ${v}`)
          if (line.block) body(name, rest, line.block)
          return v
        })
        signals.set(name, [memo, () => {}])
        break
      }
      case "effect":
      case "render":
      case "computed": {
        const create = op === "effect" ? createEffect : op === "render" ? createRenderEffect : createComputed
        create(() => {
          log.push(`${op} ${name} ${values(rest).join(" ")}`)
          if (line.block) body(name, rest, line.block)
        })
        break
      }
      case "oneffect": {
        const [mode, ...deps] = rest
        createEffect(
          on(
            () => values(deps),
            (input: number[], prev: number[] | undefined) =>
              log.push(`on ${name} ${input.join(" ")} prev ${prev ? prev.join(" ") : "-"}`),
            { defer: mode === "defer" },
          ),
        )
        break
      }
      case "mount":
        onMount(() => log.push(`mount ${name}`))
        break
      case "cleanup":
        onCleanup(() => log.push(`cleanup ${name}`))
        break
      case "for": {
        const list = rest[0]
        const memo = createMemo(
          mapArray(
            () => read(list),
            (item: number, index: () => number) => {
              log.push(`row ${name} ${item} ${untrack(index)}`)
              onCleanup(() => log.push(`unrow ${name} ${item}`))
              createRenderEffect(() => log.push(`index ${name} ${item} ${index()}`))
              if (line.block) setup(line.block)
              return item
            },
          ),
        )
        createRenderEffect(() => log.push(`rows ${name} ${memo().join(" ")}`))
        break
      }
      default:
        throw new Error(`unknown setup op ${op}`)
    }
  }
}

// The block of a computation: runs on each run; writes go to signals computed from the deps.
function body(name: string, deps: string[], lines: Line[]) {
  onCleanup(() => log.push(`cleanup ${name}`))
  const nested: Line[] = []
  for (const line of lines) {
    if (line.words[0] === "write") {
      const [, target, offset] = line.words
      const value = sum(deps) + Number(offset)
      untrack(() => signals.get(target)![1](value))
    } else nested.push(line)
  }
  setup(nested)
}

let dispose: () => void = () => {}
function act(lines: Line[]) {
  for (const line of lines) {
    const [op, name, ...rest] = line.words
    log.push(`> ${line.words.join(" ")}`)
    switch (op) {
      case "set":
        signals.get(name)![1](Number(rest[0]))
        break
      case "setlist":
        signals.get(name)![1](rest.map(Number))
        break
      case "batch":
        batch(() => act(line.block ?? []))
        break
      case "dispose":
        dispose()
        break
      case "read":
        log.push(`read ${name} ${read(name)}`)
        break
      default:
        throw new Error(`unknown action ${op}`)
    }
  }
}

const program = parse(readFileSync(process.argv[2], "utf8"))
const split = program.findIndex((line) => line.words[0] === "actions")
const setupLines = split < 0 ? program : program.slice(0, split)
const actions = split < 0 ? [] : program.slice(split + 1)
createRoot((d: () => void) => {
  dispose = d
  setup(setupLines)
})
log.push("> ready")
act(actions)
console.log(log.join("\n"))
