// Code mode language core: operators, coercion, control flow, bindings, destructuring, functions. Each scenario
// runs a few `execute` programs in one session, on the default interpreter and on the bytecode VM
// (`experimental.codemode_vm`); the program results land in the next provider request.
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

// Catches a sub-case's throw so one failure doesn't hide the rest of the program's results.
const T = `const t = (f) => { try { return f() } catch (e) { return "throws " + (e && e.name) + ": " + (e && e.message) } }\n`

programs("arithmetic and comparison operators", [
  `return {
  add: [1 + 2, "a" + 1, 1 + "2", true + 1, null + 1, undefined + 1, [1] + [2], {} + "x", 0.1 + 0.2],
  sub: [5 - 2, "5" - 2, "x" - 1, null - 1, true - false],
  mul: [3 * 4, "3" * "4", -0 * 1, Infinity * 0],
  div: [7 / 2, 1 / 0, -1 / 0, 0 / 0, "8" / "2"],
  mod: [7 % 3, -7 % 3, 7 % -3, 5.5 % 2, 1 % 0],
  exp: [2 ** 10, 2 ** -1, (-2) ** 3, 2 ** 3 ** 2, 4 ** 0.5],
  cmp: [1 < 2, 2 <= 2, 3 > 4, 4 >= 4, "a" < "b", "10" < "9", "10" < 9, null < 1, undefined < 1, NaN < 1, NaN >= NaN],
  eq: [1 == "1", 0 == false, null == undefined, null == 0, NaN == NaN, [1] == 1, [1, 2] == "1,2", "" == 0, "0" == false],
  strict: [1 === 1, 1 === "1", null === undefined, NaN === NaN, 0 === -0, +0 !== -0, "a" !== "a"],
}`,
  `let a = 0b1010, b = 0o17, c = 0xff, d = 1_000_000, e = 1e3, f = .5
return {
  literals: [a, b, c, d, e, f, 5e-7, 2e21],
  bitwise: [a & b, a | b, a ^ b, ~a, a << 2, -16 >> 2, -16 >>> 28, 1 << 31, (2 ** 32 + 5) | 0, ~~3.7, 5.9 | 0],
  unary: [+"3", +"", +"x", -"4", +true, +null, +undefined, +[], +[5], +[1, 2], -(-1), !0, !!"", !!"a", !!{}, !![]],
  typeof: [typeof 1, typeof "s", typeof true, typeof undefined, typeof null, typeof {}, typeof [], typeof (() => 1), typeof Math, typeof JSON.parse, typeof notDeclared, typeof Symbol.iterator],
  void: [void 0, void "x"],
  comma: (1, 2, 3),
}`,
  T +
    `const o = { valueOf() { return 7 }, toString() { return "seven" } }
const s = { toString() { return "str" } }
const order = []
const l = { valueOf() { order.push("l"); return 1 } }
const r = { valueOf() { order.push("r"); return 2 } }
const res = {
  mulObj: o * 2, plusObj: o + 1, tmpl: \`\${o}\`, strConcat: "" + s, cmp: o > 6, neg: -o, tilde: ~o,
  number: Number(o), string: String(o), isNaN: isNaN(s), parseInt: parseInt({ toString() { return "42px" } }),
  sum: l + r, order,
  arrToStr: (() => { const arr = [1, 2]; arr.toString = () => "x"; return arr + "" })(),
  throwing: t(() => ({ valueOf() { throw new TypeError("no value") } }) * 1),
  loose: [o == 7, s == "str", null == false, undefined == null],
  fnCoerce: [String(() => 1), \`\${Math.max}\`, isNaN(() => 1)],
  fnBinary: t(() => (() => 1) + 1),
  fnUnary: t(() => -(() => 1)),
  key: (() => { const x = {}; x[{ toString() { return "id" } }] = 1; x[null] = 2; x[true] = 3; x[[1, 2]] = 4; return x })(),
  objOps: (() => { const n = { valueOf() { return 6 } }; return [n % 4, n ** 2, n <= 6, n >= 7, n & 3, n | 1, n ^ 2, n << 1, n >> 1, n >>> 1, n - 1, n / 2, 1 - n, n < 7, 2 ** n, 13 % n] })(),
  strObj: (() => { const s = { toString() { return "b" } }; return [s < "c", s <= "a", s > "a", s >= "b", s + s, "a" < s] })(),
  nullCmp: [({}) == null, null == ({}), undefined == ({}), ({}) != null, ({}) == "[object Object]", [] == false, [0] == false],
  dates: [new Date(5) - new Date(2), new Date(0) + 1, new Date(0) < new Date(1), +new Date(7)],
  compound: (() => { let x = 1; x += { valueOf() { return 2 } }; let y = { valueOf() { return 3 } }; y *= 2; let z = "s"; z += [1, 2]; let w = 10; w -= (() => 4)(); let v; v = (() => "called")(); return [x, y, z, w, v] })(),
  strCmp: ["abc" < "abd", "B" < "a", "a" <= "a", "b" >= "c", "10" >= "9", "x" > "X"],
  mixed: [1 <= "1", "2" >= 3, null >= 0, undefined <= 0, true > false, "5" * [2], [3] - 1, "4" ** "2", "9" % "4", "6" & "3", "6" | 1, "6" ^ "2", "1" << "3", "-16" >> 2, "-16" >>> 30],
}
res`,
])

