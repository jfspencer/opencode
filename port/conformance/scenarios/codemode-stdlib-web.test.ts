// Code-mode standard library: URL, URLSearchParams, Headers, URI helpers, Uint8Array, TextEncoder/TextDecoder,
// base64, structuredClone, and crypto, on the legacy interpreter and the bytecode VM. No program calls fetch.
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

programs("url and search params", [
  `const u = new URL("https://user:pw@example.com:8080/a/b/../c?x=1&y=two#frag")
t("parts", () => ["href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"].map((k) => k + "=" + u[k]))
t("relative", () => [new URL("../d?q", "https://h.test/a/b/c").href, new URL("/root", new URL("https://h.test/x")).href, new URL(new URL("https://h.test/z")).href])
t("invalid", () => new URL("not a url"))
t("invalid base", () => new URL("/x", "nope"))
t("no args", () => new URL())
t("without new", () => URL("https://a.b"))
t("canParse", () => [URL.canParse("https://a.b"), URL.canParse("x"), URL.canParse("/p", "https://a.b"), URL.canParse(new URL("https://q.r"))])
t("canParse no args", () => URL.canParse())
t("parse", () => [URL.parse("https://a.b/c").pathname, URL.parse("bad"), URL.parse("d", "https://a.b/c/").href])
t("parse no args", () => URL.parse())
t("setters", () => { const x = new URL("http://a.test/p"); x.protocol = "https"; x.username = "me"; x.password = "s"; x.hostname = "b.test"; x.port = "81"; x.pathname = "/new path"; x.search = "?k=v w"; x.hash = "h"; return x.href })
t("host setter", () => { const x = new URL("http://a.test/"); x.host = "c.test:99"; return [x.host, x.port] })
t("href setter", () => { const x = new URL("http://a.test/"); x.href = "https://z.test/q?1"; return [x.href, x.searchParams.toString()] })
t("href invalid", () => { const x = new URL("http://a.test/"); x.href = "::bad::"; return x.href })
t("origin readonly", () => { const x = new URL("http://a.test/"); x.origin = "http://evil"; return x.origin })
t("searchParams live", () => { const x = new URL("http://a.test/?a=1"); x.searchParams.append("b", "2"); x.searchParams.set("a", "9"); return [x.search, x.href, x.searchParams === x.searchParams] })
t("toString/toJSON", () => [u.toString(), u.toJSON(), String(u), \`\${u}\`, JSON.stringify({ u })])
t("receiver", () => URL.prototype.toString.call({}))
t("getter receiver", () => { const g = Object.create(URL.prototype); return g.href })
t("tag", () => [Object.prototype.toString.call(u), u instanceof URL, URL.name])
t("encode/decode", () => [encodeURIComponent("a b&c/é"), encodeURI("http://x.y/a b?c=d&e=é"), decodeURIComponent("a%20b%26"), decodeURI("%41%2F"), encodeURIComponent(5), decodeURIComponent()])
t("decode malformed", () => decodeURIComponent("%E0%A4%A"))
t("decodeURI malformed", () => decodeURI("%"))
t("encode lone surrogate", () => encodeURIComponent("\\uD800"))
return r`,
  `t("from string", () => { const p = new URLSearchParams("?a=1&b=2&a=3&c=%20x+y"); return [p.get("a"), p.getAll("a"), p.get("c"), p.get("zz"), p.has("b"), p.has("a", "3"), p.has("a", "4"), p.size, p.toString()] })
t("from object", () => new URLSearchParams({ x: 1, y: [1, 2], z: null }).toString())
t("from pairs", () => new URLSearchParams([["a", "1"], ["b", 2]]).toString())
t("from map", () => new URLSearchParams(new Map([["k", "v"]])).toString())
t("from params", () => { const a = new URLSearchParams("q=1"); const b = new URLSearchParams(a); b.append("r", "2"); return [a.toString(), b.toString()] })
t("from primitives", () => [new URLSearchParams(5).toString(), new URLSearchParams(null).toString(), new URLSearchParams(true).toString(), new URLSearchParams().toString()])
t("bad pair length", () => new URLSearchParams([["a", "b", "c"]]))
t("bad pair", () => new URLSearchParams([5]))
t("from generator", () => { function* g() { yield ["g", "1"] } return new URLSearchParams(g()).toString() })
t("from opaque", () => new URLSearchParams(Promise.resolve(1)))
t("from function", () => new URLSearchParams(() => 1))
t("from symbol", () => new URLSearchParams(Symbol.iterator))
t("record values coerce", () => new URLSearchParams({ b: new Uint8Array([1, 2]), d: new Date(0).getTime(), re: /x/g, m: new Map(), arr: [1, [2]] }).toString())
t("without new", () => URLSearchParams("a=1"))
t("mutators", () => { const p = new URLSearchParams("b=2&a=1&a=0"); p.append("c", "3"); p.set("b", "9"); p.delete("a", "0"); const d1 = p.toString(); p.delete("a"); p.sort(); return [d1, p.toString(), p.size] })
t("iteration", () => { const p = new URLSearchParams("x=1&y=2"); return [[...p], [...p.keys()], [...p.values()], [...p.entries()], Object.fromEntries(p)] })
t("forEach", () => { const out = []; new URLSearchParams("a=1&b=2").forEach(function (v, k, params) { out.push(k + v + params.size + this.s) }, { s: "!" }); return out })
t("missing args", () => { const p = new URLSearchParams(); const out = []; for (const [name, args] of [["append", ["a"]], ["delete", []], ["get", []], ["getAll", []], ["has", []], ["set", ["a"]], ["forEach", []]]) { try { p[name](...args) } catch (e) { out.push(e.message) } } return out })
t("receiver", () => URLSearchParams.prototype.get.call({}, "a"))
t("size receiver", () => Object.create(URLSearchParams.prototype).size)
t("String", () => [String(new URLSearchParams("a=b")), \`\${new URLSearchParams({ q: "x y" })}\`])
t("json", () => JSON.stringify(new URLSearchParams("a=1")))
t("console", () => { console.log(new URLSearchParams("a=1"), new URL("http://a.b/c")); return true })
return r`,
])

