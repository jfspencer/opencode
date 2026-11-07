// Code mode language core: async functions, promises, generators and the iterator protocols, on the default
// interpreter and on the bytecode VM (`experimental.codemode_vm`).
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

programs("async functions and await ordering", [
  `const log = []
async function a() { log.push("a1"); await null; log.push("a2"); await undefined; log.push("a3"); return "A" }
async function b() { log.push("b1"); await 1; log.push("b2"); return "B" }
const arrow = async (x) => { await x; log.push("arrow " + x); return x * 2 }
const method = { async m() { return "method" } }
const pa = a(), pb = b()
log.push("sync after calls")
const results = [await pa, await pb, await arrow(21), await method.m()]
const thenable = { then(resolve) { log.push("thenable then"); resolve("from thenable") } }
const fromThenable = await thenable
const plain = await 42
const nested = await (async () => (async () => "deep")())()
let caught
try { await (async () => { throw new TypeError("async throw") })() } catch (e) { caught = e.name + ": " + e.message }
const rejectedThenable = await ({ then(_, reject) { reject(new Error("thenable rejected")) } }).catch?.((e) => e.message) ?? "no catch on plain object"
let caught2
try { await { then(_, reject) { reject("thenable reject value") } } } catch (e) { caught2 = e }
const throwingThenable = await (async () => { try { await { then() { throw new RangeError("then threw") } } } catch (e) { return e.message } })()
const typeofAsync = typeof a
const isPromise = pa instanceof Promise
return { log, results, fromThenable, plain, nested, caught, rejectedThenable, caught2, throwingThenable, typeofAsync, isPromise, str: Object.prototype.toString.call(pa) }`,
  `const order = []
const p = Promise.resolve(1)
p.then((v) => order.push("then1 " + v))
p.then((v) => order.push("then2 " + v))
order.push("sync")
await null
order.push("after await")
order.push(typeof queueMicrotask)
await Promise.resolve()
const chain = await Promise.resolve(1)
  .then((v) => v + 1)
  .then((v) => { throw new Error("chain err " + v) })
  .then(() => "skipped")
  .catch((e) => e.message)
  .finally(() => order.push("finally ran"))
const finallyKeeps = await Promise.resolve("kept").finally(() => "ignored")
const finallyOverride = await Promise.resolve("x").finally(() => { throw new Error("finally failed") }).catch((e) => e.message)
const finallyReject = await Promise.reject(new Error("orig")).finally(() => order.push("finally on reject")).catch((e) => e.message)
const thenNonFn = await Promise.resolve("passthrough").then(5, null)
const thenReject = await Promise.reject("r").then(null, (e) => "handled " + e)
const adopted = await Promise.resolve(1).then(() => Promise.resolve("adopted"))
const adoptedThenable = await Promise.resolve(1).then(() => ({ then: (r) => r("thenable adopted") }))
const selfP = Promise.resolve(1).then(() => selfP)
const selfResolution = await selfP.catch((e) => e.name + ": " + e.message)
return { order, chain, finallyKeeps, finallyOverride, finallyReject, thenNonFn, thenReject, adopted, adoptedThenable, selfResolution }`,
  `const log = []
const p = new Promise((resolve, reject) => { log.push("executor sync"); resolve("value"); resolve("ignored"); reject(new Error("ignored")) })
log.push("after construct")
const v = await p
const rejected = await new Promise((_, reject) => reject(new Error("rejected"))).catch((e) => e.message)
const execThrow = await new Promise(() => { throw new Error("executor threw") }).catch((e) => e.message)
const execThrowAfter = await new Promise((resolve) => { resolve("first"); throw new Error("ignored") })
let escaped
const later = new Promise((resolve) => { escaped = resolve })
escaped("escaped resolver")
const escapedValue = await later
const adopt = await new Promise((resolve) => resolve(Promise.resolve("adopted inner")))
const adoptThen = await new Promise((resolve) => resolve({ then: (r) => r("adopted thenable") }))
let selfResolve
const selfP = new Promise((resolve) => { selfResolve = resolve })
selfResolve(selfP)
const selfErr = await selfP.catch((e) => e.name + ": " + e.message)
const wr = Promise.withResolvers()
wr.resolve("with resolvers")
const wrValue = await wr.promise
const wr2 = Promise.withResolvers()
wr2.reject(new Error("wr rejected"))
const wrErr = await wr2.promise.catch((e) => e.message)
const tried = await Promise.try((a, b) => a + b, 2, 3)
const triedThrow = await Promise.try(() => { throw new Error("try threw") }).catch((e) => e.message)
const triedPromise = await Promise.try(() => Promise.resolve("try promise"))
const resolvers = [1, 2, 3].map((n) => new Promise((resolve) => [n].forEach(resolve)))
const fromForEach = await Promise.all(resolvers)
const asCallback = await Promise.resolve(-5).then(Math.abs)
let notNew
try { Promise((r) => r(1)) } catch (e) { notNew = e.message }
let badExecutor
try { new Promise(5) } catch (e) { badExecutor = e.message }
let staticDetached
try { [1].map(Promise.resolve) } catch (e) { staticDetached = e.message }
return { log, v, rejected, execThrow, execThrowAfter, escapedValue, adopt, adoptThen, selfErr, wrValue, wrErr, tried, triedThrow, triedPromise, fromForEach, asCallback, notNew, badExecutor, staticDetached }`,
])

