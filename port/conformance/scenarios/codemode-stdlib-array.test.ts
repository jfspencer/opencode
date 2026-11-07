// Code-mode standard library: Array. Each program runs through the `execute` tool on the legacy interpreter and,
// with `experimental.codemode_vm`, on the bytecode VM; the result text is recorded in the next provider request.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

// `t(label, fn)` records a value, or the error a call throws, so one failing case does not hide the others.
const PRELUDE = `const r = {}
const show = (v) => v === undefined ? "<undefined>" : typeof v === "number" && (!Number.isFinite(v) || Object.is(v, -0)) ? "<" + (Object.is(v, -0) ? "-0" : String(v)) + ">" : v
const t = (k, f) => { try { r[k] = show(f()) } catch (e) { r[k] = "!" + (e && e.name) + ": " + (e && e.message) } }
`

function programs(name: string, codes: string[]) {
  for (const vm of process.env.CONFORMANCE_EXPERIMENTAL ? [false, true] : [false])
    scenario(
      `${name}${vm ? " (vm)" : ""}`,
      {
        config: vm ? { experimental: { codemode_vm: true } } : {},
        turns: [
          ...codes.map((code, index): Turn => ({
            events: [{ tool: "execute", input: { code: PRELUDE + code }, id: `call_${index}` }],
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

programs("array constructors and statics", [
  `t("Array()", () => Array())
t("Array(3)", () => { const a = Array(3); return [a.length, 0 in a, JSON.stringify(a), a.join("-")] })
t("new Array(1, 2)", () => new Array(1, 2))
t("Array('x')", () => Array("x"))
t("Array(-1)", () => Array(-1))
t("Array(1.5)", () => new Array(1.5))
t("isArray", () => [Array.isArray([]), Array.isArray({ length: 0 }), Array.isArray("a"), Array.isArray(Array.prototype)])
t("of", () => Array.of(7, "a", null))
t("of empty", () => Array.of())
t("from string", () => Array.from("héllo"))
t("from set", () => Array.from(new Set([1, 1, 2])))
t("from map", () => Array.from(new Map([["a", 1], ["b", 2]])))
t("from array-like", () => Array.from({ length: 3, 0: "a", 2: "c" }))
t("from array-like negative length", () => Array.from({ length: -2 }))
t("from mapper", () => Array.from([1, 2, 3], (x, i) => x * 10 + i))
t("from mapper thisArg", () => Array.from([1, 2], function (x) { return x + this.k }, { k: 5 }))
t("from undefined mapper", () => Array.from([1, 2], undefined))
t("from length", () => Array.from({ length: 4 }, (_, i) => i * i))
t("from generator", () => { function* g() { yield 1; yield 2 } return Array.from(g(), (x) => x + 1) })
t("from iterator protocol", () => Array.from({ [Symbol.iterator]() { let i = 0; return { next() { i++; return { done: i > 3, value: i } } } } }))
t("from number", () => Array.from(5))
t("from null", () => Array.from(null))
t("from promise", () => Array.from(Promise.resolve(1)))
t("from bad mapper", () => Array.from([1], 5))
t("from async generator", () => { async function* g() { yield 1 } return Array.from(g()) })
t("from map keys", () => Array.from(new Map([[1, "a"]]).keys()))
t("from arguments", () => { function f() { return Array.from(arguments) } return f(1, "b") })
t("prototype chain", () => [Object.getPrototypeOf([]) === Array.prototype, [].constructor === Array, Array.prototype.constructor.name, Array.name, Array.length])
t("method metadata", () => [Array.prototype.push.name, Array.prototype.push.length, Array.prototype.slice.length, Array.from.length])
return r`,
  `t("length growth", () => { const a = [1, 2]; a.length = 4; return [a.length, JSON.stringify(a), 3 in a] })
t("length truncation", () => { const a = [1, 2, 3]; a.length = 1; return a })
t("length invalid", () => { const a = [1]; a.length = -1; return a })
t("length fraction", () => { const a = [1]; a.length = 1.5; return a })
t("index write grows", () => { const a = []; a[3] = "x"; return [a.length, JSON.stringify(a)] })
t("non-index key", () => { const a = [1]; a.foo = "bar"; a["01"] = "z"; return [Object.keys(a), JSON.stringify(a), a.foo, a["01"]] })
t("holes", () => { const a = [1, , 3]; return [a.length, 1 in a, a.indexOf(undefined), a.includes(undefined), Object.keys(a)] })
t("delete index", () => { const a = [1, 2, 3]; delete a[1]; return [a.length, 1 in a, JSON.stringify(a)] })
t("delete length", () => { const a = [1]; return delete a.length })
t("spread holes", () => [...[1, , 3]])
t("for-in array", () => { const out = []; for (const k in ["a", "b"]) out.push(k); return out })
t("keys", () => [...["a", , "c"].keys()])
t("values", () => [...["a", "b"].values()])
t("entries", () => [...["a", "b"].entries()])
t("iterator next", () => { const it = [10, 20][Symbol.iterator](); return [it.next(), it.next(), it.next()] })
t("iterator identity", () => [Array.prototype[Symbol.iterator] === Array.prototype.values, typeof [].keys().next])
t("iterator string tag", () => Object.prototype.toString.call([].values()))
return r`,
])

programs("array access and copying", [
  `const a = [1, 2, 3, 4, 5]
t("at", () => [a.at(0), a.at(-1), a.at(9), a.at(), a.at("1"), a.at(1.7)])
t("slice", () => [a.slice(1, 3), a.slice(-2), a.slice(), a.slice(2, -1), a.slice("1"), a.slice(10)])
t("concat", () => a.concat([6, 7], 8, [[9]]))
t("concat empty", () => [].concat())
t("flat", () => [[1, [2, [3, [4]]]].flat(), [1, [2, [3, [4]]]].flat(2), [1, [2, [3, [4]]]].flat(Infinity), [1, [2]].flat(0)])
t("with", () => [a.with(0, "x"), a.with(-1, "y"), a])
t("with out of range", () => a.with(5, 1))
t("with negative out of range", () => a.with(-6, 1))
t("join", () => [a.join(), a.join(""), a.join(" - "), [null, undefined, 1].join(","), [[1, 2], [3]].join(";")])
t("join coercion", () => [1, 2].join({ toString() { return "|" } }))
t("toString", () => [[1, [2, 3]].toString(), String([1, null]), [] + "", [1, 2] + ""])
t("toLocaleString", () => [[1234.5, new Date(0), null, "s"].toLocaleString(), [].toLocaleString()])
t("toLocaleString custom", () => [{ toLocaleString() { return "L" } }].toLocaleString())
t("includes", () => [a.includes(3), a.includes(3, 3), a.includes("3"), [NaN].includes(NaN), a.includes(1, -5)])
t("indexOf", () => [a.indexOf(3), a.indexOf(3, 3), a.indexOf(9), [NaN].indexOf(NaN), a.indexOf(5, -1)])
t("lastIndexOf", () => [[1, 2, 1].lastIndexOf(1), [1, 2, 1].lastIndexOf(1, undefined), [1, 2, 1].lastIndexOf(1, -2), [1, 2, 1].lastIndexOf(7)])
t("toReversed", () => [a.toReversed(), a])
t("toSpliced", () => [a.toSpliced(), a.toSpliced(1), a.toSpliced(1, 2), a.toSpliced(1, 0, "x", "y"), a.toSpliced(-1, 1)])
t("toSorted", () => [[3, 1, 10, 2].toSorted(), [3, 1, 10, 2].toSorted((x, y) => x - y), ["b", , "a"].toSorted()])
t("on non-array receiver", () => Array.prototype.slice.call("abc", 1))
t("call on array", () => Array.prototype.slice.call([1, 2, 3], 1))
t("push via call", () => { const x = [1]; Array.prototype.push.call(x, 2, 3); return x })
return r`,
])

programs("array mutation", [
  `t("push/pop", () => { const a = [1]; const n = a.push(2, 3); const p = a.pop(); return [n, p, a, [].pop()] })
t("shift/unshift", () => { const a = [1, 2]; const n = a.unshift(-1, 0); const s = a.shift(); return [n, s, a, [].shift()] })
t("splice remove", () => { const a = [1, 2, 3, 4]; const removed = a.splice(1, 2); return [removed, a] })
t("splice insert", () => { const a = [1, 4]; a.splice(1, 0, 2, 3); return a })
t("splice negative", () => { const a = [1, 2, 3]; const removed = a.splice(-2); return [removed, a] })
t("splice no args", () => { const a = [1, 2]; return [a.splice(), a] })
t("splice huge count", () => { const a = [1, 2, 3]; return [a.splice(1, 99), a] })
t("splice negative count", () => { const a = [1, 2, 3]; return [a.splice(1, -1), a] })
t("fill", () => [[1, 2, 3, 4].fill(0), [1, 2, 3, 4].fill(9, 1, 3), [1, 2, 3].fill(7, -1), Array(3).fill("x")])
t("copyWithin", () => [[1, 2, 3, 4, 5].copyWithin(0, 3), [1, 2, 3, 4, 5].copyWithin(1, 0, 2), [1, 2, 3].copyWithin(-1)])
t("reverse", () => { const a = [1, 2, 3]; const b = a.reverse(); return [a, b === a] })
t("sort default", () => [3, 20, 100, 1].sort())
t("sort comparator", () => [3, 20, 100, 1].sort((x, y) => y - x))
t("sort strings", () => ["b", "a", "C", "B"].sort())
t("sort stable NaN", () => [{ k: 1, v: "a" }, { k: 0, v: "b" }, { k: 1, v: "c" }].sort(() => NaN).map((x) => x.v))
t("sort holes and undefined", () => { const a = [3, undefined, , 1]; a.sort(); return [a.length, 2 in a, 3 in a, JSON.stringify(a)] })
t("sort bad comparator", () => [2, 1].sort(5))
t("sort comparator throws", () => [2, 1].sort(() => { throw new Error("cmp") }))
t("sort coerces result", () => [1, 2, 3].sort(() => "1"))
t("large default sort", () => { const x = Array.from({ length: 5000 }, (_, i) => (i * 7919) % 5003); x[10] = undefined; delete x[20]; x.sort(); return [x.slice(0, 5), x.slice(-3).map(String), x.length, 4999 in x] })
t("large toSorted", () => { const x = Array.from({ length: 4500 }, (_, i) => String(4500 - i)).toSorted(); return [x[0], x[1], x[4499]] })
t("circular push", () => { const a = []; a.push(a); return a.length })
t("circular unshift", () => { const a = [1]; a.unshift(a); return a.length })
t("circular splice", () => { const a = [1]; a.splice(0, 0, a); return a.length })
t("circular fill", () => { const a = [1]; a.fill(a); return a.length })
t("nested circular", () => { const a = []; const b = [a]; a.push(b); return a.length })
return r`,
  `const frozen = Object.freeze([1, 2])
const sealed = Object.seal([1, 2])
const fixed = Object.preventExtensions([1, 2])
const tryAll = (label, arr) => {
  t(label + " push", () => arr.push(3))
  t(label + " pop", () => arr.pop())
  t(label + " shift", () => arr.shift())
  t(label + " unshift", () => arr.unshift(0))
  t(label + " splice", () => arr.splice(0, 1, 9))
  t(label + " fill", () => arr.fill(5))
  t(label + " sort", () => arr.sort())
  t(label + " reverse", () => arr.reverse())
  t(label + " copyWithin", () => arr.copyWithin(0, 1))
  t(label + " index write", () => { arr[0] = 42; return arr[0] })
  t(label + " index add", () => { arr[5] = 42; return arr.length })
  t(label + " length", () => { arr.length = 0; return arr.length })
  t(label + " state", () => [JSON.stringify(arr), Object.isFrozen(arr), Object.isSealed(arr), Object.isExtensible(arr)])
}
tryAll("frozen", frozen)
tryAll("sealed", sealed)
tryAll("non-extensible", fixed)
t("frozen single reverse", () => Object.freeze([1]).reverse())
t("frozen empty push nothing", () => Object.freeze([]).push())
t("empty non-extensible", () => [Object.isSealed(Object.preventExtensions([])), Object.isFrozen(Object.preventExtensions([]))])
return r`,
])

programs("array callbacks", [
  `const a = [1, 2, 3, 4]
t("map", () => a.map((x, i, arr) => x * i + arr.length))
t("map holes", () => { const m = [1, , 3].map((x) => x * 2); return [m.length, 1 in m, JSON.stringify(m)] })
t("map thisArg", () => a.map(function (x) { return x + this.d }, { d: 100 }))
t("map arrow ignores thisArg", () => a.map((x) => typeof this, { d: 1 }))
t("filter", () => a.filter((x) => x % 2 === 0))
t("filter holes", () => [1, , 3].filter(() => true))
t("find", () => [a.find((x) => x > 2), a.find((x) => x > 9), [, 1].find((x) => x === undefined)])
t("findIndex", () => [a.findIndex((x) => x > 2), a.findIndex((x) => x > 9)])
t("findLast", () => [a.findLast((x) => x < 3), a.findLast((x) => x > 9)])
t("findLastIndex", () => [a.findLastIndex((x) => x < 3), a.findLastIndex((x) => x > 9)])
t("some/every", () => [a.some((x) => x > 3), a.some((x) => x > 4), a.every((x) => x > 0), a.every((x) => x > 1), [].some(() => true), [].every(() => false), [, ,].every(() => false)])
t("forEach", () => { const out = []; const ret = [5, , 7].forEach((x, i) => out.push(i + ":" + x)); return [out, ret === undefined] })
t("forEach thisArg", () => { const c = { n: 0 }; [1, 2].forEach(function () { this.n++ }, c); return c.n })
t("reduce", () => [a.reduce((s, x) => s + x), a.reduce((s, x) => s + x, 10), a.reduce((s, x, i) => s + i, ""), [, 5].reduce((s, x) => s + x)])
t("reduce empty", () => [].reduce((s, x) => s + x))
t("reduce holes only", () => [, ,].reduce((s, x) => s + x))
t("reduceRight", () => [a.reduceRight((s, x) => s + x), ["a", "b", "c"].reduceRight((s, x) => s + x, ">")])
t("reduceRight empty", () => [].reduceRight((s, x) => s + x))
t("flatMap", () => [a.flatMap((x) => [x, x * 10]), a.flatMap((x) => x), [1, 2].flatMap((x) => [[x]]), [1, , 2].flatMap((x) => [x, , x])])
t("flatMap thisArg", () => [1].flatMap(function (x) { return [this.v] }, { v: "t" }))
t("non-callable", () => a.map(5))
t("missing callback", () => a.filter())
t("object callback", () => a.forEach({}))
t("callback throws", () => a.map((x) => { if (x === 3) throw new RangeError("three"); return x }))
t("builtin callbacks", () => [[-1, 2.5].map(Math.abs), [1, "2"].map(String), ["1", "x"].map(Number), ["10", "11"].map(parseInt), [0, 1, ""].filter(Boolean), [{ a: 1 }].map(JSON.stringify)])
t("Error as callback", () => ["m"].map(Error).map((e) => e.message))
t("Map as callback", () => [1].map(Map))
t("detached includes", () => ["a"].filter("abc".includes))
t("bound callback", () => [1, 2].map(function (x) { return x + this.k }.bind({ k: 3 })))
t("mutating during map", () => { const b = [1, 2, 3]; return b.map((x, i) => { if (i === 0) b.push(9); b[2] = 30; return x }) })
t("callback receives array", () => [7].map((x, i, arr) => arr === undefined ? "none" : arr.length))
return r`,
])
