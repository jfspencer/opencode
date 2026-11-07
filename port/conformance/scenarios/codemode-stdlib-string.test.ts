// Code-mode standard library: String and RegExp, on the legacy interpreter and the bytecode VM.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const PRELUDE = `const r = {}
const show = (v) => v === undefined ? "<undefined>" : typeof v === "number" && (!Number.isFinite(v) || Object.is(v, -0)) ? "<" + (Object.is(v, -0) ? "-0" : String(v)) + ">" : v
const t = (k, f) => { try { r[k] = show(f()) } catch (e) { r[k] = "!" + (e && e.name) + ": " + (e && e.message) } }
// Errors whose message is the JS engine's own wording (no opencode text around it): the standard records the
// error type only (see the equality rules in README.md).
const te = (k, f) => { try { r[k] = show(f()) } catch (e) { r[k] = "!" + (e && e.name) } }
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

programs("string statics and conversion", [
  `t("String()", () => [String(), String(undefined), String(null), String(12.5), String(true), String([1, [2]]), String({}), String(-0)])
t("String of function", () => [String(() => 1), String(Math.max), \`\${() => 1}\`])
t("String toString object", () => String({ toString() { return "custom" } }))
t("String valueOf ignored", () => String({ valueOf() { return 5 }, toString() { return "str" } }))
t("String toString throws", () => String({ toString() { throw new TypeError("nope") } }))
t("String of symbol", () => String(Symbol.iterator))
t("fromCharCode", () => [String.fromCharCode(72, 105), String.fromCharCode(), String.fromCharCode("65", 0x1F600)])
t("fromCodePoint", () => [String.fromCodePoint(0x1F600, 97), String.fromCodePoint()])
te("fromCodePoint invalid", () => String.fromCodePoint(-1))
t("raw template", () => String.raw\`a\\n\${1 + 1}b\\t\${"x"}c\`)
t("raw object", () => String.raw({ raw: ["x", "y", "z"] }, 1, 2, 3, 4))
t("raw short", () => String.raw({ raw: "abc" }, "-", "+"))
t("raw empty", () => String.raw({ raw: [] }))
t("raw invalid", () => String.raw({}))
t("raw no args", () => String.raw())
t("String.length", () => [String.length, String.name, "abc".length, "😀".length])
t("prototype", () => [Object.getPrototypeOf("a") === String.prototype, "a".constructor === String, typeof String.prototype.at])
t("index access", () => ["abc"[1], "abc"[5], "abc"["length"], "abc"[-1]])
t("iterate", () => [..."a😀b"])
t("iterator next", () => { const it = "ab"[Symbol.iterator](); return [it.next(), it.next(), it.next()] })
t("for-of", () => { const out = []; for (const ch of "xyz") out.push(ch); return out.join("|") })
t("for-in", () => { const out = []; for (const k in "ab") out.push(k); return out })
t("null receiver", () => String.prototype.trim.call(null))
t("undefined receiver", () => String.prototype.toUpperCase.call(undefined))
t("object receiver", () => String.prototype.trim.call({ toString() { return "  a  " } }))
t("number receiver", () => String.prototype.padStart.call(5, 3, "0"))
t("toString receiver", () => [String.prototype.toString.call("x"), "y".valueOf()])
t("iterator null receiver", () => String.prototype[Symbol.iterator].call(null))
t("iterator number receiver", () => [...String.prototype[Symbol.iterator].call(42)])
return r`,
])