programs("logical, nullish, optional chaining and assignment operators", [
  T +
    `const calls = []
const f = (v) => { calls.push(v); return v }
const out = {
  and: [f(0) && f(1), f(1) && f(2), "" && "x"],
  or: [f(0) || f(3), f(4) || f(5), null || undefined],
  nullish: [null ?? "d", undefined ?? "d", 0 ?? "d", "" ?? "d", false ?? "d"],
  not: [!null, !undefined, !NaN],
  calls,
}
const o = { a: { b: { c: 1 } }, fn() { return "called" }, arr: [10, 20] }
out.opt = [o?.a?.b?.c, o.x?.y, o.x?.y.z.w, o.fn?.(), o.nofn?.(), o.arr?.[1], o.missing?.[0], null?.x, undefined?.(), (o?.a).b.c]
out.optDelete = (() => { const x = { y: { z: 1 } }; const r = [delete x?.y.z, delete x.q?.r]; return [r, x] })()
out.optThrow = t(() => o.x.y)
out.callNonFn = t(() => o.a())
out.nullRead = t(() => null.x)
out
`,
  T +
    `let a = 1
a += 2; a -= 1; a *= 10; a /= 4; a %= 3; a **= 3
let b = 6
b &= 3; b |= 8; b ^= 1; b <<= 2; b >>= 1; b >>>= 1
let s = "x"
s += 1
let n = null, z = 0, y = 5
n ??= "filled"; z ||= "or"; y &&= "and"
let q = 1
q ??= (() => { throw new Error("not evaluated") })()
const o = { count: 0, list: [1, 2, 3], nested: { v: 1 } }
o.count += 5
o["count"] *= 2
o.list[1] += 100
o.nested.v ||= 9
o.missing ??= "set"
const key = "dyn"
o[key] = 1
o[key + "2"] ??= 2
let i = 0
const pre = ++i, post = i++, preDec = --i, postDec = i--
o.count++
++o.list[0]
o["nested"].v--
const arr = [1, 2]
arr[5] = 6
arr.length = 3
return { a, b, s, n, z, y, q, o, pre, post, preDec, postDec, i, arr, len: arr.length,
  undeclared: t(() => { undeclaredVar = 1 }),
  readUndeclared: t(() => undeclaredVar2 + 1),
  updateFn: t(() => { let f = () => 1; f++ }),
  chained: (() => { let x, y2; x = y2 = 3; return [x, y2] })(),
}`,
  T +
    `const o = { a: 1, b: { c: 2 } }
const arr = [1, 2, 3]
const proto = { inherited: true }
const child = Object.create(proto)
child.own = 1
return {
  in: ["a" in o, "z" in o, 0 in arr, 5 in arr, "length" in arr, "inherited" in child, "toString" in o, "own" in child],
  inPrimitive: t(() => "a" in "abc"),
  instanceof: [arr instanceof Array, arr instanceof Object, o instanceof Array, new Error("x") instanceof Error, new TypeError("x") instanceof Error, (() => 1) instanceof Function, new Map() instanceof Map],
  instanceofBad: t(() => o instanceof o),
  delete: (() => { const x = { a: 1, b: 2 }; const r = delete x.a; const r2 = delete x["b"]; const r3 = delete x.none; return [r, r2, r3, x] })(),
  deleteIndex: (() => { const x = [1, 2, 3]; delete x[1]; return [x.length, 1 in x, x] })(),
  deleteLength: t(() => { const x = [1]; delete x.length }),
  deleteVar: t(() => { let v = 1; return delete v }),
  deleteNonRef: [delete 0],
  frozen: t(() => { const f = Object.freeze({ a: 1 }); f.a = 2 }),
  frozenAdd: t(() => { const f = Object.preventExtensions({ a: 1 }); f.b = 2 }),
  sealedDelete: t(() => { const f = Object.seal({ a: 1 }); delete f.a }),
  readOnly: t(() => { Math.PI = 3 }),
  fnName: t(() => { const f = function named() {}; f.name = "x" }),
}`,
])