programs("promise combinators", [
  `const delay = async (v, n) => { for (let i = 0; i < n; i++) await null; return v }
const fail = async (msg, n) => { for (let i = 0; i < n; i++) await null; throw new Error(msg) }
const all = await Promise.all([delay("a", 3), "plain", delay("b", 1), Promise.resolve("c")])
const allFail = await Promise.all([delay("a", 3), fail("all failed", 1), fail("later", 2)]).catch((e) => e.message)
const allEmpty = await Promise.all([])
const allSet = await Promise.all(new Set([delay(1, 1), 2]))
const allGen = await Promise.all((function* () { yield delay("g1", 1); yield "g2" })())
const settled = await Promise.allSettled([delay("ok", 1), fail("bad", 2), 3])
const race = await Promise.race([delay("slow", 5), delay("fast", 1)])
const raceReject = await Promise.race([delay("slow", 5), fail("fast fail", 1)]).catch((e) => e.message)
const racePlain = await Promise.race([delay("x", 1), "immediate"])
const any = await Promise.any([fail("e1", 1), delay("first ok", 3), delay("second ok", 4)])
const anyAll = await Promise.any([fail("e1", 2), fail("e2", 1), Promise.reject("raw")]).catch((e) => ({ name: e.name, message: e.message, errors: e.errors.map((x) => (x instanceof Error ? x.message : x)), isAgg: e instanceof AggregateError }))
const anyEmpty = await Promise.any([]).catch((e) => ({ name: e.name, errors: e.errors, message: e.message }))
const notIterable = await Promise.all(5).catch((e) => e.name + ": " + e.message)
const order = []
const p1 = delay("p1", 1)
p1.then(() => order.push("member reaction"))
await Promise.all([p1]).then(() => order.push("all reaction"))
const agg = new AggregateError([new Error("x"), "y"], "custom agg")
return { all, allFail, allEmpty, allSet, allGen, settled, race, raceReject, racePlain, any, anyAll, anyEmpty, notIterable, order, agg: { name: agg.name, message: agg.message, errors: agg.errors.length } }`,
  `const p = Promise.reject(new Error("never awaited"))
const q = (async () => { throw new TypeError("background failure") })()
Promise.reject("string reason")
const pending = new Promise(() => {})
const ok = Promise.resolve("fine")
"returned while rejections were unhandled"`,
  `const log = []
const work = (async () => { log.push("started"); await new Promise(() => {}); log.push("never") })()
const slow = (async () => { for (let i = 0; i < 1000; i++) await null; log.push("slow done") })()
const fast = await Promise.race([slow, Promise.resolve("race winner")])
return { fast, log }`,
  `const handled = Promise.reject(new Error("handled later"))
await null
const msg = await handled.catch((e) => e.message)
const late = Promise.reject(new Error("caught before end"))
late.catch(() => {})
return msg`,
  `const p = Promise.resolve({ nested: Promise.resolve(1) })
return p`,
  `async function main() { throw new RangeError("top-level async failure") }
await main()`,
  `Promise.reject(new Error("returned rejection"))`,
])