programs("string methods", [
  `const s = "  Hello, World  "
t("case", () => [s.toUpperCase(), s.toLowerCase(), "İ".toLocaleLowerCase(), "ß".toLocaleUpperCase()])
t("trim", () => [s.trim(), s.trimStart(), s.trimEnd(), s.trimLeft(), s.trimRight()])
t("localeCompare", () => ["a".localeCompare("b"), "b".localeCompare("a"), "a".localeCompare("a"), "a".localeCompare()])
t("normalize", () => ["e\\u0301".normalize().length, "e\\u0301".normalize("NFC").length, "é".normalize("NFD").length, "ﬁ".normalize("NFKC"), "ﬁ".normalize("NFKD")])
t("normalize invalid", () => "a".normalize("NFX"))
t("split", () => ["a,b,c".split(","), "a,b,c".split(",", 2), "abc".split(""), "abc".split(), "a1b2c".split(/\\d/), "a1b2c".split(/(\\d)/), "abc".split(undefined, 0), "a,b".split(",", -1), "".split(","), "a,b".split(",", "1")])
t("split regex limit", () => "a1b2c3".split(/\\d/, 2))
t("split object separator", () => "a-b".split({ toString() { return "-" } }))
t("slice", () => ["hello".slice(1, 3), "hello".slice(-3), "hello".slice(), "hello".slice(2, -1), "hello".slice("1", "2")])
t("includes", () => ["hello".includes("ell"), "hello".includes("ell", 2), "hello".includes(), "undefined".includes()])
t("includes regex", () => "abc".includes(/b/))
t("startsWith", () => ["hello".startsWith("he"), "hello".startsWith("l", 2), "hello".startsWith("x")])
t("startsWith regex", () => "abc".startsWith(/a/))
t("endsWith", () => ["hello".endsWith("lo"), "hello".endsWith("l", 4), "hello".endsWith("h", 0)])
t("endsWith regex", () => "abc".endsWith(/c/))
t("indexOf", () => ["hello".indexOf("l"), "hello".indexOf("l", 3), "hello".indexOf("z"), "hello".indexOf(""), "a1".indexOf(1)])
t("lastIndexOf", () => ["hello".lastIndexOf("l"), "hello".lastIndexOf("l", 2), "hello".lastIndexOf("z")])
t("indexOf coercion", () => "abc".indexOf({ toString() { return "b" } }))
t("repeat", () => ["ab".repeat(3), "x".repeat(0), "x".repeat("2")])
t("repeat negative", () => "x".repeat(-1))
t("repeat infinite", () => "x".repeat(Infinity))
t("repeat too long", () => "abcdefgh".repeat(4000000))
t("padStart", () => ["5".padStart(3, "0"), "abc".padStart(6), "abc".padStart(2), "abc".padStart(8, "12"), "x".padStart(3, "")])
t("padEnd", () => ["5".padEnd(3, "0"), "abc".padEnd(6, "-="), "abc".padEnd(1)])
t("pad too long", () => "x".padEnd(20000000))
t("charAt", () => ["abc".charAt(1), "abc".charAt(), "abc".charAt(9), "abc".charAt(-1)])
t("at", () => ["abc".at(0), "abc".at(-1), "abc".at(5), "abc".at()])
t("substring", () => ["hello".substring(1, 3), "hello".substring(3, 1), "hello".substring(-2), "hello".substring()])
t("substr", () => ["hello".substr(1, 3), "hello".substr(-3, 2), "hello".substr(), "hello".substr(2)])
t("well formed", () => ["ab".isWellFormed(), "a\\uD800".isWellFormed(), "a\\uD800".toWellFormed().charCodeAt(1)])
t("charCodeAt", () => ["abc".charCodeAt(0), "abc".charCodeAt(), "abc".charCodeAt(10), "😀".charCodeAt(1)])
t("codePointAt", () => ["😀".codePointAt(0), "😀".codePointAt(1), "a".codePointAt(5), "a".codePointAt()])
t("concat", () => ["a".concat("b", 1, null, [2, 3]), "a".concat()])
t("concat object", () => "a".concat({ toString() { return "Z" } }))
t("opaque argument", () => "abc".indexOf(Promise.resolve(1)))
t("ToPrimitive order", () => { const log = []; const o = (n) => ({ toString() { log.push(n); return "b" } }); "abc".lastIndexOf(o("search"), { valueOf() { log.push("pos"); return 2 } }); return log })
return r`,
])