programs("property access and writes", [
  T +
    `const out = {}
const proto = { inherited: 1, greet() { return "hi " + this.name } }
const child = Object.create(proto)
child.name = "child"
child.inherited = 2
out.inherited = [child.inherited, proto.inherited, child.greet(), Object.keys(child)]
out.primWrite = t(() => { "str".x = 1 })
out.numWrite = t(() => { const n = 5; n.prop = 1 })
out.boolWrite = t(() => { true.x = 1 })
out.promiseRead = t(() => Promise.resolve(1).value)
out.promiseWrite = t(() => { Promise.resolve(1).x = 1 })
out.fnProps = (() => { const f = () => 1; f.x = 2; f["y"] = 3; f.x += 1; delete f.y; return [f.x, f.y, Object.keys(f), "x" in f] })()
out.nullComputed = t(() => { const k = "key"; return null[k] })
out.undefWrite = t(() => { undefined.x = 1 })
out.nullWriteComputed = t(() => { const k = "k"; null[k] = 1 })
out.instanceofArrow = t(() => ({}) instanceof (() => 1))
out.instanceofPrim = [1 instanceof Number, "s" instanceof String, null instanceof Object]
out.protoKey = (() => { const o = { __proto__: proto }; const p = {}; p.__proto__ = { x: 1 }; return [o.inherited, "greet" in o, p.x, Object.keys(p)] })()
out.arrayProps = (() => { const a = [1, 2]; a.foo = "bar"; a["01"] = "zero-one"; a["1"] = "one"; return [a, a.foo, a["01"], Object.keys(a), a.length, JSON.stringify(a)] })()
out.stringIndex = ["abc"[1], "abc"["length"], "abc"[5], "abc".at(-1), "abc"[-1]]
out.numberMethods = [(255).toString(16), 1.5.toFixed(1), (5).constructor === Number, true.toString(), (12).toString.name]
out.chained = [1, 2, 3, 4].map((x) => x * 2).filter((x) => x > 2).reduce((a, b) => a + b, 0)
out.computedCall = (() => { const o = { k: function () { return this === o } }; const key = "k"; return [o[key](), o?.[key]?.(), o.k?.()] })()
out.literalKeys = { 1: "a", "1.5": "b", "-1": "c", 0x10: "hex", [2 + 3]: "computed", a: 1, a: 2, "quoted key": true }
out.methods = (() => { const o = { async am() { return 1 }, *gm() { yield 1 }, async *agm() { yield 1 }, arrow: () => 1, ["comp" + 1]() { return "c" } }; return [typeof o.am, typeof o.gm, typeof o.agm, o.comp1(), o.gm.name, o.agm.name, o.am.name] })()
out.deleteOptional = (() => { const o = { a: { b: 1 } }; return [delete o?.a.b, delete o?.zz, o] })()
out.deleteComputed = (() => { const o = { k1: 1, k2: 2 }; const k = "k"; return [delete o[k + 1], o] })()
out.deleteArrayLen = t(() => { const a = [1]; return delete a["length"] })
out.toolsShape = [typeof tools, String(tools)]
out.globalObjects = [typeof globalThis, typeof Object, typeof Iterator, Object.prototype.toString.call(null), Object.prototype.toString.call(undefined)]
out.superCall = t(() => ({ m() { return 1 } }).m())
out.chainNames = (() => { const o = { f: () => ({ g: 1 }), arr: [() => ({})] }; return [t(() => o.f().g()), t(() => o.f().h.i()), t(() => o["f"]()["missing"]()), t(() => o.arr[0]().x()), t(() => (0, o.f)().g()), t(() => o.f()[1 + 1]())] })()
out.stringIndexWrite = t(() => { const s = "abc"; s[0] = "z" })
out.symbolProp = t(() => Symbol.iterator.description)
out.nativeAccessor = t(() => Object.create(Map.prototype).size)
out.noPrimitive = t(() => ({ valueOf() { return {} }, toString() { return {} } }) + 1)
out.nullProtoPrimitive = t(() => \`\${Object.create(null)}\`)
out.localeNull = t(() => Object.prototype.toLocaleString.call(null))
out.localeArray = [1, null, "x", undefined, { toLocaleString() { return "L" } }].toLocaleString()
out.wideTemplate = ((strings, ...values) => [strings.length, values.length, values[255]])\`${Array.from({ length: 256 }, (_, i) => "${" + i + "}").join("-")}\`
out
`,
  `const o = { m() { return super.toString() } }
o.m()`,
  `const { [("dyn" + "Key")]: picked, ...others } = { dynKey: 1, keep: 2, also: 3 }
const m = await import("url", { with: {} })
return { picked, others, m: Object.keys(m).sort() }`,
  `function inner() {
  import { URL } from "url"
  return URL
}
inner()`,
  `new Array(${Array.from({ length: 300 }, (_, i) => i).join(", ")}).length`,
])

