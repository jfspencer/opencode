// Code-mode standard library: Object, JSON, and the program-result boundary, on the legacy interpreter and the
// bytecode VM.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const PRELUDE = `const r = {}
const show = (v) => v === undefined ? "<undefined>" : typeof v === "number" && (!Number.isFinite(v) || Object.is(v, -0)) ? "<" + (Object.is(v, -0) ? "-0" : String(v)) + ">" : v
const t = (k, f) => { try { r[k] = show(f()) } catch (e) { r[k] = "!" + (e && e.name) + ": " + (e && e.message) } }
`

function programs(name: string, codes: string[], prelude = true) {
  for (const vm of process.env.CONFORMANCE_EXPERIMENTAL ? [false, true] : [false])
    scenario(
      `${name}${vm ? " (vm)" : ""}`,
      {
        config: vm ? { experimental: { codemode_vm: true } } : {},
        turns: [
          ...codes.map((code, index): Turn => ({
            events: [{ tool: "execute", input: { code: (prelude ? PRELUDE : "") + code }, id: `call_${index}` }],
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

programs("object statics", [
  `const o = { b: 2, a: 1, 10: "ten", 2: "two", "x-y": 3 }
t("keys", () => [Object.keys(o), Object.keys([5, 6]), Object.keys("ab"), Object.keys(5), Object.keys(true)])
t("keys null", () => Object.keys(null))
t("keys undefined", () => Object.keys(undefined))
t("keys promise", () => Object.keys(Promise.resolve({})))
t("keys tools", () => Object.keys(tools))
t("keys tool namespace", () => Object.keys(tools.opencode).length > 0)
t("values", () => [Object.values(o), Object.values("hi"), Object.values([1, , 3])])
t("values tools", () => Object.values(tools))
t("entries", () => [Object.entries({ a: 1, b: [2] }), Object.entries("ab")])
t("entries null", () => Object.entries(null))
t("hasOwn", () => [Object.hasOwn(o, "a"), Object.hasOwn(o, "toString"), Object.hasOwn([1], 0), Object.hasOwn([1], "length"), Object.hasOwn("ab", 1), Object.hasOwn(o, 10)])
t("hasOwn null", () => Object.hasOwn(null, "a"))
t("is", () => [Object.is(NaN, NaN), Object.is(0, -0), Object.is(o, o), Object.is({}, {}), Object.is("a", "a")])
t("assign", () => Object.assign({ a: 1 }, { b: 2 }, null, undefined, { a: 3 }, "xy", 5))
t("assign array target", () => Object.assign([1, 2], [9], { 2: "c" }))
t("assign primitive target", () => Object.assign(1, { a: 1 }))
t("assign null target", () => Object.assign(null, {}))
t("assign frozen target", () => Object.assign(Object.freeze({ a: 1 }), { a: 2 }))
t("assign non-extensible", () => Object.assign(Object.preventExtensions({}), { z: 1 }))
t("assign circular", () => { const x = {}; return Object.assign(x, { self: x }) })
t("assign promise source", () => Object.assign({}, Promise.resolve(1)))
t("fromEntries", () => [Object.fromEntries([["a", 1], ["b", 2]]), Object.fromEntries(new Map([["m", true]])), Object.fromEntries(new URLSearchParams("q=1&r=2"))])
t("fromEntries generator", () => { function* g() { yield ["k", "v"]; yield [1, 2] } return Object.fromEntries(g()) })
t("fromEntries bad entry", () => Object.fromEntries([1]))
t("fromEntries opaque entry", () => Object.fromEntries([Promise.resolve(1)]))
t("fromEntries not iterable", () => Object.fromEntries({ a: 1 }))
t("fromEntries closes", () => { let closed = false; const it = { [Symbol.iterator]() { return { next() { return { done: false, value: 5 } }, return() { closed = true; return {} } } } }; try { Object.fromEntries(it) } catch (e) { return [e.message, closed] } })
t("create", () => { const proto = { greet() { return "hi " + this.n } }; const x = Object.create(proto); x.n = "z"; return [x.greet(), Object.getPrototypeOf(x) === proto, Object.keys(x), "greet" in x, x.hasOwnProperty("greet")] })
t("create null", () => { const x = Object.create(null); x.a = 1; return [Object.keys(x), Object.getPrototypeOf(x), typeof x.toString] })
t("create invalid proto", () => Object.create(5))
t("create descriptors", () => Object.create({}, { a: { value: 1 } }))
t("create undefined descriptors", () => Object.keys(Object.create({}, undefined)))
t("getPrototypeOf", () => [Object.getPrototypeOf({}) === Object.prototype, Object.getPrototypeOf(Object.prototype), Object.getPrototypeOf(1) === Number.prototype, Object.getPrototypeOf(true) === Boolean.prototype, Object.getPrototypeOf(() => 1) === Function.prototype])
t("getPrototypeOf null", () => Object.getPrototypeOf(null))
t("getPrototypeOf symbol", () => Object.getPrototypeOf(Symbol.iterator))
t("Object()", () => [Object(), Object(null), Object(undefined), new Object(), Object([1])])
t("Object(1)", () => Object(1))
t("new Object('s')", () => new Object("s"))
t("groupBy", () => Object.groupBy([1, 2, 3, 4, 5], (x) => (x % 2 ? "odd" : "even")))
t("groupBy proto", () => [Object.getPrototypeOf(Object.groupBy([1], () => "a")), typeof Object.groupBy([1], () => "toString").toString])
t("groupBy keys", () => Object.groupBy(["a", "bb", "cc"], (s, i) => ({ toString() { return "len" + s.length } })))
t("groupBy index", () => Object.groupBy("abc", (ch, i) => i % 2))
t("groupBy null", () => Object.groupBy(null, (x) => x))
t("groupBy not iterable", () => Object.groupBy(5, (x) => x))
t("groupBy bad callback", () => Object.groupBy([1], 5))
t("groupBy opaque key", () => Object.groupBy([1], () => Math.max))
t("groupBy promise key", () => Object.keys(Object.groupBy([1], async () => 1)))
return r`,
  `t("freeze", () => { const x = Object.freeze({ a: 1, n: { b: 2 } }); const out = []; try { x.a = 2 } catch (e) { out.push(e.message) } try { x.c = 3 } catch (e) { out.push(e.message) } try { delete x.a } catch (e) { out.push(e.message) } x.n.b = 5; return [out, x, Object.isFrozen(x), Object.isSealed(x), Object.isExtensible(x)] })
