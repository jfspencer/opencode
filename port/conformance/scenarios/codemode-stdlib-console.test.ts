// Code-mode standard library: console capture and value formatting, on the legacy interpreter and the bytecode VM.
// Every console method appends one line to the "Logs:" section of the execute result.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

function programs(name: string, codes: string[]) {
  for (const vm of process.env.CONFORMANCE_EXPERIMENTAL ? [false, true] : [false])
    scenario(
      `${name}${vm ? " (vm)" : ""}`,
      {
        config: vm ? { experimental: { codemode_vm: true } } : {},
        turns: [
          ...codes.map((code, index): Turn => ({
            events: [{ tool: "execute", input: { code }, id: `call_${index}` }],
          })),
          say("Done."),
        ],
      },
      async (ctx) => {
        const id = await ctx.session()
        await ctx.prompt(id, `Run the ${name} programs`)
      },
    )
}

programs("console formatting", [
  `console.log("plain", 1, -0, NaN, true, null, undefined, "quoted? no")
console.info("info line")
console.debug("debug line")
console.warn("warn line", { a: 1 })
console.error("error line", [1, "two"])
console.log()
console.log({ s: "str", n: 1.5, b: false, nil: null, u: undefined, arr: [1, [2, [3]]], nested: { deep: { deeper: "x" } } })
console.log([undefined, null, , 4])
console.log(new Map([["k", { v: 1 }], [{ o: 1 }, "objkey"]]), new Set([1, "s", [2]]))
console.log(new WeakMap(), new WeakSet())
console.log(new Date(Date.UTC(2021, 5, 7)).getTime(), new Date(NaN), /re+/gi)
console.log(new Error("boom"), new TypeError("typed"), Object.assign(new RangeError("ranged"), { code: 7 }))
const e = new Error("named"); e.name = ""; console.log(e)
const e2 = new Error(""); console.log(e2)
console.log(() => 1, function named() {}, Math.max, Promise.resolve(1), tools, tools.opencode, (function* () {})(), [].values())
console.log(Symbol.iterator, Symbol.asyncIterator)
console.log(new URL("https://example.com/p?q=1"), new URLSearchParams("a=1&b=2"), new Headers({ "X-A": "1" }), new Uint8Array([1, 2, 3]))
console.log({ fn() {}, arr: [() => 2] })
console.log(Object.create(null))
function args() { return arguments }
console.log(args(1, "a"))
const m = new Map(); m.set("self", m); console.log(m)
const s = new Set(); s.add(s); console.log(s)
let deep = { v: 0 }; for (let i = 0; i < 40; i++) { const m2 = new Map(); m2.set("d", deep); deep = m2 } console.log(deep)
console.dir({ dir: [1, 2] })
console.dir()
console.dir("a string")
"done"`,
  `console.table([{ a: 1, b: "x" }, { a: 2, c: true }])
console.table([{ a: 1, b: 2 }, { a: 3, b: 4 }], ["b"])
console.table({ row1: { col: 1 }, row2: { col: 2, other: [1] } })
console.table([1, "two", null])
console.table([[1, 2], { k: undefined }])
console.table("scalar")
console.table(5)
console.table()
console.table([])
console.table([Promise.resolve(1)])
console.table({ r: { nested: { x: 1 }, m: new Map([[1, 2]]) } })
console.table([{ a: 1 }], "not-an-array")
const methods = Object.keys(console)
const names = ["log", "info", "debug", "warn", "error", "dir", "table"].map((n) => typeof console[n])
return { methods, names, tag: Object.prototype.toString.call(console), returns: console.log("x") === undefined }`,
  `console.log("before the failure")
console.warn({ state: "partial" })
throw new Error("program failed after logging")`,
  `for (let i = 0; i < 5; i++) console.log("line", i)
const results = [1, 2, 3].map((x) => { console.log("mapping", x); return x * 2 })
console.log(JSON.stringify(results), String(results), \`\${results}\`)
console.log("%s is not a format", "this", "%d")
results`,
  `const out = [1, 2].forEach(console.log)
const items = ["a", "b"]
items.forEach(console.error)
const bound = console.info.bind(console)
bound("bound info")
console.log.call(null, "called log")
console.warn.apply(console, ["applied", "warn"])
const { log } = console
log("destructured log")
console.log(console.log.name, console.table.length)
null`,
])