programs("control flow statements", [
  `const out = []
for (let i = 0; i < 10; i++) {
  if (i === 2) continue
  if (i === 6) break
  if (i % 2 === 0) out.push("even" + i)
  else if (i === 3) out.push("three")
  else out.push("odd" + i)
}
let w = 0
while (w < 3) w++
let d = 10
do { d-- } while (d > 7)
let once = 0
do once++; while (false)
for (;;) { out.push("forever-once"); break }
let k = 0
for (let a = 0, b = 10; a < b; a += 3, b -= 3) k++
const sw = (v) => {
  const seen = []
  switch (v) {
    case 1: seen.push("one")
    case 2: seen.push("two"); break
    case "x": { seen.push("x"); break }
    default: seen.push("default")
    case 3: seen.push("three")
  }
  return seen
}
const sw2 = (v) => { switch (v) { case 1: return "a" } return "none" }
let empty = 0
;;;
{ }
if (true) ; else empty = 1
return { out, w, d, once, k, sw: [sw(1), sw(2), sw("x"), sw(3), sw(9)], sw2: [sw2(1), sw2(2)], empty }`,
  `const result = []
outer: for (let i = 0; i < 3; i++) {
  inner: for (let j = 0; j < 3; j++) {
    if (j === 1) continue outer
    if (i === 2) break outer
    result.push([i, j])
  }
}
let count = 0
block: {
  count++
  if (count) break block
  count = 100
}
loop: while (true) {
  do {
    count++
    if (count > 3) break loop
    continue loop
  } while (true)
}
const fn = () => { lbl: for (const x of [1, 2]) { for (const y of [1, 2]) { if (y === 2) continue lbl; if (x === 2) return "ret" + x } } return "end" }
sw: switch (1) { case 1: for (;;) { break sw } }
return { result, count, fn: fn() }`,
  `const obj = { b: 1, a: 2, 10: "ten", 2: "two" }
const keys = []
for (const k in obj) keys.push(k)
const arrKeys = []
for (const k in ["x", "y"]) arrKeys.push(k)
const strKeys = []
for (const k in "hi") strKeys.push(k)
const proto = { inherited: 1, shadow: 1 }
const child = Object.create(proto)
child.own = 1
child.shadow = 2
const chain = []
for (const k in child) chain.push(k)
const none = []
for (const k in null) none.push(k)
for (const k in 5) none.push(k)
let target = {}
for (target.last in { p: 1, q: 2 });
let lastKey
for (lastKey in { m: 1 });
const idx = []
let n = 0
for (idx[n++] in { u: 1, v: 2 });
const [first] = ["unused"]
const destr = []
for (const [c0, c1] of Object.entries({ ab: 1 })) destr.push(c0, c1)
for ({ x: target.fromPattern } of [{ x: "px" }]);
let varLeak
for (var vi = 0; vi < 2; vi++) varLeak = vi
return { keys, arrKeys, strKeys, chain, none, target, lastKey, idx, first, destr, vi, varLeak }`,
  `const out = {}
const iter = (it) => { const r = []; for (const v of it) r.push(v); return r }
out.array = iter([1, , 3])
out.string = iter("a😀b")
out.map = iter(new Map([["k", 1], ["j", 2]]))
out.set = iter(new Set([3, 3, 4]))
out.params = iter(new URLSearchParams("a=1&b=2"))
out.entries = iter([7, 8].entries())
out.keys = iter(new Map([["x", 1]]).keys())
const custom = { [Symbol.iterator]() { let i = 0; return { next: () => (i < 3 ? { value: i++, done: false } : { value: "ret", done: true }) } } }
out.custom = iter(custom)
const closed = []
const closable = { [Symbol.iterator]() { let i = 0; return { next: () => ({ value: i++, done: false }), return: () => { closed.push("closed"); return {} } } } }
for (const v of closable) { if (v === 2) break }
try { for (const v of closable) { throw new Error("boom" + v) } } catch (e) { closed.push(e.message) }
const fnRet = () => { for (const v of closable) { return v } }
closed.push(fnRet())
out.closed = closed
const letCaptures = []
for (let i = 0; i < 3; i++) letCaptures.push(() => i)
out.letCaptures = letCaptures.map((f) => f())
const varCaptures = []
for (var j = 0; j < 3; j++) varCaptures.push(() => j)
out.varCaptures = varCaptures.map((f) => f())
const ofCaptures = []
for (const v of [1, 2]) ofCaptures.push(() => v)
out.ofCaptures = ofCaptures.map((f) => f())
try { for (const v of 5) {} } catch (e) { out.notIterable = e.message }
try { for (const v of { a: 1 }) {} } catch (e) { out.objNotIterable = e.message }
try { for (const v of { [Symbol.iterator]() { return { next: () => 1 } } }) {} } catch (e) { out.badResult = e.message }
return out`,
  `const log = []
const f1 = () => { try { log.push("try"); return "try-value" } finally { log.push("finally") } }
const f2 = () => { try { return "try" } finally { return "finally-wins" } }
const f3 = () => { try { throw new Error("x") } catch (e) { return "caught " + e.message } finally { log.push("f3 finally") } }
const f4 = () => { try { throw new Error("inner") } finally { log.push("f4 finally") } }
const f5 = () => { for (const i of [1, 2, 3]) { try { if (i === 2) break; continue } finally { log.push("loop finally " + i) } } return "after loop" }
const f6 = () => { try { try { throw new Error("deep") } finally { log.push("inner finally") } } catch (e) { return "outer caught " + e.message } }
const f7 = () => { try { throw 1 } catch { return "no binding" } }
const f8 = () => { try { throw { code: 42 } } catch ({ code }) { return code } }
const f9 = () => { try { throw new Error("first") } catch (e) { throw new Error("second") } finally { log.push("f9 finally") } }
const f10 = () => { try { return "t" } finally { log.push("no override") } }
let r4, r9
try { f4() } catch (e) { r4 = e.message }
try { f9() } catch (e) { r9 = e.message }
const nested = () => { let n = 0; while (true) { try { n++; if (n > 2) break } finally { n += 10 } } return n }
return { r1: f1(), r2: f2(), r3: f3(), r4, r5: f5(), r6: f6(), r7: f7(), r8: f8(), r9, r10: f10(), nested: nested(), log }`,
])