programs("generators and iterators", [
  `function* counter(limit) { let i = 0; while (i < limit) { const got = yield i++; if (got) i += got } return "done" }
const g = counter(10)
const steps = [g.next(), g.next(), g.next(5), g.next(), g.return("early"), g.next()]
function* withFinally() { try { yield 1; yield 2 } finally { yield "cleanup" } }
const wf = withFinally()
const wfSteps = [wf.next(), wf.return("r"), wf.next(), wf.next()]
function* catcher() { while (true) { try { yield "waiting" } catch (e) { yield "caught " + e } } }
const c = catcher()
const cSteps = [c.next(), c.throw("boom"), c.next(), c.throw("again")]
function* thrower() { yield 1 }
const th = thrower()
let thrownOut
try { th.throw(new Error("not started")) } catch (e) { thrownOut = e.message }
const thAfter = th.next()
function* inner() { const x = yield "inner1"; yield "inner got " + x; return "inner result" }
function* outer() { const r = yield* inner(); yield "outer got " + r; yield* [10, 20]; yield* "ab" }
const o = outer()
const oSteps = [o.next(), o.next("X"), o.next(), o.next(), o.next(), o.next(), o.next(), o.next()]
const genExpr = function* named() { yield named.name }
const objGen = { *items() { yield* [1, 2] } }
function* fib() { let [a, b] = [0, 1]; while (true) { yield a; [a, b] = [b, a + b] } }
const firstFibs = []
for (const n of fib()) { if (n > 50) break; firstFibs.push(n) }
const isIter = typeof g[Symbol.iterator] === "function" && g[Symbol.iterator]() === g
function* argsGen(a, { b } = {}) { yield a + b }
let badArg
try { argsGen(1, null) } catch (e) { badArg = e.name }
const running = (function* self() { yield 1 })()
return { steps, wfSteps, cSteps, thrownOut, thAfter, oSteps, genExpr: [...genExpr()], objGen: [...objGen.items()], firstFibs, isIter, badArg, str: Object.prototype.toString.call(g), from: Array.from(counter(3), (v, i) => v * 10 + i), instance: g instanceof counter, json: JSON.stringify(g) }`,
  `function* reentrant() { yield gen.next() }
const gen = reentrant()
let reentry
try { gen.next() } catch (e) { reentry = e.name + ": " + e.message }
function* throwsInside() { yield 1; throw new Error("gen threw") }
const ti = throwsInside()
ti.next()
let insideErr
try { ti.next() } catch (e) { insideErr = e.message }
const afterThrow = ti.next()
function* returnsThrow() { try { yield 1 } finally { throw new Error("finally threw") } }
const rt = returnsThrow()
rt.next()
let finallyErr
try { rt.return() } catch (e) { finallyErr = e.message }
const closing = { [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return: () => 5 } } }
let badReturn
try { for (const x of closing) break } catch (e) { badReturn = e.name + ": " + e.message }
const throwsReturn = { [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return: () => { throw new Error("return threw") } } } }
let consumerWins
try { for (const x of throwsReturn) throw new Error("consumer error") } catch (e) { consumerWins = e.message }
let cleanupThrow
try { for (const x of throwsReturn) break } catch (e) { cleanupThrow = e.message }
const yieldStarReturn = (function* () { const r = yield* { [Symbol.iterator]() { let i = 0; return { next: () => (i++ < 1 ? { value: "from custom", done: false } : { value: "custom result", done: true }) } } }; yield r })()
return { reentry, insideErr, afterThrow, finallyErr, badReturn, consumerWins, cleanupThrow, yieldStar: [...yieldStarReturn] }`,
  `async function* ticker(n) { for (let i = 0; i < n; i++) { await null; yield i } return "end" }
const seen = []
for await (const v of ticker(3)) seen.push(v)
const ag = ticker(2)
const manual = [await ag.next(), await ag.next(), await ag.next(), await ag.next()]
const fromSync = []
for await (const v of [Promise.resolve("p1"), "plain", Promise.resolve("p2")]) fromSync.push(v)
const custom = { [Symbol.asyncIterator]() { let i = 0; return { next: async () => (i < 2 ? { value: "async" + i++, done: false } : { done: true }), return: async () => { seen.push("async return"); return { done: true } } } } }
for await (const v of custom) seen.push(v)
for await (const v of custom) { seen.push("break at " + v); break }
async function* delegating() { yield* ticker(2); yield* ["sync-a", Promise.resolve("sync-b")]; yield* custom }
const delegated = []
for await (const v of delegating()) delegated.push(v)
async function* withFinally() { try { yield "x"; yield "y" } finally { seen.push("async gen finally") } }
for await (const v of withFinally()) break
const queued = ticker(3)
const parallel = await Promise.all([queued.next(), queued.next(), queued.return("stop"), queued.next()])
async function* thrower() { yield 1; throw new Error("async gen threw") }
let thrownAsync
try { for await (const v of thrower()) seen.push("got " + v) } catch (e) { thrownAsync = e.message }
const ag2 = ticker(5)
await ag2.next()
const throwIn = await ag2.throw(new Error("injected")).catch((e) => e.message)
let syncSpread
try { [...ticker(1)] } catch (e) { syncSpread = e.name + ": " + e.message }
const rejectedYield = (async function* () { yield Promise.reject(new Error("yielded rejection")) })()
const rejectedStep = await rejectedYield.next().catch((e) => e.message)
return { seen, manual, fromSync, delegated, parallel, thrownAsync, throwIn, syncSpread, rejectedStep, tag: Object.prototype.toString.call(ag) }`,
  `const iterable = {
  from: 1, to: 4,
  [Symbol.iterator]() { let cur = this.from; const last = this.to; return { next: () => (cur <= last ? { value: cur++, done: false } : { value: undefined, done: true }), [Symbol.iterator]() { return this } } },
}
const arr = [...iterable]
const [first, ...others] = iterable
const set = new Set(iterable)
const map = new Map([[1, "a"]].concat([[2, "b"]]))
const fromEntries = Object.fromEntries((function* () { yield ["k1", 1]; yield ["k2", 2] })())
const grouped = Object.groupBy((function* () { yield* [1, 2, 3, 4] })(), (n) => (n % 2 ? "odd" : "even"))
const mapGrouped = Map.groupBy([1, 2, 3], (n) => n > 1)
const arrayIter = [1, 2][Symbol.iterator]()
const steps = [arrayIter.next(), arrayIter.next(), arrayIter.next()]
const entries = [..."ab".matchAll(/./g)].map((m) => m[0])
const maxOf = Math.max(...iterable)
const strIter = "xy"[Symbol.iterator]()
let badIter
try { [...{ [Symbol.iterator]: 5 }] } catch (e) { badIter = e.name + ": " + e.message }
let badNext
try { [...{ [Symbol.iterator]() { return {} } }] } catch (e) { badNext = e.name + ": " + e.message }
return { arr, first, others, set: [...set], map: [...map], fromEntries, grouped, mapGrouped: [...mapGrouped], steps, entries, maxOf, strIter: [strIter.next(), strIter.next(), strIter.next()], badIter, badNext, asyncSym: typeof Symbol.asyncIterator, symStr: String(Symbol.iterator) }`,
])

