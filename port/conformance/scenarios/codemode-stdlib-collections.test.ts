// Code-mode standard library: Map, Set, WeakMap, WeakSet, and the Iterator helpers, on the legacy interpreter and
// the bytecode VM.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

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

programs("map and set", [
  `t("Map basics", () => { const m = new Map([["a", 1], [2, "b"]]); m.set(NaN, "nan").set(-0, "zero"); return [m.size, m.get("a"), m.get(2), m.get(NaN), m.get(0), m.has("z"), m.delete("a"), m.delete("a"), m.size, [...m.keys()], [...m.values()], [...m.entries()], [...m]] })
t("Map object keys", () => { const k = {}; const m = new Map([[k, 1]]); return [m.get(k), m.get({}), m.has(k)] })
t("Map clear", () => { const m = new Map([[1, 1]]); const c = m.clear(); return [m.size, c === undefined] })
t("Map forEach", () => { const out = []; new Map([["x", 1], ["y", 2]]).forEach(function (v, k, map) { out.push(k + "=" + v + ":" + map.size + ":" + this.tag) }, { tag: "T" }); return out })
t("Map forEach bad", () => new Map().forEach(1))
t("Map getOrInsert", () => { const m = new Map([["a", 1]]); return [m.getOrInsert("a", 5), m.getOrInsert("b", 6), [...m]] })
t("Map getOrInsertComputed", () => { const m = new Map([["a", 1]]); const seen = []; const v1 = m.getOrInsertComputed("a", (k) => { seen.push(k); return 9 }); const v2 = m.getOrInsertComputed(-0, (k) => { seen.push(Object.is(k, -0)); return "z" }); return [v1, v2, seen, [...m.keys()].map(String)] })
t("Map getOrInsertComputed bad", () => new Map().getOrInsertComputed("a", 5))
t("Map without new", () => Map())
t("Map bad init", () => new Map(5))
t("Map bad entry", () => new Map([1]))
t("Map null init", () => new Map(null).size)
t("Map from generator", () => { function* g() { yield ["k", 1] } return [...new Map(g())] })
t("Map iterator", () => { const it = new Map([[1, 2]]).entries(); return [it.next(), it.next()] })
t("Map size receiver", () => { const proto = Map.prototype; try { return proto.size } catch (e) { return e.message } })
t("Map method receiver", () => Map.prototype.get.call({}, 1))
t("Map groupBy", () => [...Map.groupBy([1, 2, 3, 4], (x) => (x > 2 ? "big" : "small"))])
t("Map groupBy object keys", () => { const odd = { n: "odd" }; const even = { n: "even" }; const g = Map.groupBy([1, 2, 3], (x) => (x % 2 ? odd : even)); return [g.get(odd), g.get(even)] })
t("Map groupBy bad", () => Map.groupBy(null, (x) => x))
t("Map symbol iterator", () => Map.prototype[Symbol.iterator] === Map.prototype.entries)
t("Map tags", () => [Map.name, Map.length, typeof Map.prototype.set, new Map() instanceof Map])
t("Map spread into array", () => [...new Map([["a", [1]]])])
t("Map destructure", () => { const [[k, v]] = new Map([["key", "val"]]); return k + v })
return r`,
  `t("Set basics", () => { const s = new Set([1, 2, 2, "2", NaN, NaN]); s.add(3).add(1); return [s.size, s.has(2), s.has("3"), s.has(NaN), s.delete(2), s.delete(2), [...s], [...s.keys()], [...s.values()], [...s.entries()]] })
t("Set from string", () => [...new Set("hello")])
t("Set clear", () => { const s = new Set([1]); s.clear(); return s.size })
t("Set forEach", () => { const out = []; new Set(["a", "b"]).forEach(function (v, k, set) { out.push(v + k + set.size + this.x) }, { x: "!" }); return out })
t("Set without new", () => Set([1]))
t("Set bad init", () => new Set(5))
t("Set null init", () => new Set(undefined).size)
t("Set receiver", () => Set.prototype.add.call(new Map(), 1))
t("Set size receiver", () => Set.prototype.size)
const A = new Set([1, 2, 3, 4])
const B = new Set([3, 4, 5])
const small = new Set([3])
t("union", () => [...A.union(B)])
t("intersection", () => [[...A.intersection(B)], [...small.intersection(A)], [...A.intersection(small)]])
t("difference", () => [[...A.difference(B)], [...small.difference(A)], [...A.difference(small)]])
t("symmetricDifference", () => [...A.symmetricDifference(B)])
t("isSubsetOf", () => [small.isSubsetOf(A), A.isSubsetOf(small), new Set([9]).isSubsetOf(A)])
t("isSupersetOf", () => [A.isSupersetOf(small), small.isSupersetOf(A), A.isSupersetOf(new Set([1, 9]))])
t("isDisjointFrom", () => [A.isDisjointFrom(new Set([7])), A.isDisjointFrom(B), small.isDisjointFrom(A), A.isDisjointFrom(new Set([9]))])
t("set ops with Map", () => [[...A.union(new Map([[10, "x"]]))], [...new Set([1]).intersection(new Map([[1, "a"], [2, "b"]]))], new Set([2]).isSubsetOf(new Map([[2, 0], [3, 0]]))])
t("small disjoint", () => [new Set([1]).isDisjointFrom(new Set([7, 8])), new Set([1]).isDisjointFrom(new Set([1, 8]))])
t("set-like object", () => { const like = { size: 2, has(x) { return x === 1 || x === 9 }, keys() { return [1, 9].values() } }; return [[...A.intersection(like)], [...A.union(like)], A.isSupersetOf(like), [...new Set([1]).difference(like)], A.isDisjointFrom(like), new Set([1, 2, 3, 4, 5]).isDisjointFrom(like), [...A.symmetricDifference(like)], new Set([1]).isSubsetOf(like)] })
t("set-like array keys", () => { const like = { size: 1, has() { return false }, keys() { return [5] } }; return [...A.union(like)] })
t("set-like generator keys", () => { const like = { size: 1, has() { return true }, *keys() { yield 8 } }; return [...A.union(like)] })
t("set-like bad size", () => A.union({ size: "x", has() {}, keys() {} }))
t("set-like missing methods", () => A.union({ size: 1 }))
t("set ops non-object", () => A.union(5))
t("set-like intersection large", () => { const like = { size: 1, has() { return true }, keys() { return [2, 99].values() } }; return [...A.intersection(like)] })
t("set-like difference large", () => { const like = { size: 1, has() { return true }, keys() { return [2].values() } }; return [...A.difference(like)] })
t("Set spread", () => [...new Set([[1], [1]])].length)
return r`,
  `t("WeakMap", () => { const k = {}; const k2 = []; const w = new WeakMap([[k, "v"]]); w.set(k2, 2); return [w.get(k), w.get(k2), w.get({}), w.get(1), w.has(k), w.has(1), w.delete(k), w.has(k), w.delete(5)] })
t("WeakMap bad key", () => new WeakMap().set("str", 1))
t("WeakMap bad init", () => new WeakMap(5))
t("WeakMap bad entry", () => new WeakMap([1]))
t("WeakMap bad entry key", () => new WeakMap([["a", 1]]))
t("WeakMap null init", () => typeof new WeakMap(null))
t("WeakMap without new", () => WeakMap())
t("WeakMap getOrInsert", () => { const k = {}; const w = new WeakMap(); return [w.getOrInsert(k, 1), w.getOrInsert(k, 2)] })
t("WeakMap getOrInsertComputed", () => { const k = {}; const w = new WeakMap(); return [w.getOrInsertComputed(k, (key) => key === k), w.getOrInsertComputed(k, () => "never")] })
t("WeakMap getOrInsert bad key", () => new WeakMap().getOrInsert(1, 2))
t("WeakMap tool key", () => new WeakMap().set(tools, 1))
t("WeakMap set returns", () => { const w = new WeakMap(); return w.set({}, 1) === w })
t("WeakSet", () => { const k = {}; const s = new WeakSet([k]); return [s.has(k), s.has({}), s.has(1), s.delete(k), s.has(k), s.add(k) === s, s.has(k)] })
t("WeakSet bad value", () => new WeakSet().add(1))
t("WeakSet bad init", () => new WeakSet(1))
t("WeakSet bad item", () => new WeakSet([1]))
t("WeakSet without new", () => WeakSet())
t("WeakSet null", () => typeof new WeakSet(null))
t("weak receivers", () => [(() => { try { return WeakMap.prototype.get.call({}, {}) } catch (e) { return e.message } })(), (() => { try { return WeakSet.prototype.has.call(new Set(), 1) } catch (e) { return e.message } })()])
return r`,
])