programs("bindings, hoisting and scope", [
  T +
    `const out = {}
out.varHoist = (() => { const before = typeof hoisted; var hoisted = 1; return [before, hoisted] })()
out.fnHoist = hoistedFn()
function hoistedFn() { return "hoisted" }
out.blockFn = (() => { { function inner() { return "inner" } return inner() } })()
out.tdz = t(() => { const r = later; let later = 1; return r })
out.tdzTypeof = t(() => { typeof tdzLater; let tdzLater = 1 })
out.constAssign = t(() => { const c = 1; c = 2 })
out.constUpdate = t(() => { const c = 1; c++ })
out.selfRef = t(() => { let x = x + 1 })
out.shadow = (() => { let x = "outer"; { let x = "inner"; } const f = () => { let x = "fn"; return x }; return [x, f()] })()
out.varRedeclare = (() => { var a = 1; var a = 2; return a })()
out.fnRedeclare = (() => { function g() { return 1 } function g() { return 2 } return g() })()
out.varInBlocks = (() => { if (true) { var inIf = "if" } try { var inTry = "try" } catch { } switch (1) { case 1: var inCase = "case" } return [inIf, inTry, inCase] })()
out.paramVar = ((p) => { var p; return p })("arg")
out.paramDefaultScope = ((a, b = () => a) => { var a = "body"; return b() })("param")
out.paramTdz = t(() => ((a = b, b = 1) => a)())
out.switchScope = t(() => { switch (2) { case 1: let s = "one"; case 2: s = "two"; return "assigned" } })
out.switchShared = (() => { switch (1) { case 1: let s = "one"; case 2: s += "+two"; return s } })()
out.switchTdz = t(() => { switch (2) { case 1: let s2 = "one"; break; case 2: return s2 } })
out.switchFnHoist = (() => { switch (1) { case 0: function sf() { return "hoisted in switch" } case 1: return sf() } })()
out.closureCounter = (() => { const mk = () => { let c = 0; return { inc: () => ++c, get: () => c } }; const a = mk(), b = mk(); a.inc(); a.inc(); b.inc(); return [a.get(), b.get()] })()
out.globalShadow = (() => { const Math = { max: () => "shadowed" }; return Math.max(1, 2) })()
out.undefinedRead = t(() => missingGlobal)
out.assignGlobalName = (() => { let JSON = 5; return JSON })()
out
`,
  `var top = "top-level var"
let topLet = "top-level let"
const search = "shadowed search"
const tools = "shadowed tools"
function outer() { return top + "/" + topLet }
;[outer(), search, tools, this, typeof globalThis, typeof window, typeof process, typeof require, typeof setTimeout]`,
  `"use strict"
const out = []
var x = 1
function dup(a, b) { return [a, b] }
out.push(dup(1, 2))
out`,
  `function dup(a, a) { return a }
const yield = "yield-ident"
;[dup(1, 2), yield]`,
  `"use strict"
function dup(a, a) { return a }
dup(1, 2)`,
])