programs("string replace and match", [
  `t("replace string", () => ["aaa".replace("a", "b"), "aaa".replaceAll("a", "b"), "a.b.c".replaceAll(".", "-")])
t("replace patterns", () => ["John Smith".replace(/(\\w+)\\s(\\w+)/, "$2, $1"), "abc".replace("b", "[$&]"), "abc".replace("b", "[$\`|$']"), "abc".replace("b", "$$"), "2024-01-05".replace(/(?<y>\\d+)-(?<m>\\d+)-(?<d>\\d+)/, "$<d>/$<m>/$<y>")])
t("replace regex global", () => ["a1b2c3".replace(/\\d/g, "#"), "a1b2c3".replace(/\\d/, "#")])
t("replace function", () => "a1b22".replace(/\\d+/g, (m, offset, str) => "[" + m + "@" + offset + "/" + str.length + "]"))
t("replace function groups", () => "x=1,y=2".replace(/(?<k>\\w)=(?<v>\\d)/g, (m, k, v, offset, str, groups) => groups.v + groups.k))
t("replace function captures", () => "ab".replace(/(a)(b)?(c)?/, (m, p1, p2, p3) => [p1, p2, p3 === undefined].join(",")))
t("replace string function", () => "aXbX".replace("X", (m, offset) => offset))
t("replaceAll function", () => "aXbX".replaceAll("X", (m, offset) => "<" + offset + ">"))
t("replaceAll regex function", () => "aXbX".replaceAll(/X/g, (m) => m.toLowerCase()))
t("replaceAll non-global", () => "aaa".replaceAll(/a/, "b"))
t("replaceAll non-global fn", () => "aaa".replaceAll(/a/, () => "b"))
t("replace returns promise", () => "ab".replace("a", async () => "x"))
t("replace with tool", () => "ab".replace("a", tools))
t("replace with Map", () => "ab".replace("a", Map))
t("replace with tool function", () => "ab".replace("a", tools.opencode.models))
t("replace with search", () => "ab".replaceAll("a", search))
t("replace coerces replacement", () => ["ab".replace("a", 5), "ab".replace("a", null), "ab".replace(/a/, { toString() { return "R" } })])
t("replace builtin", () => "3.7 and 1.2".replace(/\\d\\.\\d/g, Math.floor))
t("replace object pattern", () => "a-b".replace({ toString() { return "-" } }, "+"))
t("replace undefined", () => ["undefinedx".replace(undefined, "!"), "ab".replace("a")])
t("replace callback throws", () => "ab".replace("a", () => { throw new SyntaxError("cb") }))
return r`,
  `t("match", () => { const m = "Price: 42 USD".match(/(\\d+) (?<cur>[A-Z]+)/); return [m[0], m[1], m.index, m.input, m.groups.cur, m.length] })
t("match global", () => "a1b2c3".match(/\\d/g))
t("match none", () => "abc".match(/\\d/))
t("match string pattern", () => "a.c".match("."))
t("match undefined", () => "abc".match())
t("match invalid string", () => "abc".match("("))
t("matchAll", () => [..."a1b2".matchAll(/[a-z](\\d)/g)].map((m) => [m[0], m[1], m.index]))
t("matchAll string", () => "a.b.".matchAll(".").length)
t("matchAll non-global", () => "a1".matchAll(/\\d/))
t("matchAll invalid", () => "a".matchAll("["))
t("search", () => ["abc".search(/c/), "abc".search("b"), "abc".search(/z/), "abc".search()])
t("search invalid", () => "a".search("+"))
t("match indices", () => { const m = "xaby".match(/a(?<b>b)/d); return [m.indices[0], m.indices[1], m.indices.groups.b] })
t("match indices no groups", () => { const m = "xab".match(/a(b)?(c)?/d); return [m.indices.length, m.indices[2] === undefined, m.indices.groups === undefined] })
return r`,
  `t("literal props", () => { const re = /a+(b)/gimsuyd; return [re.source, re.flags, re.global, re.ignoreCase, re.multiline, re.dotAll, re.unicode, re.sticky, re.hasIndices, re.unicodeSets, re.lastIndex] })
t("v flag", () => /[\\p{L}--[a-z]]/v.unicodeSets)
t("constructor", () => [String(new RegExp("a.b", "g")), String(RegExp("x")), String(new RegExp(/y/i)), String(new RegExp(/y/i, "g")), String(new RegExp()), String(RegExp(undefined, "m"))])
t("constructor invalid pattern", () => new RegExp("("))
t("constructor invalid flags", () => new RegExp("a", "gg"))
t("constructor flags type", () => new RegExp("a", 5))
t("constructor null flags", () => new RegExp("a", null))
t("constructor number pattern", () => String(new RegExp(12)))
t("exec", () => { const re = /(\\d)(?<l>[a-z])?/g; const out = []; let m; while ((m = re.exec("1a2 3c")) !== null) out.push([m[0], m.index, m.groups.l === undefined ? "-" : m.groups.l, re.lastIndex]); return out })
t("exec null", () => /z/.exec("abc"))
t("test", () => [/a/.test("cat"), /a/.test("dog"), /a/i.test("CAT")])
t("test global lastIndex", () => { const re = /a/g; return [re.test("aa"), re.lastIndex, re.test("aa"), re.lastIndex, re.test("aa"), re.lastIndex] })
t("sticky", () => { const re = /a/y; re.lastIndex = 1; return [re.test("ba"), re.lastIndex, re.test("ba")] })
t("lastIndex set", () => { const re = /b/g; re.lastIndex = "2"; return [re.exec("abab").index, re.lastIndex] })
t("dotAll", () => [/a.b/.test("a\\nb"), /a.b/s.test("a\\nb")])
t("multiline", () => "a\\nb".match(/^b/m)[0])
t("unicode", () => [/^.$/u.test("😀"), /^.$/.test("😀")])
t("toString", () => [/a\\/b/g.toString(), String(/x/), \`\${/y/i}\`, /a/ + ""])
t("escape", () => RegExp.escape("a.b*c"))
t("escape non-string", () => RegExp.escape(5))
t("receiver", () => RegExp.prototype.test.call({}, "a"))
t("source getter receiver", () => { const d = Object.getOwnPropertyNames ? 1 : 0; return RegExp.prototype.exec.call("x") })
t("prototype", () => [Object.getPrototypeOf(/a/) === RegExp.prototype, /a/ instanceof RegExp, Object.prototype.toString.call(/a/)])
t("json", () => JSON.stringify({ re: /a/g }))
t("in template", () => \`\${/a/g}\`)
return r`,
])