programs("headers", [
  `t("from object", () => { const h = new Headers({ "Content-Type": "text/plain", "X-Num": 5 }); return [h.get("content-type"), h.get("x-num"), h.has("X-NUM"), h.get("missing")] })
t("from pairs", () => [...new Headers([["B", "2"], ["a", "1"], ["a", "3"]])])
t("from headers", () => { const a = new Headers({ k: "v" }); const b = new Headers(a); b.set("k", "w"); return [a.get("k"), b.get("k")] })
t("from map", () => [...new Headers(new Map([["m", "1"]]))])
t("empty", () => [...new Headers()])
t("invalid name", () => new Headers({ "bad name": "x" }))
t("invalid pairs", () => new Headers([["only"]]))
t("invalid init", () => new Headers(5))
t("opaque init", () => new Headers(Promise.resolve(1)))
t("without new", () => Headers())
t("methods", () => { const h = new Headers(); h.append("Accept", "a"); h.append("accept", "b"); h.set("X-Y", "1"); const before = h.get("accept"); h.delete("x-y"); return [before, h.has("x-y"), [...h.keys()], [...h.values()], [...h.entries()]] })
t("set-cookie", () => { const h = new Headers(); h.append("Set-Cookie", "a=1"); h.append("Set-Cookie", "b=2"); return [h.getSetCookie(), h.get("set-cookie")] })
t("invalid append", () => new Headers().append("bad name", "v"))
t("invalid set", () => new Headers().set("x", "bad\\nvalue"))
t("invalid get", () => new Headers().get("bad name"))
t("invalid has", () => new Headers().has("bad name"))
t("invalid delete", () => new Headers().delete("bad name"))
t("forEach", () => { const out = []; new Headers({ a: "1", b: "2" }).forEach(function (v, k, h) { out.push(k + v + this.p) }, { p: "?" }); return out })
t("missing args", () => { const h = new Headers(); const out = []; for (const [name, args] of [["append", ["a"]], ["delete", []], ["get", []], ["has", []], ["set", ["a"]], ["forEach", []]]) { try { h[name](...args) } catch (e) { out.push(e.message) } } return out })
t("receiver", () => Headers.prototype.get.call({}, "a"))
t("json", () => JSON.stringify({ h: new Headers({ A: "1" }) }))
t("destructure", () => { const [[k, v]] = new Headers({ Z: "26" }); return k + "=" + v })
t("console", () => { console.log(new Headers({ "X-Log": "yes" })); return true })
return r`,
])