programs("destructuring and spread", [
  T +
    `const { a, b: renamed, c = "default", d: { e } = { e: "nested default" }, ...rest } = { a: 1, b: 2, x: 9, y: 8 }
const [p, , q = 30, ...tail] = [10, 20, undefined, 40, 50]
const [[n1, n2], { deep: [d1] }] = [[1, 2], { deep: ["d"] }]
const key = "dyn"
const { [key]: computed, [key + "2"]: computed2 = "c2" } = { dyn: "computed!" }
const { length } = "abc"
const { toFixed } = 1
const {} = 1
const { 1: second, ...strRest } = "xyz"
const { constructor } = new Error("x")
const { slice } = [1]
const [s1, s2] = "hi"
const [m1, [mk, mv]] = new Map([["a", 1], ["b", 2]])
const [set1] = new Set(["only"])
let x1, y1
;[x1, y1] = [1, 2]
;[x1, y1] = [y1, x1]
const tgt = {}
;({ a: tgt.a, b: tgt["b"], c: tgt.c = "dflt" } = { a: "A", b: "B" })
const arr2 = []
;[arr2[0], arr2[1]] = ["i0", "i1"]
const fn = ({ a, b = 2 } = {}, [c2, d2] = [3, 4], ...more) => [a, b, c2, d2, more]
return {
  a, renamed, c, e, rest, p, q, tail, n1, n2, d1, computed, computed2, length, toFixedType: typeof toFixed, second, strRest,
  ctorIsError: constructor === Error, sliceType: typeof slice, s1, s2, m1, mk, mv, set1, x1, y1, tgt, arr2,
  fn: [fn(), fn({ a: 1 }, [5, 6], 7, 8)],
  nullDestructure: t(() => { const { z } = null }),
  undefDestructure: t(() => { const [z] = undefined }),
  notIterable: t(() => { const [z] = {} }),
  nestedNull: t(() => { const { a: { b } } = { a: null } }),
  defaultFnName: (() => { const { f = () => 1 } = {}; const [g = function () {}] = []; return [f.name, g.name] })(),
}`,
  `const closed = []
const it = (label) => ({ [Symbol.iterator]() { let i = 0; return { next: () => ({ value: i++, done: i > 5 }), return: () => { closed.push(label); return {} } } } })
const [a, b] = it("pattern")
const [c, ...rest] = it("rest-no-close")
const [] = it("empty")
try { const [x = (() => { throw new Error("default failed") })()] = [undefined] } catch (e) { closed.push(e.message) }
try { const [{ y }] = it("binding-failure") } catch (e) { closed.push("caught: " + e.constructor.name) }
const gen = function* () { try { yield 1; yield 2; yield 3 } finally { closed.push("gen finally") } }
const [g1] = gen()
const arr = [1, 2, 3]
const spread = {
  array: [0, ...arr, 4],
  string: [..."héllo"],
  set: [...new Set([1, 1, 2])],
  map: [...new Map([[1, "a"]])],
  holes: [...[1, , 3]],
  gen: [...gen()],
  call: Math.max(...arr, 10),
  callIter: ((...args) => args.length)(..."abcd"),
  object: { ...{ a: 1, b: 2 }, b: 3, ...null, ...undefined, ...5, ..."hi", ...[7, 8] },
  objectOrder: { z: 1, ...{ a: 2, z: 3 } },
}
return { a, b, c, rest, g1, closed, spread }`,
  `const tag = (strings, ...values) => ({ strings: [...strings], raw: strings.raw, values, sameRaw: Array.isArray(strings.raw) })
const name = "world"
const n = 5
const sites = []
const remember = (s) => { sites.push(s); return s }
for (let i = 0; i < 2; i++) remember\`site\`
const obj = { prefix: "P", tag(strings, v) { return this.prefix + strings[0] + v } }
return {
  plain: \`hello \${name}!\`,
  multiline: \`a
b\`,
  nested: \`outer \${\`inner \${n * 2}\`}\`,
  expr: \`\${n > 3 ? "big" : "small"} \${[1, 2]} \${{}} \${null} \${undefined} \${() => 1}\`,
  escapes: \`tab\\there \\u0041 \\x42 \\\` dollar \\\${not}\`,
  tagged: tag\`a\${1}b\${"two"}c\`,
  taggedEmpty: tag\`\`,
  rawEscape: tag\`line\\nnext \\unicode\`,
  memberTag: obj.tag\`-\${"v"}\`,
  sameSite: sites[0] === sites[1],
  stringRaw: String.raw\`x\\ny\${1 + 1}\`,
}`,
])

