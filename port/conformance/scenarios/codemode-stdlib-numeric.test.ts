// Code-mode standard library: Math, Number, Boolean, the global coercion functions, and Date, on the legacy
// interpreter and the bytecode VM. Dates use fixed timestamps only.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const PRELUDE = `const r = {}
const show = (v) => v === undefined ? "<undefined>" : typeof v === "number" && (!Number.isFinite(v) || Object.is(v, -0)) ? "<" + (Object.is(v, -0) ? "-0" : String(v)) + ">" : v
const t = (k, f) => { try { r[k] = show(f()) } catch (e) { r[k] = "!" + (e && e.name) + ": " + (e && e.message) } }
const num = (v) => typeof v === "number" ? (Number.isNaN(v) ? "NaN" : Object.is(v, -0) ? "-0" : String(v)) : v
// ISO timestamps are kept readable: the recorder would otherwise mask them as wall-clock times.
const D = (v) => typeof v === "string" ? v.replace(/(\\d{4}-\\d{2}-\\d{2})T(\\d{2}:)/g, "$1 $2") : Array.isArray(v) ? v.map(D) : v
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

programs("math", [
  `t("constants", () => [Math.PI, Math.E, Math.LN2, Math.LN10, Math.LOG2E, Math.LOG10E, Math.SQRT2, Math.SQRT1_2].map(num))
t("unary", () => ["abs", "acos", "acosh", "asin", "asinh", "atan", "atanh", "floor", "ceil", "round", "trunc", "sign", "sqrt", "cbrt", "cos", "cosh", "sin", "sinh", "tan", "tanh", "log", "log2", "log10", "log1p", "exp", "expm1", "f16round", "fround", "clz32"].map((name) => name + "=" + [-2.5, -0.5, 0, 0.5, 1, 2.7, 100].map((x) => num(Math[name](x))).join(",")))
t("binary", () => [Math.atan2(1, 2), Math.pow(2, 10), Math.pow(-8, 1 / 3), Math.imul(3, 4), Math.imul(0xffffffff, 5)].map(num))
t("variadic", () => [Math.max(), Math.min(), Math.max(1, 3, 2), Math.min(1, -3), Math.max(1, NaN), Math.hypot(3, 4), Math.hypot(), Math.hypot(1, 2, 2)].map(num))
t("coercion", () => [Math.abs("-3"), Math.max("7", [8]), Math.floor({ valueOf() { return 4.9 } }), Math.abs(null), Math.abs(undefined), Math.abs([])].map(num))
t("coercion throws", () => Math.abs({ valueOf() { throw new RangeError("vo") } }))
t("coercion opaque", () => num(Math.abs(() => 1)))
t("random", () => { const x = Math.random(); return [typeof x, x >= 0 && x < 1] })
t("sumPrecise", () => [Math.sumPrecise([1e20, 0.1, -1e20]), Math.sumPrecise([]), Math.sumPrecise(new Set([1, 2]))].map(num))
t("sumPrecise generator", () => { function* g() { yield 0.1; yield 0.2 } return num(Math.sumPrecise(g())) })
t("sumPrecise non-number", () => Math.sumPrecise([1, "2"]))
t("sumPrecise non-iterable", () => Math.sumPrecise(5))
t("sumPrecise closes", () => { let closed = false; function* g() { try { yield 1; yield "x"; yield 3 } finally { closed = true } } try { Math.sumPrecise(g()) } catch (e) { return [e.message, closed] } })
t("as callbacks", () => [[1.5, -2.5].map(Math.round), [4, 9].map(Math.sqrt), [3, 1, 2].reduce((a, b) => Math.max(a, b))])
t("Math props", () => [typeof Math, Object.prototype.toString.call(Math), Math.max.length, Math.abs.name, Object.keys(Math).length])
t("Math readonly", () => { Math.E = 1; return Math.E })
t("Math method replaced", () => { Math.abs = (x) => "patched"; return Math.abs(-1) })
return r`,
])