t("seal", () => { const x = Object.seal({ a: 1 }); x.a = 2; const out = []; try { x.b = 1 } catch (e) { out.push(e.message) } try { delete x.a } catch (e) { out.push(e.message) } return [out, x, Object.isFrozen(x), Object.isSealed(x)] })
t("preventExtensions", () => { const x = Object.preventExtensions({ a: 1 }); delete x.a; const out = []; try { x.b = 1 } catch (e) { out.push(e.message) } return [out, x, Object.isExtensible(x), Object.isSealed(x), Object.isFrozen(x)] })
t("empty non-extensible", () => { const x = Object.preventExtensions({}); return [Object.isSealed(x), Object.isFrozen(x)] })
t("primitives", () => [Object.freeze(1), Object.seal("s"), Object.preventExtensions(null), Object.isFrozen(1), Object.isSealed("x"), Object.isExtensible(1)])
t("freeze map", () => { const m = Object.freeze(new Map()); m.set("a", 1); return [m.size, Object.isFrozen(m)] })
t("freeze bytes", () => Object.freeze(new Uint8Array(2)))
t("seal bytes", () => Object.seal(new Uint8Array(1)))
t("freeze empty bytes", () => [Object.isFrozen(Object.freeze(new Uint8Array(0))), Object.isExtensible(Object.preventExtensions(new Uint8Array(3)))])
t("freeze function", () => { const f = () => 1; Object.freeze(f); try { f.x = 1 } catch (e) { return [e.message, Object.isFrozen(f)] } })
t("frozen read-only builtin", () => { try { Math.PI = 3 } catch (e) { return e.message } })
t("prototype methods", () => { const x = { a: 1 }; return [x.hasOwnProperty("a"), x.hasOwnProperty("b"), Object.prototype.isPrototypeOf.call(Object.prototype, x), Array.prototype.isPrototypeOf([]), x.propertyIsEnumerable("a"), [1].propertyIsEnumerable("length"), x.valueOf() === x] })
t("hasOwnProperty receiver", () => Object.prototype.hasOwnProperty.call("abc", 0))
t("toString tags", () => [{}, [], "s", 1, true, null, undefined, () => 1, new Date(0), /a/, new Map(), new Set(), new WeakMap(), new WeakSet(), new Error("e"), Promise.resolve(1), new URL("http://a.b"), new URLSearchParams(), new Headers(), new Uint8Array(1), [].values(), (function* () {})(), Math, JSON].map((v) => Object.prototype.toString.call(v)))
t("arguments tag", () => { function f() { return Object.prototype.toString.call(arguments) } return f() })
t("toLocaleString", () => [{ toString() { return "TS" } }.toLocaleString(), [1, 2].toLocaleString(), Object.prototype.toLocaleString.call(5)])
t("valueOf null", () => Object.prototype.valueOf.call(null))
t("valueOf primitive", () => Object.prototype.valueOf.call(7))
t("in operator", () => ["a" in { a: 1 }, "toString" in {}, 0 in [1], "length" in [], "x" in Object.create({ x: 1 })])
t("spread", () => ({ ...{ a: 1 }, ...[9, 8], ..."hi", ...null, ...5, b: 2 }))
t("proto key", () => { const x = { ["__proto__"]: 5 }; return [Object.keys(x), x.__proto__, Object.getPrototypeOf(x) === Object.prototype] })
t("integer key order", () => Object.keys({ b: 1, 2: 1, a: 1, 1: 1, "-1": 1, "01": 1 }))
t("computed keys", () => ({ [1 + 1]: "two", [null]: "n", [[1, 2]]: "arr", [{ toString() { return "obj" } }]: "o" }))
t("delete", () => { const x = { a: 1, b: 2 }; const d = delete x.a; return [d, x, delete x.zzz] })
t("function props", () => { function f(a, b) {} f.extra = 1; return [f.name, f.length, Object.keys(f), f.extra] })
t("function name readonly", () => { function f() {} f.name = "g"; return f.name })
return r`,
])

programs("json parse and stringify", [
  `t("parse", () => [JSON.parse("1"), JSON.parse('"s"'), JSON.parse("null"), JSON.parse("[1,{\\"a\\":true}]"), JSON.parse(" {\\"x\\": [] } ")])