programs("functions, closures and this", [
  T +
    `function decl(a, b = a * 2, ...rest) { return [a, b, rest, arguments.length] }
const expr = function (x) { return x + 1 }
const named = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1) }
const arrow = (x) => x * 3
const arrowBlock = (x) => { return { x } }
const arrowObj = () => ({ literal: true })
const fib = (n) => (n < 2 ? n : fib(n - 1) + fib(n - 2))
const compose = (...fns) => (x) => fns.reduceRight((acc, f) => f(acc), x)
const curry = (a) => (b) => (c) => a + b + c
const props = function myName(a, b, c = 1, ...d) {}
props.custom = "prop"
const meta = {
  declName: decl.name, declLength: decl.length, exprName: expr.name, arrowName: arrow.name, propsLength: props.length, propsName: props.name,
  custom: props.custom, keys: Object.keys(props), anonName: (function () {}).name, arrowAnon: (() => {}).name,
  methodName: { m() {} }.m.name, assignedName: (() => { let late; late = () => 1; return late.name })(),
  computedName: { ["comp" + "uted"]: () => 1 }.computed.name,
  fnToString: Object.prototype.toString.call(decl), protoIsFn: Object.getPrototypeOf(decl) === Function.prototype,
}
const namedReadonly = t(() => { const f = function self() { self = 1 }; f() })
return {
  decl: [decl(1), decl(1, 5, 6, 7)], expr: expr(1), fact: named(5), arrow: arrow(2), arrowBlock: arrowBlock(1), arrowObj: arrowObj(),
  fib: fib(15), composed: compose((x) => x + 1, (x) => x * 2)(5), curry: curry(1)(2)(3), meta, namedReadonly,
  iife: (function () { return "iife" })(), iifeArrow: (() => "arrow iife")(),
  hasConstructor: decl.constructor === Function, fnCtor: t(() => Function("return 1")),
  newUser: t(() => new decl(1)), newArrow: t(() => new arrow(1)), newNumber: t(() => new Number(1)), newNonFn: t(() => new (5)()),
  callNonFn: t(() => (5)()), callUndefined: t(() => { const o = {}; o.missing() }),
}`,
  T +
    `const obj = {
  name: "obj",
  regular() { return String(this && this.name) },
  arrow: () => typeof this,
  nested() { const inner = () => this.name; return inner() },
  fnInside() { return typeof (function () { return this })() },
  counter: 0,
  inc() { this.counter++; return this },
}
const detached = obj.regular
const other = { name: "other" }
function show(greeting, punct) { return greeting + " " + (this && this.name) + punct }
const bound = show.bind(obj, "Hi")
const boundTwice = bound.bind(other, "?")
function argsFn() { return { len: arguments.length, json: JSON.stringify(arguments), str: String(arguments), spread: [...arguments], arr: Array.from(arguments), isArr: Array.isArray(arguments), first: arguments[0] } }
function shadowArgs(arguments) { return arguments }
function arrowArgs() { return (() => arguments[0])() }
return {
  regular: obj.regular(), computed: obj["regular"](), detached: detached(), comma: (0, obj.regular)(), arrow: obj.arrow(), nested: obj.nested(), fnInside: obj.fnInside(),
  chained: obj.inc().inc().counter,
  call: show.call(other, "Hello", "!"), apply: show.apply(obj, ["Hey", "."]), applyNull: show.apply(null), applyArrayLike: show.apply(other, { length: 2, 0: "A", 1: "B" }),
  bound: bound("!"), boundName: bound.name, boundLength: bound.length, boundTwice: boundTwice(), newBound: t(() => new bound()),
  builtinCall: Array.prototype.push.call([1], 2), mathApply: Math.max.apply(null, [1, 5, 3]), joinCall: Array.prototype.join.call(["a", "b"], "-"),
  args: argsFn(1, "two", [3]), shadowArgs: shadowArgs("mine"), arrowArgs: arrowArgs("outer arg"), topThis: typeof this,
  thisArg: (() => { const c = { n: 0 }; [1, 2].forEach(function () { this.n++ }, c); return c.n })(),
  applyBad: t(() => show.apply(obj, 5)),
}`,
  T +
    `let depth = 0
const recurse = () => { depth++; return recurse() }
const overflow = t(recurse)
const reachedDeep = depth > 1000
const mutual = { even: (n) => (n === 0 ? true : mutual.odd(n - 1)), odd: (n) => (n === 0 ? false : mutual.even(n - 1)) }
const deepOk = (n) => (n === 0 ? 0 : 1 + deepOk(n - 1))
const viaCallbacks = t(() => { const f = () => [1].map(f); f() })
return { overflow, reachedDeep, even: mutual.even(500), deep: deepOk(5000), viaCallbacks }`,
])

