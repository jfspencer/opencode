// Code mode language core: errors, diagnostics (parse errors, unsupported syntax, uncaught throws), imports, limits
// and the program result boundary, on the default interpreter and on the bytecode VM (`experimental.codemode_vm`).
import { call, say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

function programs(name: string, codes: readonly string[]) {
  for (const vm of process.env.CONFORMANCE_EXPERIMENTAL ? [false, true] : [false])
    scenario(
      `${name}${vm ? " (vm)" : ""}`,
      {
        config: vm ? { experimental: { codemode_vm: true } } : {},
        turns: [...codes.map((code) => call("execute", { code })), say("Done.")],
      },
      async (ctx) => {
        const id = await ctx.session()
        await ctx.prompt(id, `Run the ${name} programs`)
      },
    )
}

programs("error values and handling", [
  `const make = (E, ...args) => { const e = new E(...args); return { name: e.name, message: e.message, str: String(e), isError: e instanceof Error, own: Object.keys(e), proto: Object.getPrototypeOf(e) === E.prototype, isErr: Error.isError(e) } }
const types = [Error, TypeError, RangeError, ReferenceError, SyntaxError, EvalError, URIError].map((E) => make(E, "msg"))
const called = TypeError("without new")
const withCause = new Error("outer", { cause: new Error("inner") })
const noCause = new Error("x", {})
const causeUndefined = new Error("x", { cause: undefined })
const custom = new Error("custom")
custom.code = "E_CUSTOM"
custom.name = "CustomError"
const empty = new Error()
const numMsg = new Error(42)
const objMsg = new Error({ toString() { return "from toString" } })
return {
  types, called: [called.name, called.message, called instanceof TypeError],
  cause: [withCause.cause.message, "cause" in noCause, "cause" in causeUndefined, Object.keys(withCause)],
  custom: [String(custom), custom.code, JSON.stringify(custom), Object.prototype.toString.call(custom)],
  empty: [empty.message, String(empty)], numMsg: numMsg.message, objMsg: objMsg.message,
  isError: [Error.isError({ name: "Error", message: "fake" }), Error.isError("x"), Error.isError(new RangeError())],
  stack: typeof new Error("s").stack,
  toStringCall: Error.prototype.toString.call({ name: "N", message: "M" }),
  mapped: ["a", "b"].map(Error).map((e) => e.message),
  errorsAsData: [new Error("in array"), { nested: new TypeError("in object") }],
}`,
  `const caught = []
const tryThrow = (v) => { try { throw v } catch (e) { return [typeof e, e === null ? "null" : String(e)] } }
caught.push(tryThrow("a string"), tryThrow(42), tryThrow(null), tryThrow(undefined), tryThrow({ code: 1 }), tryThrow([1, 2]), tryThrow(() => 1))
const rethrow = () => { try { throw new RangeError("original") } catch (e) { e.message += " (annotated)"; throw e } }
try { rethrow() } catch (e) { caught.push(e.name + ": " + e.message) }
const builtinErrors = []
const capture = (f) => { try { f() } catch (e) { builtinErrors.push(e.constructor === undefined ? String(e) : e.name + ": " + e.message) } }
capture(() => null.prop)
capture(() => undefined.call())
capture(() => JSON.parse("{bad"))
capture(() => new Array(-1))
capture(() => "x".repeat(-1))
// toFixed's range message is the JS engine's own wording: record the error type only.
try { (1).toFixed(200) } catch (e) { builtinErrors.push(e.name) }
capture(() => decodeURIComponent("%"))
capture(() => new RegExp("("))
capture(() => Symbol())
capture(() => new Symbol())
capture(() => structuredClone(() => 1))
capture(() => Object.defineProperty({}, "a", {}))
capture(() => [].reduce((a, b) => a))
capture(() => [1].map(5))
capture(() => [1].forEach())
capture(() => Array.prototype.join.call(null))
capture(() => String.prototype.trim.call(undefined))
capture(() => [1].sort(5))
capture(() => "abc".replace("a", 5))
capture(() => JSON.parse("[1]", 5))
return { caught, builtinErrors }`,
  `const e = new TypeError("uncaught at top level")
e.detail = { reason: "test" }
throw e`,
  `throw "a plain string"`,
  `throw { code: 42, message: "object thrown" }`,
  `function deep() { return [1].map(() => { throw new RangeError("from a callback") }) }
deep()`,
  `console.log("before")
null.property`,
  `const err = new Error("with cause", { cause: { code: 7 } })
throw err`,
])

programs("parse errors and unsupported syntax", [
  `const x = ;`,
  `let a = 1
let a = 2`,
  `export const value = 1`,
  `return {`,
  "   ",
  `class Point { constructor(x) { this.x = x } }
new Point(1)`,
  `const o = { get value() { return 1 } }
o.value`,
  `const o = { set value(v) {} }
1`,
  `const big = 10n
big`,
  `const s = Symbol("custom")
s`,
  `with (Math) { max(1, 2) }`,
  `const C = class {}
C`,
  `const re = /a(b)c/g
re.exec("abc")`,
  `const t = new.target`,
  `debugger
1`,
  `function f() { return super.x }`,
  `async function f() { for await (const x of [1]) {} }
label: function g() {}
1`,
  `x => {}
import.meta`,
  `const o = { #priv: 1 }`,
  `let x = 1
x.#y`,
])

programs("imports and module globals", [
  `import { URL as U, URLSearchParams } from "url"
import * as urlModule from "node:url"
import util from "util"
import { atob } from "buffer"
import "url"
const req = require("url")
const dyn = await import("util")
return {
  u: new U("https://example.com/a?b=1").pathname,
  params: new URLSearchParams("x=1").get("x"),
  ns: Object.keys(urlModule).sort(),
  utilKeys: Object.keys(util).sort(),
  atob: atob("aGk="),
  req: typeof req.URL,
  dyn: Object.keys(dyn).sort(),
  same: req === urlModule.default,
}`,
  `import fs from "fs"
fs`,
  `import fsp from "fs/promises"
fsp`,
  `const r = require("child_process")
r`,
  `const r = await import("os").catch((e) => e.name + ": " + e.message)
const notString = (() => { try { return require(5) } catch (e) { return e.message } })()
return { r, notString }`,
  `function inner() { import("url") }
inner()
const late = () => { const m = require("url"); return typeof m }
late()`,
  `if (true) { import x from "url" }`,
])

programs("limits and runaway values", [
  `const out = {}
const capture = (label, f) => { try { out[label] = f() } catch (e) { out[label] = e.name + ": " + e.message } }
capture("repeat", () => "x".repeat(2 ** 25).length)
capture("padStart", () => "x".padStart(2 ** 25).length)
capture("concat", () => { let s = "y".repeat(2 ** 23); s = s + s; s = s + s; return (s + s).length })
capture("template", () => { const s = "z".repeat(2 ** 23); return \`\${s}\${s}\${s}\`.length })
capture("join", () => new Array(2 ** 13).fill("w".repeat(2 ** 12)).join("-").length)
capture("arrayCtor", () => new Array(2 ** 30).length)
capture("arrayLength", () => { const a = []; a.length = 2 ** 31; return a.length })
capture("arrayFrom", () => Array.from({ length: 2 ** 30 }).length)
capture("badLength", () => { const a = []; a.length = -1 })
capture("badLength2", () => { const a = []; a.length = 1.5 })
capture("ok", () => "a".repeat(1000).length)
return out`,
  `const depth = (n) => (n === 0 ? {} : { child: depth(n - 1) })
const deep = depth(40)
return deep`,
  `const big = []
for (let i = 0; i < 20000; i++) big.push(i)
const scrambled = big.map((i) => (i * 7919) % 20011)
const sorted = [...scrambled].sort()
const sortedNumeric = [...scrambled].sort((a, b) => a - b)
const withHoles = [...scrambled.slice(0, 5000), undefined, , 3].sort()
return { length: big.length, sorted: sorted.slice(0, 5), last: sorted.at(-1), numeric: sortedNumeric.slice(0, 5), holes: [withHoles.length, withHoles.slice(-3)] }`,
  `const out = {}
const nest = (make) => { let a = make(null); for (let i = 0; i < 100000; i++) a = make(a); return a }
const capture = (label, f) => { try { out[label] = f() } catch (e) { out[label] = e.name + ": " + e.message } }
capture("string", () => String(nest((a) => (a === null ? [] : [a]))).length)
capture("clone", () => { structuredClone(nest((a) => (a === null ? {} : { a }))); return "cloned" })
capture("flat", () => nest((a) => (a === null ? [] : [a])).flat(Infinity).length)
capture("stringify", () => JSON.stringify(nest((a) => (a === null ? [] : [a]))).length)
console.log(nest((a) => (a === null ? [] : [a])))
return out`,
  `const values = []
for (let i = 0; i < 10001; i++) values.push(new Promise(() => {}))
values.length`,
])

programs("result boundary and REPL values", [
  `1 + 1`,
  `let x = 5
x * 2;`,
  `const a = 1`,
  `undefined`,
  `function f() {}
if (true) { "block value" }`,
  `({ toJSON() { return { replaced: true } }, ignored: 1 })`,
  `return { fn: () => 1, undef: undefined, nan: NaN, inf: -Infinity, arr: [undefined, () => 1, NaN], date: new Date(0), map: new Map([[1, 2]]), set: new Set([1, 2]), re: /x/g, err: new RangeError("boundary"), params: new URLSearchParams("a=1"), nested: { toJSON: () => "nested toJSON" }, negZero: -0, proto: { __proto__: { x: 1 }, own: 1 } }`,
  `const o = { a: 1 }
o.self = o
return o`,
  `const a = [1]
a.push(a)
a`,
  `return new Uint8Array([1, 2, 3])`,
  `return Promise.resolve({ awaited: "on return" })`,
  `return (function* () { yield 1 })()`,
  `return Symbol.iterator`,
  `return { [Symbol.iterator]: 1, plain: 2 }`,
  `const thenable = { then(resolve) { resolve("thenable result") } }
return thenable`,
  `return "a string result"`,
  `return null`,
  `return [1, [2, [3, [4]]]]`,
])

programs("console output formatting", [
  `console.log("string", 1, true, null, undefined, NaN, -0)
console.log({ a: 1, nested: { b: [1, 2, { c: 3 }] } }, [1, "two", [3]])
console.log(() => 1, function named() {}, Math.max)
console.log(new Map([["k", { v: 1 }]]), new Set([1, "a"]), new Date(86400000), /re/gi)
console.log(new Error("logged error"), new TypeError("typed"))
console.log(Promise.resolve(1), (function* () {})(), Symbol.iterator)
console.info("info"); console.warn("warn"); console.error("error"); console.debug("debug")
console.log()
console.log("%s and %d", "fmt", 5)
const circ = { name: "circ" }
circ.list = [1]
console.log(circ)
console.log(new URL("https://example.com/x"), new URLSearchParams("q=1"), new Uint8Array([1, 2]))
console.log([undefined, null, , 3])
console.log("multi\\nline")
console.log(new WeakMap(), new WeakSet(), new Headers({ a: "1" }), String(new Uint8Array([1, 2])), [new Date(0)], { d: new Date(0), u: new URL("https://x.test/") })
return "logged"`,
  `console.log("logged before failure")
console.warn("warned")
throw new Error("after logs")`,
  `for (let i = 0; i < 3; i++) console.log("line " + i)
console.table?.([{ a: 1 }])
console.dir?.({ d: 1 })
console.trace?.("t")
"done"`,
])