programs("number boolean and coercion", [
  `t("Number()", () => [Number(), Number("12"), Number(" 0x1f "), Number("1e3"), Number(""), Number("abc"), Number(null), Number(undefined), Number(true), Number([5]), Number([1, 2]), Number({}), Number("-0"), Number("Infinity"), Number(new Date(5))].map(num))
t("Number valueOf object", () => Number({ valueOf() { return "42" } }))
t("Number of symbol", () => Number(Symbol.iterator))
t("constants", () => [Number.MAX_SAFE_INTEGER, Number.MIN_SAFE_INTEGER, Number.MAX_VALUE, Number.MIN_VALUE, Number.EPSILON, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY].map(num))
t("predicates", () => [Number.isInteger(5), Number.isInteger(5.5), Number.isInteger("5"), Number.isFinite(1), Number.isFinite("1"), Number.isFinite(Infinity), Number.isNaN(NaN), Number.isNaN("NaN"), Number.isSafeInteger(2 ** 53), Number.isSafeInteger(2 ** 53 - 1)])
t("parseInt", () => [parseInt("42px"), parseInt("0x1F"), parseInt("101", 2), parseInt("z", 36), parseInt(""), parseInt("  -7.9"), parseInt("12", 1), parseInt("12", 37), parseInt(null), parseInt("ff", { valueOf() { return 16 } }), Number.parseInt("08"), Number.parseInt === parseInt].map(num))
t("parseFloat", () => [parseFloat("3.14abc"), parseFloat(".5"), parseFloat("-.5e2"), parseFloat("x"), parseFloat("Infinityx"), Number.parseFloat("1e-3")].map(num))
t("global predicates", () => [isNaN("abc"), isNaN("12"), isNaN(undefined), isFinite("12"), isFinite("1/0"), isNaN(() => 1), isFinite(null)])
t("toFixed", () => [(1.005).toFixed(2), (1234.5678).toFixed(1), (0).toFixed(), (-1.5).toFixed(0), (1e21).toFixed(2), (12.3).toFixed("3")])
t("toFixed range", () => (1).toFixed(101))
t("toExponential", () => [(123456).toExponential(2), (0.00015).toExponential(), (5).toExponential(0)])
t("toExponential range", () => (1).toExponential(-1))
t("toPrecision", () => [(123.456).toPrecision(4), (0.000123).toPrecision(2), (123456).toPrecision(2), (5).toPrecision(), (1.5).toPrecision(undefined)])
t("toPrecision range", () => (1).toPrecision(0))
t("toString radix", () => [(255).toString(16), (255).toString(2), (-255).toString(36), (0.5).toString(2), (10).toString(), (255).toString({ valueOf() { return 16 } })])
t("toString radix range", () => (10).toString(1))
t("toLocaleString", () => [(1234567.891).toLocaleString(), (0).toLocaleString(), (-5.5).toLocaleString(), NaN.toLocaleString()])
t("valueOf", () => [(5).valueOf(), Number.prototype.valueOf.call(7)])
t("receiver", () => Number.prototype.toFixed.call("1", 2))
t("valueOf receiver", () => Number.prototype.valueOf.call({}))
t("Boolean()", () => [Boolean(), Boolean(0), Boolean(""), Boolean("0"), Boolean([]), Boolean({}), Boolean(NaN), Boolean(null), Boolean(Symbol.iterator)])
t("Boolean methods", () => [true.toString(), false.valueOf(), Boolean.prototype.toString.call(true), String(false)])
t("Boolean receiver", () => Boolean.prototype.toString.call("true"))
t("Boolean valueOf receiver", () => Boolean.prototype.valueOf.call(1))
t("new Number", () => new Number(1))
t("new Boolean", () => new Boolean(1))
t("new String", () => new String("s"))
t("number method on literal", () => [1.5.toFixed(1), (2).toString(2), 3..toString()])
t("names", () => [Number.name, Number.length, Boolean.name, parseInt.name, parseInt.length, isNaN.length, String(parseFloat)])
t("prototype", () => [Object.getPrototypeOf(5) === Number.prototype, (5).constructor === Number, true.constructor === Boolean])
t("operators", () => [1 / 0, -1 / 0, 0 / 0, 2 ** -1, 7 % -3, -7 % 3, 5 / 2 | 0, ~5, 1 << 31, -1 >>> 28, 0.1 + 0.2].map(num))
return r`,
])