// Programs past the bytecode's narrow operand ranges: more than 255 call arguments, iterator and stack slots above 255
// (functions with hundreds of locals, objects with hundreds of keys), and a function body longer than 65535
// instructions (array-pattern elisions are cheap to write but expand to several instructions each).
const many = (prefix: string, count: number, value = (i: number) => String(i)) =>
  Array.from({ length: count }, (_, i) => `${prefix}${i} = ${value(i)}`).join(", ")
const locals = `let ${many("v", 300)}`

programs("wide operands", [
  `const f = (...a) => [a.length, a[0], a[299]]
const g = function (a0, a1) { return [arguments.length, a0, a1] }
const o = { m(...a) { return a.length } }
return [f(${Array.from({ length: 300 }, (_, i) => i).join(", ")}), g(${Array.from({ length: 260 }, (_, i) => `"s${i}"`).join(", ")}), o.m(${Array.from({ length: 256 }, () => "0").join(", ")}), Math.max(...[1, 2], ${Array.from({ length: 256 }, (_, i) => i).join(", ")})]`,
  `function* gen() {
  ${locals}
  for (const x of [1, 2]) yield x
  const [a, , b = 9, ...rest] = [3, 4, undefined, 5, 6]
  yield [a, b, rest]
  yield* [7, 8]
  const inner = function* () { const got = yield "inner"; return got }
  const r = yield* inner()
  yield r
  yield [...new Set([v0, v299])]
  const sub = function* () { yield "g1"; yield "g2"; return "gr" }
  for (const x of sub()) yield "of " + x
  const [g1, ...gRest] = sub()
  yield [g1, gRest, [...sub()]]
  const delegated = yield* sub()
  yield delegated
  for (const k in { p: 1 }) yield k
  try { for (const x of [1]) throw new Error("in loop") } catch (e) { yield e.message }
  outer: for (const x of [1, 2]) { for (const y of [1, 2]) { if (y === 2) continue outer; if (x === 2) break outer; yield [x, y] } }
  return v1 + v298
}
const it = gen()
const out = []
let step = it.next()
while (!step.done) { out.push(step.value); step = it.next(step.value === "inner" ? "sent" : undefined) }
out.push(step.value)
async function asyncMany() {
  ${locals}
  let total = 0
  for await (const x of [Promise.resolve(1), 2]) total += x
  total += await Promise.resolve(10)
  const [c] = await Promise.all([5])
  return total + c + v1
}
async function* agMany() {
  ${locals}
  yield* [1, 2]
  yield* (async function* () { yield 3 })()
  for await (const x of [4]) yield x
}
const agOut = []
for await (const x of agMany()) agOut.push(x)
const { ${Array.from({ length: 300 }, (_, i) => `k${i}`).join(", ")}, ...restKeys } = { ${many("k", 302).replaceAll(" = ", ": ")} }
const objOut = [k0, k299, restKeys]
const arrayOut = [${Array.from({ length: 300 }, (_, i) => i).join(", ")}, ...[1]].length
return { out, asyncMany: await asyncMany(), agOut, objOut, arrayOut }`,
])

programs("very long function body", [
  `function big(n) {
  const [${",".repeat(14000)}] = []
  let total = 0
  for (let i = 0; i < n; i++) {
    if (i % 2 === 0) continue
    total += i
    if (total > 20) break
  }
  while (total < 100) total *= 2
  do { total-- } while (total > 90)
  const pick = n > 3 ? "big" : "small"
  const both = (n && total) || "none"
  const fallback = null ?? n
  switch (n) { case 1: total = -1; break; case 5: total += 1000; default: total += 1 }
  let tried
  try { if (n > 4) throw new Error("thrown") } catch (e) { tried = e.message } finally { total++ }
  for (const x of [1, 2, 3]) { if (x === 2) continue; total += x }
  for (const k in { a: 1, b: 2 }) total += k.length
  const chained = { a: { b: null } }
  const opt = chained?.a?.b?.c ?? "short"
  let assigned = null
  assigned ??= "set"
  assigned &&= assigned + "!"
  const fn = () => total
  return { total: fn(), pick, both, fallback, tried, opt, assigned }
}
big(5)`,
])