t("parse invalid", () => JSON.parse("{bad"))
t("parse empty", () => JSON.parse(""))
t("parse non-string", () => [JSON.parse(5), JSON.parse(true), JSON.parse({ toString() { return "[7]" } })])
t("parse undefined", () => JSON.parse())
t("parse reviver", () => JSON.parse('{"a":1,"b":{"c":2,"d":[3,4]}}', (k, v) => (typeof v === "number" ? v * 10 : v)))
t("parse reviver delete", () => JSON.parse('{"a":1,"b":2}', (k, v) => (k === "a" ? undefined : v)))
t("parse reviver this", () => { const seen = []; JSON.parse('{"x":{"y":1}}', function (k, v) { seen.push(k + ":" + Object.keys(this).join("|")); return v }); return seen })
t("parse reviver root", () => JSON.parse("[1]", (k, v) => (k === "" ? "root" : v)))
t("parse non-function reviver", () => JSON.parse("[1]", 5))
t("parse proto key", () => { const x = JSON.parse('{"__proto__": {"p": 1}, "a": 2}'); return [Object.keys(x), x.p] })
t("stringify", () => [JSON.stringify(1), JSON.stringify("s"), JSON.stringify(null), JSON.stringify(undefined), JSON.stringify([undefined, () => 1, NaN, Infinity]), JSON.stringify({ a: undefined, b: () => 1, c: NaN, d: -0 })])
t("stringify nested", () => JSON.stringify({ a: [1, { b: "x" }], c: { d: null } }))
t("stringify indent number", () => JSON.stringify({ a: [1, 2], b: {} }, null, 2))
t("stringify indent string", () => JSON.stringify({ a: 1 }, null, "--"))
t("stringify indent big", () => JSON.stringify([1], null, 20))
t("stringify indent other", () => JSON.stringify([1], null, {}))
t("stringify replacer array", () => JSON.stringify({ a: 1, b: 2, c: { a: 3, d: 4 }, 1: "one" }, ["a", "c", 1, {}]))
t("stringify replacer function", () => JSON.stringify({ a: 1, b: "x", c: [1, 2] }, (k, v) => (typeof v === "number" ? v + 100 : v)))
t("stringify replacer drops", () => JSON.stringify({ a: 1, b: 2 }, (k, v) => (k === "b" ? undefined : v)))
t("stringify replacer this", () => { const keys = []; JSON.stringify({ o: { p: 1 } }, function (k, v) { keys.push(k + "@" + (this === undefined ? "u" : Object.keys(this).join(","))); return v }); return keys })
t("stringify replacer root", () => JSON.stringify(5, (k, v) => (k === "" ? [v] : v)))
t("stringify toJSON", () => JSON.stringify({ a: { toJSON(key) { return "J:" + key } }, d: new Date(0), u: new URL("http://x.y/z") }))
t("stringify toJSON array", () => JSON.stringify([{ toJSON(k) { return k } }]))
t("stringify cyclic", () => { const a = { n: 1 }; const b = { a }; return JSON.stringify([b, b]) })
t("stringify promise", () => JSON.stringify({ p: Promise.resolve(1) }))
t("stringify map set", () => JSON.stringify({ m: new Map([[1, 2]]), s: new Set([1]), re: /a/, h: new Headers({ "X-A": "1" }), q: new URLSearchParams("a=1"), b: new Uint8Array([1, 2]) }))
t("stringify error", () => JSON.stringify({ e: new TypeError("bad"), f: Object.assign(new Error("x"), { code: 5 }) }))
t("stringify function", () => JSON.stringify(() => 1))
t("stringify symbol", () => JSON.stringify(Symbol.iterator))
t("stringify generator", () => JSON.stringify((function* () {})()))
t("stringify deep", () => { let x = 1; for (let i = 0; i < 40; i++) x = [x]; return JSON.stringify(x).length })
t("stringify too long", () => { const s = "x".repeat(9000000); return JSON.stringify([s, s]) })
t("stringify unicode", () => JSON.stringify("é\\u2028\\n\\u0001"))
t("JSON tag", () => [Object.prototype.toString.call(JSON), typeof JSON.parse, JSON.parse.length, JSON.stringify.length])
t("stringify callable", () => [1, 2].map(JSON.stringify))
return r`,
])

programs("program result boundary", [
  `return new Set([1, "a", [2]])`,
  `return { q: new URLSearchParams({ a: "1", b: "x y" }), h: new Headers([["X-One", "1"]]), d: new Date(Date.UTC(2020, 1, 3)), m: new Map([[1, 2]]), re: /x/g, u: new URL("https://example.com/a?b=c#d") }`,
  `return new Uint8Array([1, 2, 3])`,
  `return { nested: { bytes: new Uint8Array(1) } }`,
  `return Promise.resolve({ v: Promise.resolve(42) })`,
  `return Object.assign(new RangeError("out"), { extra: [1] })`,
  `const x = { ["__proto__"]: { polluted: true }, ok: 1 }; return x`,
  `const a = { n: 1 }; return [a, a, { a }]`,
  `let x = { v: 0 }; for (let i = 0; i < 40; i++) x = { x }; return x`,
  `return { f() {}, u: undefined, n: NaN, i: -Infinity, z: -0, arr: [undefined, () => 1] }`,
  `return { toJSON() { return { replaced: true } } }`,
  `return undefined`,
  `const g = (function* () { yield 1 })(); return { g, it: [1].values(), wm: new WeakMap(), ws: new WeakSet(), sym: Symbol.iterator }`,
  `"final expression " + (1 + 2)`,
  `const value = 5`,
  `return Promise.reject(new TypeError("rejected at the boundary"))`,
  `return { p: Promise.reject(new Error("nested rejection")) }`,
  `return { toJSON() { throw new Error("toJSON failed") } }`,
  `return tools`,
  `return Math.max`,
])