programs("async delegation and settlement edge cases", [
  `const log = []
const syncIt = () => ({ [Symbol.iterator]() { let i = 0; return {
  next: (v) => ({ value: "n" + i++ + ":" + v, done: false }),
  return: (v) => { log.push("sync return " + v); return { value: "ret:" + v, done: true } },
  throw: (e) => { log.push("sync throw " + e); return { value: "recovered", done: false } },
} } })
async function* outer() { const r = yield* syncIt(); return r }
const o1 = outer()
const r1 = [await o1.next("ignored"), await o1.next("second"), await o1.return("R"), await o1.next()]
const o2 = outer()
const r2 = [await o2.next(), await o2.throw("E"), await o2.return("done")]
const bare = () => ({ [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }) } } })
async function* outer2() { yield* bare() }
const o3 = outer2()
const r3 = [await o3.next(), await o3.return("bare return")]
const o4 = outer2()
await o4.next()
const r4 = await o4.throw(new Error("no throw method")).catch((e) => e.name + ": " + e.message)
const badReturn = () => ({ [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return: () => 5, throw: () => 6 } } })
async function* outer5() { yield* badReturn() }
const o5 = outer5()
await o5.next()
const r5 = await o5.return("x").catch((e) => e.name + ": " + e.message)
const o5b = outer5()
await o5b.next()
const r5b = await o5b.throw("y").catch((e) => e.name + ": " + e.message)
const rejecting = () => ({ [Symbol.iterator]() { return { next: () => ({ value: Promise.reject(new Error("rejected value")), done: false }), return: () => { log.push("closed on rejection"); return {} } } } })
async function* outer3() { yield* rejecting() }
const r6 = await outer3().next().catch((e) => e.message)
let r7
try { for await (const v of rejecting()) log.push("never " + v) } catch (e) { r7 = e.message }
async function* outer4() { yield* [1, 2, 3] }
const o6 = outer4()
const r8 = [await o6.next(), await o6.return("arr ret"), await o6.next()]
const o7 = outer4()
await o7.next()
const r9 = await o7.throw(new Error("arr throw")).catch((e) => e.message)
const doneFirst = () => ({ [Symbol.iterator]() { return { next: () => ({ value: "final", done: true }) } } })
async function* outer8() { const r = yield* doneFirst(); yield "after " + r }
const r10 = []
for await (const v of outer8()) r10.push(v)
return { log, r1, r2, r3, r4, r5, r5b, r6, r7, r8, r9, r10 }`,
  `const log = []
const ag = (async function* () { try { yield 1 } finally { log.push("cleanup") } })()
const fresh = await ag.return(Promise.resolve("resolved before start"))
const after = await ag.next()
const ag2 = (async function* () { yield 1 })()
await ag2.next()
await ag2.next()
const completed = await ag2.return(Promise.resolve("on completed"))
const ag3 = (async function* () { yield 1 })()
const rejectedReturn = await ag3.return(Promise.reject(new Error("rejected return"))).catch((e) => e.message)
const ag4 = (async function* () { yield 1 })()
const throwFresh = await ag4.throw(new Error("thrown before start")).catch((e) => e.message)
const ag5 = (async function* () { const x = yield 1; yield x })()
const queued = await Promise.all([ag5.next(), ag5.next("queued value"), ag5.next(), ag5.return("q")])
const sg = (function* () { yield 1 })()
const syncReturnFresh = [sg.return("fresh"), sg.next()]
const sg2 = (function* () { yield 1 })()
let syncThrowFresh
try { sg2.throw("boom") } catch (e) { syncThrowFresh = e }
let acc = 0
acc += await Promise.resolve(5)
acc = (await Promise.resolve(acc)) * 2
let ref = { n: 1 }
ref.n += await Promise.resolve(2)
ref["n"] *= await 3
const raceEmpty = await Promise.race([]).catch?.((e) => e.message)
return { log, fresh, after, completed, rejectedReturn, throwFresh, queued, syncReturnFresh, syncThrowFresh, acc, ref, raceEmpty }`,
  `const tried = (() => { try { return Promise.try(5) } catch (e) { return e.name + ": " + e.message } })()
const tried2 = await (async () => { try { return await Promise.try("str") } catch (e) { return e.message } })()
const describes = [[1], Promise.resolve(), (function* () {})(), [].values(), new Map(), new Set(), null, 5, "s", {}, Symbol.iterator, new Date(0), /r/, new Error("e"), new URL("https://example.com"), new Uint8Array(1), new Headers()].map((v) => { try { Promise.try(v); return "no throw" } catch (e) { return e.message } })
const notIterables = [Promise.resolve(), (async function* () {})(), new WeakMap(), () => 1].map((v) => { try { for (const x of v) {} return "iterated" } catch (e) { return e.message } })
let spins = 0
for (let i = 0; i < 10000; i++) { await null; spins++ }
const rejecting = { [Symbol.iterator]() { return { next: () => ({ value: Promise.reject(new Error("original reason")), done: false }), return: () => { throw new Error("close failed") } } } }
let closeThrows
try { for await (const v of rejecting) {} } catch (e) { closeThrows = e.message }
const rejectingBadClose = { [Symbol.iterator]() { return { next: () => ({ value: Promise.reject(new Error("reason wins")), done: false }), return: () => 5 } } }
let closeBad
try { for await (const v of rejectingBadClose) {} } catch (e) { closeBad = e.message }
return { tried, tried2, describes, notIterables, spins, closeThrows, closeBad }`,
  `const r = Promise.race([])
"unreachable"`,
  `await new Promise(() => {})
"never"`,
  `async function waitForever() { await new Promise(() => {}) }
const a = waitForever()
const b = waitForever()
await a`,
  `const gen = (async function* () { await new Promise(() => {}); yield 1 })()
for await (const v of gen) {}`,
  `throw () => 1`,
  `throw Promise.resolve(1)`,
])