programs("bytes and text", [
  `t("construct", () => [String(new Uint8Array()), String(new Uint8Array(3)), String(new Uint8Array([1, 256, -1, 3.7])), String(new Uint8Array(new Set([7, 8]))), String(new Uint8Array(new Uint8Array([9])))])
t("bad length", () => new Uint8Array(-1))
t("fraction length", () => new Uint8Array(1.5))
t("string length", () => new Uint8Array("2").length)
t("bad source", () => new Uint8Array({ a: 1 }))
t("without new", () => Uint8Array(2))
t("from", () => [String(Uint8Array.from([1, 2])), String(Uint8Array.from("123")), String(Uint8Array.from([1, 2], (x, i) => x * 10 + i)), String(Uint8Array.from([1], function (x) { return x + this.k }, { k: 4 })), String(Uint8Array.from(new Uint8Array([5])))])
t("from bad", () => Uint8Array.from(5))
t("of", () => String(Uint8Array.of(1, 2, 300)))
t("dates as bytes", () => [String(new Uint8Array([new Date(5), new Date(300)])), String(Uint8Array.of(new Date(7)))])
t("large default sort", () => { const x = new Uint8Array(5000).map((_, i) => (i * 31) % 256); x.sort(); return [x[0], x[19], x[20], x[4999], x.length] })
t("base64", () => [new Uint8Array([104, 105]).toBase64(), String(Uint8Array.fromBase64("aGk=")), new Uint8Array([255, 0]).toHex(), String(Uint8Array.fromHex("ff00"))])
t("fromBase64 invalid", () => Uint8Array.fromBase64("***"))
t("fromHex invalid", () => Uint8Array.fromHex("zz"))
t("fromHex non-string", () => Uint8Array.fromHex(5))
const b = new Uint8Array([5, 1, 4, 2, 3])
t("access", () => [b.length, b[0], b[9], b.at(-1), b.at(), b.indexOf(4), b.indexOf("4"), b.indexOf(4, 3), b.lastIndexOf(5), b.lastIndexOf(5, 0), b.lastIndexOf(5, -9), b.includes(2), b.includes(2, 4), b.join("-"), b.join(), b.toString()])
t("slice/subarray", () => { const x = new Uint8Array([1, 2, 3, 4]); const s = x.slice(1, 3); const v = x.subarray(1, 3); v[0] = 99; return [String(s), String(v), String(x), String(x.slice(-1)), String(x.subarray())] })
t("set", () => { const x = new Uint8Array(4); x.set([1, 2]); x.set(new Uint8Array([9]), 3); return String(x) })
t("set overflow", () => new Uint8Array(2).set([1, 2, 3]))
t("set bad offset", () => new Uint8Array(2).set([1], -1))
t("fill/reverse/sort", () => [String(new Uint8Array(4).fill(7, 1, 3)), String(new Uint8Array([1, 2, 3]).reverse()), String(new Uint8Array([10, 2, 33]).sort()), String(new Uint8Array([10, 2, 33]).sort((a, b) => b - a))])
t("index write", () => { const x = new Uint8Array(2); x[0] = 300; x[1] = "7"; x[5] = 1; return [String(x), x.length, 5 in x] })
t("delete index", () => { const x = new Uint8Array(1); return delete x[0] })
t("iterators", () => [[...new Uint8Array([1, 2])], [...new Uint8Array([1, 2]).keys()], [...new Uint8Array([1, 2]).entries()], [...new Uint8Array([3]).values()]])
t("callbacks", () => { const x = new Uint8Array([1, 2, 3]); return [String(x.map((v) => v * 100)), String(x.filter((v) => v > 1)), x.find((v) => v > 1), x.findIndex((v) => v > 5), x.findLast((v) => v < 3), x.findLastIndex((v) => v < 3), x.some((v) => v > 2), x.every((v) => v > 0), x.reduce((a, v) => a + v), x.reduceRight((a, v) => a + "" + v, ""), (() => { const out = []; x.forEach((v, i) => out.push(i)); return out })()] })
t("map returns bytes", () => Object.prototype.toString.call(new Uint8Array([1]).map((v) => v)))
t("reduce empty", () => new Uint8Array().reduce((a, b) => a + b))
t("receiver", () => Uint8Array.prototype.at.call([1], 0))
t("length receiver", () => Object.create(Uint8Array.prototype).length)
t("Object.keys", () => [Object.keys(new Uint8Array([4, 5])), Object.values(new Uint8Array([4, 5]))])
t("json", () => JSON.stringify(new Uint8Array([1, 2])))
t("spread to array", () => Array.from(new Uint8Array([7, 8])))
t("for-of", () => { let s = 0; for (const v of new Uint8Array([1, 2, 3])) s += v; return s })
return r`,
  `t("TextEncoder", () => { const e = new TextEncoder(); return [e.encoding, String(e.encode("hé")), String(e.encode()), String(e.encode(12)), Object.prototype.toString.call(e.encode("x"))] })
t("TextEncoder without new", () => TextEncoder())
t("TextDecoder", () => { const d = new TextDecoder(); return [d.encoding, d.fatal, d.ignoreBOM, d.decode(new Uint8Array([104, 195, 169])), d.decode(), d.decode(new Uint8Array([0xff]))] })
t("TextDecoder labels", () => [new TextDecoder("utf8").encoding, new TextDecoder(" UTF-8 ").encoding, new TextDecoder("unicode-1-1-utf-8").encoding])
t("TextDecoder unsupported", () => new TextDecoder("latin1"))
t("TextDecoder fatal", () => new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array([0xff])))
t("TextDecoder options", () => { const d = new TextDecoder("utf-8", { fatal: 1, ignoreBOM: true }); return [d.fatal, d.ignoreBOM, d.decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]))] })
t("TextDecoder BOM", () => new TextDecoder().decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41])))
t("TextDecoder bad input", () => new TextDecoder().decode("abc"))
t("TextDecoder without new", () => TextDecoder())
t("TextDecoder receiver", () => TextDecoder.prototype.decode.call({}, new Uint8Array(1)))
t("TextDecoder getter receiver", () => Object.create(TextDecoder.prototype).encoding)
t("roundtrip", () => new TextDecoder().decode(new TextEncoder().encode("ünïcödé ✓")))
t("btoa/atob", () => [btoa("hello"), atob("aGVsbG8="), btoa(""), atob(" aGk= "), btoa(123)])
t("btoa invalid", () => btoa("✓"))
t("atob invalid", () => atob("@@@"))
t("atob missing", () => atob())
t("btoa missing", () => btoa())
t("structuredClone", () => {
  const shared = { s: 1 }
  const src = { a: [1, { b: 2 }], m: new Map([[shared, shared]]), set: new Set([shared]), d: new Date(5), re: /x/gi, bytes: new Uint8Array([1, 2]), err: new RangeError("r", { cause: shared }), nested: { shared, again: shared }, hole: [1, , 3] }
  const c = structuredClone(src)
  return [c !== src, c.a[1].b, c.m.size, [...c.m.keys()][0] === [...c.m.values()][0], c.set.size, c.d.getTime(), String(c.re), String(c.bytes), c.bytes !== src.bytes, c.err.name, c.err.message, c.err.cause.s, c.err instanceof RangeError, c.nested.shared === c.nested.again, c.nested.shared !== shared, 1 in c.hole, c.hole.length]
})
t("structuredClone primitives", () => [structuredClone(1), structuredClone("s"), structuredClone(null), structuredClone(undefined) === undefined])
t("structuredClone cyclic object", () => { const o = { n: 1 }; const m = new Map(); m.set("self", m); const c = structuredClone(m); return c.get("self") === c })
t("structuredClone error kinds", () => { const e = new Error("plain"); e.name = "CustomName"; const c = structuredClone(e); return [c.name, c.message, Object.prototype.toString.call(c)] })
t("structuredClone function", () => structuredClone(() => 1))
t("structuredClone promise", () => structuredClone(Promise.resolve(1)))
t("structuredClone weakmap", () => structuredClone(new WeakMap()))
t("structuredClone symbol", () => structuredClone(Symbol.iterator))
t("structuredClone tool", () => structuredClone(tools))
t("structuredClone missing", () => structuredClone())
t("crypto", () => { const id = crypto.randomUUID(); const bytes = crypto.getRandomValues(new Uint8Array(4)); return [typeof id, id.length, /^[0-9a-f-]+$/.test(id), bytes.length, Object.prototype.toString.call(bytes)] })
t("crypto bad", () => crypto.getRandomValues([1, 2]))
t("crypto iterator", () => crypto.getRandomValues([].values()))
t("console bytes", () => { console.log(new Uint8Array([1, 2])); return true })
return r`,
])