programs("dates", [
  `const d = new Date(Date.UTC(2024, 1, 29, 13, 45, 30, 123))
t("getters", () => ["getFullYear", "getMonth", "getDate", "getDay", "getHours", "getMinutes", "getSeconds", "getMilliseconds", "getUTCFullYear", "getUTCMonth", "getUTCDate", "getUTCDay", "getUTCHours", "getUTCMinutes", "getUTCSeconds", "getUTCMilliseconds", "getTimezoneOffset"].map((name) => name + "=" + d[name]()))
t("formats", () => D([d.toISOString(), d.toJSON(), String(d), d.toString(), d.toDateString(), d.toTimeString(), d.toUTCString(), d.toGMTString(), d.toLocaleString(), d.toLocaleDateString(), d.toLocaleTimeString()]))
t("numbers", () => D([d.getTime(), d.valueOf(), +d, d - new Date(Date.UTC(2024, 0, 1)), d + 1, \`\${d}\`]))
t("constructors", () => D([new Date(0).toISOString(), new Date("2020-05-06T07:08:09Z").getTime(), new Date("2020-05-06").toISOString(), new Date(2020, 0, 31, 1, 2, 3, 4).toISOString(), new Date(2020, 0).toISOString(), new Date(d).getTime() === d.getTime(), new Date(1e3).getTime(), new Date("1e3").getTime()]))
t("constructor object", () => D(new Date({ valueOf() { return 86400000 } }).toISOString()))
t("constructor string object", () => D(new Date({ toString() { return "2001-02-03T00:00:00Z" }, valueOf() { return {} } }).toISOString()))
t("extra components ignored", () => D(new Date(2020, 1, 2, 3, 4, 5, 6, 7, 8).toISOString()))
t("invalid", () => { const x = new Date("nope"); return [x.getTime(), String(x), x.toJSON(), x.getFullYear(), JSON.stringify({ x })].map(num) })
t("invalid toISOString", () => new Date(NaN).toISOString())
t("statics", () => [Date.UTC(2000, 0, 1), Date.UTC(2000), Date.parse("2000-01-01T00:00:00.000Z"), Date.parse("garbage"), typeof Date.now(), Date.name, Date.length].map(num))
t("Date() string", () => typeof Date())
t("Date.now positive", () => Date.now() > 1600000000000)
t("new Date() now", () => { const n = new Date(); return [n instanceof Date, n.getTime() > 1600000000000] })
t("setters", () => {
  const x = new Date(Date.UTC(2020, 0, 1))
  const out = []
  out.push(x.setTime(86400000))
  out.push(x.setMilliseconds(5))
  out.push(x.setUTCMilliseconds(6))
  out.push(x.setSeconds(7, 8))
  out.push(x.setUTCSeconds(9))
  out.push(x.setMinutes(10, 11, 12))
  out.push(x.setUTCMinutes(13))
  out.push(x.setHours(14, 15, 16, 17))
  out.push(x.setUTCHours(18))
  out.push(x.setDate(19))
  out.push(x.setUTCDate(20))
  out.push(x.setMonth(5, 6))
  out.push(x.setUTCMonth(7))
  out.push(x.setFullYear(2030, 1, 2))
  out.push(x.setUTCFullYear(2031))
  return [out, D(x.toISOString())]
})
t("setters on invalid", () => { const x = new Date(NaN); return [x.setHours(1), x.setMonth(2), x.setFullYear(2020), D(x.toISOString())].map(num) })
t("setTime invalid", () => { const x = new Date(0); return num(x.setTime(NaN)) })
t("setter coercion order", () => { const x = new Date(0); const log = []; x.setUTCHours({ valueOf() { log.push("h"); return 1 } }, { valueOf() { log.push("m"); return 2 } }); return [log, D(x.toISOString())] })
t("receiver", () => Date.prototype.getTime.call({}))
t("instanceof", () => [d instanceof Date, Object.prototype.toString.call(d), Object.getPrototypeOf(d) === Date.prototype])
t("compare", () => [d > new Date(0), d < new Date(0), new Date(5) == 5, new Date(5).getTime() === 5])
t("clone and mutate", () => { const c = new Date(d); c.setUTCFullYear(1999); return [c.getUTCFullYear(), d.getUTCFullYear()] })
return r`,
])