programs("iterator helpers", [
  `const it = () => [1, 2, 3, 4, 5].values()
t("map", () => it().map((x, i) => x * 10 + i).toArray())
t("filter", () => it().filter((x) => x % 2).toArray())
t("take", () => it().take(2).toArray())
t("take zero", () => it().take(0).toArray())
t("take negative", () => it().take(-1))
t("take NaN", () => it().take("x"))
t("drop", () => [it().drop(3).toArray(), it().drop(10).toArray()])
t("drop negative", () => it().drop(-2))
t("flatMap", () => it().take(3).flatMap((x) => [x, x]).toArray())
t("flatMap string", () => it().take(1).flatMap(() => "ab").toArray())
t("flatMap iterator", () => it().take(2).flatMap((x) => new Set([x, -x]).values()).toArray())
t("flatMap non-iterable", () => it().flatMap((x) => x).toArray())
t("reduce", () => [it().reduce((a, b) => a + b), it().reduce((a, b) => a + b, 100), it().reduce((a, b, i) => a + i, "")])
t("reduce empty", () => [].values().reduce((a, b) => a + b))
t("toArray", () => new Set(["a", "b"]).values().toArray())
t("forEach", () => { const out = []; const ret = it().forEach((x, i) => out.push(x * i)); return [out, ret === undefined] })
t("some/every/find", () => [it().some((x) => x > 4), it().some((x) => x > 5), it().every((x) => x > 0), it().every((x) => x < 3), it().find((x) => x > 2), it().find((x) => x > 9)])
t("next", () => { const i = it().map((x) => x * 2); return [i.next(), i.next().value] })
t("return closes", () => { const i = it().map((x) => x); const ret = i.return(); return [ret, i.next()] })
t("lazy", () => { const log = []; const i = it().map((x) => { log.push(x); return x }); i.next(); return log })
t("callback throws", () => it().map(() => { throw new Error("boom") }).toArray())
t("filter throws", () => it().filter(() => { throw new Error("f") }).toArray())
t("non-callable", () => it().map(1))
t("reentrant", () => { let inner; inner = it().map((x) => inner.next()); return inner.next() })
t("Iterator.from array", () => Iterator.from([7, 8]).toArray())
t("Iterator.from string", () => Iterator.from("hi").map((c) => c.toUpperCase()).toArray())
t("Iterator.from iterator", () => { const i = [1].values(); return Iterator.from(i) === i })
t("Iterator.from protocol", () => { let n = 0; return Iterator.from({ next() { n++; return { done: n > 2, value: n } } }).toArray() })
t("Iterator.from number", () => Iterator.from(5))
t("Iterator.from generator", () => { function* g() { yield 1; yield 2 } return Iterator.from(g()).map((x) => x * 3).toArray() })
t("Iterator()", () => Iterator())
t("new Iterator()", () => new Iterator())
t("Iterator.prototype", () => [typeof Iterator.prototype.map, Object.getPrototypeOf([].values()) === Iterator.prototype, [].values() instanceof Iterator])
t("generator helpers", () => { function* g() { yield 1; yield 2; yield 3 } return g().drop(1).map((x) => x + 1).toArray() })
t("closing source", () => { let closed = false; function* g() { try { yield 1; yield 2 } finally { closed = true } } const v = g().take(1).toArray(); return [v, closed] })
t("find closes", () => { let closed = 0; function* g() { try { yield 1; yield 2 } finally { closed++ } } g().find((x) => x === 1); g().some(() => true); g().every(() => false); return closed })
t("flatMap close", () => { let closed = 0; function* g() { try { yield 1; yield 2 } finally { closed++ } } const h = g().flatMap((x) => [x, x]); h.next(); h.return(); return closed })
t("next receiver", () => Iterator.prototype.next.call({}))
t("helper tag", () => Object.prototype.toString.call([].values().map((x) => x)))
return r`,
])
