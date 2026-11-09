// Generates the targeted corpora (JSON lines): async ordering, tools, limits and output bounding, host extensions,
// hooks, scanTools, and value matrices that cross every operator, coercion, and common built-in with a spread of
// values. Deterministic.
//
// usage: bun harness/gen-cases.ts <async|tools|extensions|hooks|scan|chains|deep|matrix> > corpus/<name>.jsonl
const kind = process.argv[2]
const out: Array<Record<string, unknown>> = []
const add = (name: string, code: string, extra: Record<string, unknown> = {}) => out.push({ name: `${kind}/${name}`, code, ...extra })

const T = `const t = (f) => { try { return f() } catch (e) { return "!" + (e && e.name) + ": " + (e && e.message) } }\n`
const object = (properties: Record<string, unknown> = {}, required: string[] = []) => ({ type: "object", properties, ...(required.length ? { required } : {}) })

if (kind === "async") {
  const programs: Record<string, string> = {
    order1: `const log = []; const a = async () => { log.push("a1"); await null; log.push("a2"); await null; log.push("a3") }; const b = async () => { log.push("b1"); await null; log.push("b2") }; const pa = a(); const pb = b(); log.push("sync"); await pa; await pb; return log`,
    order2: `const log = []; Promise.resolve().then(() => log.push(1)).then(() => log.push(2)); queueMicrotask?.(() => {}); await null; log.push("x"); await null; log.push("y"); return log`,
    thenables: `const log = []; const th = { then(r) { log.push("then"); r(7) } }; const v = await th; log.push(v); const p = Promise.resolve(th); log.push("sync"); log.push(await p); return log`,
    finally: `const log = []; await Promise.resolve(1).finally(() => log.push("f1")).then((v) => log.push("v" + v)); await Promise.reject(new Error("x")).finally(() => log.push("f2")).catch((e) => log.push(e.message)); try { await Promise.resolve(1).finally(() => { throw new TypeError("cleanup") }) } catch (e) { log.push(e.name) } return log`,
    combinators: `const log = []; const r = await Promise.allSettled([1, Promise.reject(new Error("no")), Promise.resolve(3)]); log.push(JSON.stringify(r)); log.push(await Promise.race([new Promise(() => {}), Promise.resolve("fast")])); try { await Promise.any([Promise.reject(1), Promise.reject(2)]) } catch (e) { log.push(e.name, e.message, e.errors) } log.push(await Promise.all([])); return log`,
    raceOrder: `const log = []; const p1 = Promise.resolve(1); p1.then(() => log.push("p1 reaction")); Promise.race([p1]).then(() => log.push("race")); Promise.all([p1]).then(() => log.push("all")); await null; await null; await null; await null; return log`,
    unhandled: `Promise.reject(new Error("lost")); const p = Promise.reject(new TypeError("handled later")); await null; p.catch(() => {}); Promise.reject("str"); return "done"`,
    stuck: `await new Promise(() => {})`,
    stuckNested: `const f = async () => { await new Promise(() => {}) }; await f()`,
    generatorBasics: `function* g() { const x = yield 1; try { yield x * 2 } finally { yield "cleanup" } } const it = g(); return [it.next(), it.next(5), it.return(9), it.next(), it.next()]`,
    generatorThrow: `function* g() { try { yield 1 } catch (e) { yield "caught " + e } } const it = g(); return [it.next(), it.throw("boom"), it.next(), [...g()]]`,
    generatorRunning: `${T}function* g() { yield t(() => it.next()) } const it = g(); return it.next()`,
    asyncGenerator: `async function* ag() { yield 1; await null; yield 2; return 3 } const out = []; for await (const v of ag()) out.push(v); const it = ag(); return [out, await it.next(), await it.return(5), await it.next()]`,
    asyncGeneratorQueue: `const log = []; async function* ag() { log.push("start"); const a = yield "first"; log.push("got " + a); yield "second" } const it = ag(); const p1 = it.next("ignored"); const p2 = it.next("A"); const p3 = it.next("B"); log.push("queued"); log.push(JSON.stringify(await p1), JSON.stringify(await p2), JSON.stringify(await p3)); return log`,
    forAwaitSync: `const out = []; for await (const v of [1, Promise.resolve(2), 3]) out.push(v); return out`,
    delegate: `function* inner() { yield 1; return "r" } function* outer() { const r = yield* inner(); yield r; yield* [7, 8] } return [...outer()]`,
    asyncDelegate: `async function* inner() { yield 1; yield 2 } async function* outer() { yield* inner(); yield* [3, Promise.resolve(4)] } const out = []; for await (const v of outer()) out.push(v); return out`,
    awaitInLoop: `let s = 0; for (let i = 0; i < 50; i++) s += await i; return s`,
    recursionAsync: `const f = async (n) => n === 0 ? 0 : 1 + await f(n - 1); return await f(200)`,
    deepRecursion: `const f = (n) => n === 0 ? 0 : 1 + f(n - 1); return f(5000)`,
    tooDeep: `const f = (n) => n === 0 ? 0 : 1 + f(n - 1); try { return f(20000) } catch (e) { return [e.name, e.message] }`,
    tooDeepUncaught: `const f = (n) => 1 + f(n + 1); f(0)`,
    promiseCtor: `const log = []; const p = new Promise((resolve, reject) => { log.push("executor"); resolve(1); reject(2); resolve(3) }); log.push(await p); const q = new Promise(() => { throw new RangeError("in executor") }); try { await q } catch (e) { log.push(e.name) } return log`,
    selfResolve: `let res; const p = new Promise((r) => { res = r }); res(p); try { await p } catch (e) { return [e.name, e.message] }`,
    withResolvers: `const { promise, resolve } = Promise.withResolvers(); setTimeout?.(() => {}); resolve(42); return await promise`,
    tryStatic: `return [await Promise.try(() => 1), await Promise.try((a, b) => a + b, 2, 3), await Promise.try(() => { throw new Error("t") }).catch((e) => e.message)]`,
    awaitPlain: `const log = []; (async () => { log.push(1); await 0; log.push(3) })(); log.push(2); await 0; await 0; return log`,
    rejectionIdentity: `const p = Promise.reject(new Error("same")); let a, b; try { await p } catch (e) { a = e } try { await p } catch (e) { b = e } return a === b`,
    interpreterErrorIdentity: `const p = (async () => null.x)(); const r = await Promise.allSettled([p, p]); return [r[0].reason === r[1].reason, r[0].reason.name, r[0].reason.message]`,
    promiseProps: `const p = Promise.resolve(1); try { return p.value } catch (e) { return e.name + ": " + e.message }`,
    asyncReturnsPromise: `const f = async () => Promise.resolve(5); return [await f(), f() instanceof Promise]`,
    microtaskStorm: `const log = []; for (let i = 0; i < 5; i++) Promise.resolve(i).then((v) => log.push(v)); await Promise.resolve(); return log`,
    labels: `const out = []; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j === 1) continue outer; if (i === 2) break outer; out.push([i, j]) } } return out`,
  }
  for (const [name, code] of Object.entries(programs)) add(name, code)
  const tool = (timing: string) => [{ path: "t.echo", description: "echo", output: object(), timing }, { path: "t.fail", description: "fail", output: object(), behavior: { kind: "fail", message: "nope" }, timing }]
  for (const timing of ["sync", "yield"]) {
    add(`tools-order-${timing}`, `const log = []; const p = tools.t.echo({ a: 1 }); p.then(() => log.push("tool")); Promise.resolve().then(() => log.push("r1")).then(() => log.push("r2")); await null; log.push("a1"); await null; log.push("a2"); await p; log.push("after"); return log`, { tools: tool(timing) })
    add(`tools-all-${timing}`, `const r = await Promise.allSettled([tools.t.echo({ n: 1 }), tools.t.fail({}), tools.t.echo({ n: 2 })]); return r`, { tools: tool(timing) })
    add(`tools-unawaited-${timing}`, `tools.t.fail({}); tools.t.echo({}); return "left"`, { tools: tool(timing) })
    add(`tools-unawaited-await-${timing}`, `tools.t.fail({}); await tools.t.echo({}); return "left"`, { tools: tool(timing) })
    add(`tools-race-${timing}`, `return await Promise.race([tools.t.echo({ who: "a" }), tools.t.echo({ who: "b" })])`, { tools: tool(timing) })
    add(`tools-catch-${timing}`, `try { await tools.t.fail({ x: 1 }) } catch (e) { return [e.name, e.message, e instanceof Error, Object.keys(e)] }`, { tools: tool(timing) })
    add(`tools-uncaught-${timing}`, `await tools.t.fail({ x: 1 })`, { tools: tool(timing) })
    add(`tools-args-promise-${timing}`, `return await tools.t.echo({ v: Promise.resolve(3) })`, { tools: tool(timing) })
    add(`tools-arg-await-${timing}`, `return await tools.t.echo(Promise.resolve({ v: 4 }))`, { tools: tool(timing) })
  }
}

if (kind === "tools") {
  const inventory = [
    { path: "opencode.models", description: "Search the models available to use.", input: object({ query: { type: "string", description: "Text to search for." }, limit: { type: "integer", minimum: 1, maximum: 100, description: "Maximum number." } }), output: object({ providers: { type: "array", items: object({ id: { type: "string" } }, ["id"]) }, next: { anyOf: [{ type: "number" }, { type: "null" }] } }, ["providers"]) },
    { path: "opencode.session_rename", description: "Rename a session, or omit sessionID to rename the current session.", input: object({ sessionID: { type: "string", pattern: "^ses", description: "Omit to rename the current session." }, title: { type: "string", minLength: 1 } }, ["title"]), output: object({ title: { type: "string" } }) },
    { path: "opencode.worktree_list", description: "List a repository's worktrees.", input: object(), output: object({ worktrees: { type: "array" } }) },
    { path: "files.read_file", description: "Read a file from disk.", input: object({ path: { type: "string" } }, ["path"]), output: "nullOrString", behavior: { kind: "value", value: "contents" } },
    { path: "files.list-dir", description: "List a directory.", input: object({ path: { type: "string" } }), output: { type: "array", items: { type: "string" } }, behavior: { kind: "value", value: ["a", "b"] } },
    { path: "weird.voidTool", description: "Returns nothing.", input: object({ x: { type: "number" } }) },
    { path: "weird.enumTool", description: "Takes an enum.", input: object({ mode: { enum: ["a", "b"], default: "a" }, flag: { type: "boolean", deprecated: true } }), output: { type: "string" }, behavior: { kind: "sequence", values: [{ ok: "first" }, { fail: "second failed" }, { ok: "third" }] } },
    { path: "dotted.name.tool", description: "A dotted tool name.", input: object(), output: object() },
  ]
  const programs: Record<string, string> = {
    keys: `return [Object.keys(tools), Object.keys(tools.opencode), Object.keys(tools.files), typeof tools, typeof tools.opencode, typeof tools.opencode.models, typeof tools.nope, String(tools), "opencode" in tools, "nope" in tools]`,
    keysUnknown: `${T}return [t(() => Object.keys(tools.nope)), t(() => "x" in tools.nope)]`,
    forIn: `const out = []; for (const k in tools) out.push(k); for (const k in tools.files) out.push(k); return out`,
    call: `return [await tools.opencode.models({ query: "flash" }), await tools.files.read_file({ path: "/x" }), await tools.files["list-dir"]({}), await tools.weird.voidTool({ x: 1 })]`,
    sequence: `const out = []; for (let i = 0; i < 3; i++) { try { out.push(await tools.weird.enumTool({ mode: "b" })) } catch (e) { out.push(e.message) } } return out`,
    unknown: `await tools.opencode.modles({})`,
    unknownNs: `await tools.nothere.thing({})`,
    unknownSimilar: `await tools.files.read({})`,
    notCallable: `await tools.opencode({})`,
    root: `${T}return t(() => tools())`,
    twoArgs: `await tools.opencode.models({}, {})`,
    noArgs: `return await tools.opencode.worktree_list()`,
    searchAll: `return search()`,
    searchQuery: `return [search({ query: "rename session" }), search({ query: "file" }), search({ query: "zzz" })]`,
    searchNs: `return [search({ namespace: "files" }), search({ namespace: "tools.opencode", limit: 1 }), search({ namespace: "opencode", offset: 1, limit: 1 })]`,
    searchExact: `return [search({ query: "opencode.models" }).items.map((i) => i.path), search({ query: 'tools.files["list-dir"]' }).items.map((i) => i.path)]`,
    searchSignatures: `return search({ query: "" , limit: 20 }).items.map((i) => i.signature)`,
    searchThen: `const r = await search({ query: "models" }); const s = await search({ query: "models" }).then((v) => v.items.length); return [r.items.length, s, typeof search({}).then, Object.keys(search({}))]`,
    toolsSearch: `return await tools.search({ query: "worktree" })`,
    searchBad: `${T}return [t(() => search({ limit: 0 })), t(() => search(1)), t(() => search({ query: 1 }))]`,
    limits: `const out = []; for (let i = 0; i < 4; i++) { try { await tools.opencode.worktree_list({}); out.push("ok") } catch (e) { out.push(e.name + ": " + e.message) } } return out`,
    limitSearch: `search({}); search({}); try { search({}) } catch (e) { return e.message }`,
    pathSegments: `return [typeof tools.dotted.name.tool, await tools["dotted.name"].tool({}), await tools.dotted["name.tool"]({})]`,
    toolRefs: `${T}const r = tools.opencode.models; return [r === tools.opencode.models, t(() => r + 1), t(() => JSON.stringify(tools)), t(() => [1].map(tools.opencode.models)), t(() => new tools.opencode.models())]`,
    deleteTool: `${T}return [t(() => delete tools.opencode), t(() => { tools.x = 1 })]`,
    invalidOutput: `try { await tools.files.read_file({ path: 1 }) } catch (e) { return e.message }`,
    bigOutput: `return "x".repeat(300)`,
    bigLogs: `for (let i = 0; i < 20; i++) console.log("line " + i + " " + "y".repeat(20)); return "ok"`,
    bigWarnings: `for (let i = 0; i < 10; i++) Promise.reject(new Error("w" + i)); return 1`,
    bindingsUse: `return [boundValue, typeof boundObject, boundObject.nested.deep]`,
    bindingShadow: `const boundValue = 2; return boundValue`,
  }
  for (const [name, code] of Object.entries(programs)) {
    const extra: Record<string, unknown> = { tools: inventory }
    if (name === "limits" || name === "limitSearch") extra.limits = { maxToolCalls: 2 }
    if (name.startsWith("big")) extra.limits = { maxOutputBytes: 200 }
    if (name.startsWith("binding")) extra.bindings = { boundValue: 1, boundObject: { nested: { deep: [1, 2] } } }
    add(name, code, extra)
  }
  add("timeout", `console.log("before"); while (true) {}`, { limits: { timeoutMs: 50 } })
  add("timeoutAsync", `console.log("before"); await new Promise(() => {})`, { limits: { timeoutMs: 50 } })
}

if (kind === "extensions") {
  const web = [
    {
      name: "web",
      globals: {
        later: { kind: "echo", async: true },
        keep: { kind: "echo" },
        args: { kind: "args" },
        argsLater: { kind: "args", async: true },
        settings: { kind: "value", value: { retries: 3, nested: { deep: true }, list: [1, "two", null] } },
        fail: { kind: "throw", name: "TypeError", message: "bad thing", fields: { code: "E_BAD", errno: -2 } },
        failPlain: { kind: "throw", message: "plain" },
        failLater: { kind: "throw", name: "CustomError", message: "later bad", async: true },
        failRef: { kind: "throw", name: "ReferenceError", message: "ref", fields: { stack: "hidden", constructor: 1, path: "/x" } },
        fetch: { kind: "response", body: '{"a":[1,2],"b":"x"}' },
        fs: {
          kind: "object",
          members: {
            readFile: { kind: "echo", async: true },
            stat: { kind: "value", value: { size: 3 } },
            nested: { kind: "object", members: { deep: { kind: "args" } } },
          },
        },
      },
    },
  ]
  const programs: Record<string, string> = {
    basics: `return [await later(1), keep({ a: [1, 2] }), typeof later, later.name, later.length, settings(), typeof fs, typeof fs.readFile, await fs.readFile("p"), fs.stat(), fs.nested.deep(1, 2)]`,
    promiseShape: `const p = later(5); return [p instanceof Promise, typeof p.then, await p, await Promise.all([later(1), later(2)]), await Promise.race([later("a"), later("b")])]`,
    crossing: `${T}return [args(new Date(0), /x/g, new Map([[1, { a: 2 }]]), new Set([1, "s"]), new URL("https://a.b/c?d=1"), new Uint8Array([1, 2]), [1, , 3], new URLSearchParams("a=1&b=2"), new Headers({ "x-a": "1" }))]`,
    crossingNoBytes: `return args(new Date(0), /x/g, new Map([[1, { a: 2 }]]), new Set([1, "s"]), new URL("https://a.b/c?d=1"), [1, , 3], new URLSearchParams("a=1&b=2"), new Headers({ "x-a": "1" }), new Error("e"), -0, NaN, "\u{1F600}")`,
    bytesBack: `const r = await fetch("x"); const b = await r.bytes(); b[0] = 0; return [b instanceof Uint8Array, b[0], (await r.bytes())[0]]`,
    crossingTypes: `const [d, r, m, s, u, b, h, sp] = args(new Date(0), /x/gi, new Map([[1, 2]]), new Set([1]), new URL("https://a.b/c"), new Uint8Array([1, 2]), [1, , 3], new URLSearchParams("q=1")); return [d instanceof Date, d.getTime(), r instanceof RegExp, r.flags, m instanceof Map, m.get(1), s.has(1), u instanceof URL, u.href, b instanceof Uint8Array, b.length, 1 in h, h.length, sp.get("q")]`,
    rejected: `${T}return [t(() => args(() => 1)), t(() => args(Promise.resolve(1))), t(() => args(Symbol.iterator)), t(() => { const o = {}; o.self = [o]; return args(o) }), t(() => args(1, [2, { f() {} }])), t(() => args(new Error("x", { cause: () => 1 })))]`,
    rejectedLater: `${T}let r; try { await argsLater(() => 1) } catch (e) { r = e.message } return [r, t(() => argsLater(function named() {}))]`,
    sharedNotCircular: `const shared = { v: 1 }; return args([shared, shared], { a: shared, b: shared })`,
    errorsCross: `${T}let caught; try { fail() } catch (e) { caught = [e.name, e.message, e.code, e.errno, e instanceof TypeError, Object.keys(e), String(e)] } let later; try { await failLater() } catch (e) { later = [e.name, e.message, e instanceof Error, Object.keys(e)] } let ref; try { failRef() } catch (e) { ref = [e.name, e instanceof ReferenceError, e.path, e.constructor === ReferenceError, Object.keys(e)] } return [caught, later, ref, t(() => failPlain())]`,
    errorArgs: `const e = new TypeError("arg", { cause: { why: 1 } }); e.code = 7; const [back] = args(e); return [back instanceof TypeError, back.message, back.code, back.cause, Object.keys(back)]`,
    aggregateArg: `const [back] = args(new AggregateError([1], "agg")); return [back.name, back.message, back instanceof AggregateError]`,
    uncaught: `fail()`,
    uncaughtPlain: `failPlain()`,
    uncaughtLater: `await failLater()`,
    unhandledLater: `failLater(); return 1`,
    unhandledAfterJob: `failLater(); await null; return 1`,
    unhandledAfterHost: `failLater(); await later(1); return 1`,
    handledLate: `const p = failLater(); await later(1); try { await p } catch (e) { return e.message }`,
    completionOrder: `const log = []; const a = later("a").then(() => log.push("a")); const b = fs.readFile("b").then(() => log.push("b")); keep(1); await Promise.all([a, b]); return log`,
    fetchLike: `const res = await fetch("https://a.test/"); return [res.status, res.ok, res.headers.get("content-type"), res.headers.get("x"), await res.text(), await res.json(), [...(await res.bytes())].length, typeof res.json, res.json.name, res.json.length, res.headers.get.length, res.sync(), res.url, fetch.length]`,
    fetchUrlArg: `const res = await fetch(new URL("https://b.test/p")); return res.url`,
    ordering: `const log = []; const p = later("v"); p.then(() => log.push("later")); Promise.resolve().then(() => log.push("r1")).then(() => log.push("r2")).then(() => log.push("r3")); await null; log.push("a1"); await p; log.push("a2"); return log`,
    orderingSync: `const log = []; Promise.resolve().then(() => log.push("r1")); log.push(keep("k")); await null; return log`,
    notConstructible: `${T}return [t(() => new later()), t(() => new fs.readFile()), t(() => new keep())]`,
    asCallbacks: `return [[1, 2, 3].map(keep), [3, 1, 2].sort((a, b) => keep(a) - keep(b)), await Promise.all([1, 2].map(later))]`,
    thisAndCall: `return [keep.call(null, 4), keep.apply(null, [5]), keep.bind(null, 6)(), typeof keep.bind(null)]`,
    asValues: `${T}const o = { keep }; return [o.keep(1), JSON.stringify({ keep }), t(() => structuredClone(keep)), Object.keys(fs), typeof keep.prototype, keep === keep, fs.readFile === fs.readFile]`,
    returnFunction: `return keep`,
    returnObject: `return fs`,
    returnPromise: `return later(3)`,
    mutateGlobalObject: `${T}fs.extra = 1; fs.readFile = 2; return [fs.extra, fs.readFile, t(() => { keep = 1 })]`,
    shadowLocal: `const keep = (x) => x * 2; return keep(2)`,
    depth: `${T}let a = []; for (let i = 0; i < 40; i++) a = [a]; return [t(() => keep(a)), t(() => args(a))]`,
    depthOk: `let a = []; for (let i = 0; i < 20; i++) a = [a]; return JSON.stringify(keep(a)).length`,
    longArray: `${T}return [t(() => keep(new Array(20000).fill(0)).length), keep(new Array(5).fill(1))]`,
    deepResult: `${T}let s = settings(); s.nested.deep = false; return [settings().nested.deep, s.list]`,
    holes: `const r = keep([1, , 3]); return [r.length, 1 in r, r]`,
    objectKeys: `return keep({ b: 1, a: 2, 1: "x", 0: "y", [Symbol.iterator]: 1 })`,
    getterArg: `${T}let n = 0; const o = { get v() { n++; return n } }; return [t(() => keep(o)), n]`,
    classArg: `${T}class A { constructor() { this.x = 1 } } return [t(() => keep(new A())), t(() => keep(Object.create(null)))]`,
    awaitInLoop: `const out = []; for (let i = 0; i < 5; i++) out.push(await later(i)); return out`,
    forAwait: `const out = []; for await (const v of [later(1), later(2), 3]) out.push(v); return out`,
    asyncInAsync: `const f = async (x) => (await later(x)) + (await fs.readFile(x)); return [await f(1), await Promise.allSettled([failLater(), later(2)])]`,
    imports: `import fsModule, { readFile } from "fs"; return [typeof fsModule, typeof readFile]`,
    requireIt: `${T}return t(() => typeof require("fs"))`,
    logs: `console.log(keep("logged"), later); console.log(fs); return 1`,
  }
  for (const [name, code] of Object.entries(programs)) add(name, code, { extensions: web })
  add("twoExtensions", `return [a(1), b(2)]`, {
    extensions: [
      { name: "one", globals: { a: { kind: "echo" } } },
      { name: "two", globals: { b: { kind: "args" } } },
    ],
  })
  add("withTools", `return [await tools.echo({ x: keep(1) }), keep(await tools.echo({ y: 2 }))]`, {
    extensions: [{ name: "one", globals: { keep: { kind: "echo" } } }],
    tools: [{ path: "echo", input: { type: "object", properties: { x: { type: "number" }, y: { type: "number" } } } }],
  })
  add("withLimits", `const out = []; for (let i = 0; i < 3; i++) out.push(await later(i)); return out`, {
    extensions: [{ name: "one", globals: { later: { kind: "echo", async: true } } }],
    limits: { maxToolCalls: 1 },
  })
}

if (kind === "hooks") {
  const tools = [
    { path: "echo", input: { type: "object", properties: { x: {} } }, output: { type: "object" } },
    { path: "slow", input: { type: "object", properties: { x: {} } }, output: { type: "object" }, timing: "yield" },
    { path: "fail", behavior: { kind: "fail", message: "nope" }, timing: "yield" },
    { path: "text", behavior: { kind: "value", value: 5 }, output: "nullOrString" },
    { path: "files.read", input: { type: "object", properties: { path: { type: "string" } } }, output: { type: "object" } },
  ]
  const extensions = [
    {
      name: "web",
      globals: {
        fetch: { kind: "response", body: '{"a":1}' },
        keep: { kind: "echo" },
        later: { kind: "echo", async: true },
        boom: { kind: "throw", message: "boom" },
        boomLater: { kind: "throw", message: "boom later", async: true },
        fs: { kind: "object", members: { readFile: { kind: "echo", async: true }, stat: { kind: "value", value: 1 } } },
      },
    },
  ]
  const programs: Record<string, string> = {
    sequential: `await tools.echo({ x: 1 }); await tools.slow({ x: 2 }); return await tools.files.read({ path: "a" })`,
    concurrent: `return await Promise.all([tools.slow({ x: 1 }), tools.echo({ x: 2 }), tools.slow({ x: 3 }), tools.echo()])`,
    failures: `return await Promise.allSettled([tools.fail(), tools.text(), tools.echo({ x: 1 })])`,
    interrupted: `tools.slow({ x: "never awaited" }); tools.echo({ x: 1 }); return 1`,
    interruptedByError: `tools.slow({ x: 1 }); later(2); await tools.fail()`,
    limit: `await tools.echo({ x: 1 }); await tools.echo({ x: 2 }); return await tools.echo({ x: 3 })`,
    invalidInput: `${T}return [await tools.echo(1, 2).catch((e) => e.message), await tools.echo(() => 1).catch((e) => e.message)]`,
    search: `return [await tools.search({ query: "read" }), await tools.echo({ x: 1 })]`,
    extensionCalls: `${T}const res = await fetch("https://a.test/"); return [await res.json(), keep(1), await later(2), t(() => boom()), await boomLater().catch((e) => e.message), await fs.readFile("p"), fs.stat(), res.sync()]`,
    extensionArgs: `${T}return [keep(1, 2, 3), t(() => keep(() => 1)), await later()]`,
    extensionInterrupted: `later(1); fs.readFile("x"); return keep(0)`,
    mixedOrder: `const a = later(1); const b = tools.slow({ x: 1 }); const c = fs.readFile(2); return [await c, await b, await a]`,
    unknownTool: `${T}return [await tools.nope().catch((e) => e.message), await tools.echo({ x: 1 })]`,
  }
  for (const [name, code] of Object.entries(programs)) {
    add(name, code, { tools, extensions, hooks: true, ...(name === "limit" ? { limits: { maxToolCalls: 2 } } : {}) })
  }
}

if (kind === "scan") {
  const programs: Record<string, string> = {
    upstreamOrder: `
      const chapters = await tools["hex-meetings"].get_chapters({ meeting_id: "m1" })
      const thread = await tools.slack.read_thread({ ts: await tools.slack.latest_ts({}) })
      await tools.slack.read_thread({ ts: "again" })
      const ignored = { tools: 1 }.tools
      return { wake: true, state: chapters }
    `,
    upstreamShadowed: `
      function helper(tools) {
        return tools[dynamicKey]()
      }
      {
        const tools = { any: () => 1 }
        tools.any()
      }
      return helper({ [dynamicKey]: () => ({ wake: false }) })
    `,
    empty: `   `,
    emptyUnicode: ` ﻿ `,
    syntax: `const x = {`,
    exported: `export const a = 1`,
    dynamic: `return await tools.slack[name]({})`,
    aliased: `const s = tools.slack; return await s.read_thread({})`,
    passed: `return Object.keys(tools)`,
    bare: `tools`,
    numberIndex: `tools[0]()`,
    templateIndex: "tools[`a`]()",
    stringIndexNested: `tools["a"]["b-c"].d()`,
    optional: `tools.a?.b?.(); tools?.c(); (tools.d)?.()`,
    newCall: `new tools.a()`,
    tagged: "tools.a`x`",
    assigned: `tools.a = 1`,
    deleted: `delete tools.a`,
    awaitedChain: `await (await tools.a()).b()`,
    resultMember: `(await tools.a()).b.c()`,
    callResultCall: `tools.a()()`,
    switchOrder: `switch (tools.first()) { case tools.second(): tools.third(); default: tools.fourth() }`,
    labelOrder: `tools: for (const x of [tools.a()]) { tools.b(); break tools }`,
    objectKeys: `const o = { tools: 1, [tools.k()]: tools.v(), tools() {} }; o.tools`,
    shorthand: `const o = { tools }`,
    shorthandPattern: `const { tools } = x; tools.a()`,
    patternKey: `const { tools: t } = x; tools.a()`,
    patternDefault: `const { t = tools.def() } = x`,
    assignmentPattern: `({ tools } = x); tools.b()`,
    varHoisted: `tools.a(); if (x) { var tools = 1 }`,
    varInFunction: `tools.a(); function f() { var tools = 1 }`,
    varInForIn: `tools.a(); for (var tools in x) {}`,
    letInFor: `for (let tools = 0; ; ) { tools.a() } tools.b()`,
    forOfConst: `for (const tools of x) tools.a(); tools.b()`,
    catchParam: `try { tools.a() } catch (tools) { tools.b() } tools.c()`,
    catchPattern: `try {} catch ({ tools }) { tools.b() } tools.c()`,
    switchLet: `switch (x) { case 1: let tools = 1 } tools.a()`,
    blockFunction: `{ function tools() {} } tools.a()`,
    blockClass: `{ class tools {} } tools.a()`,
    topFunction: `tools.a(); function tools() {}`,
    arrowParam: `const f = (tools) => tools.x(); tools.y()`,
    arrowExpressionScope: `const f = ({ a: [tools] }) => tools.x(); tools.y()`,
    restParam: `function g(...tools) { tools.x() } tools.y()`,
    namedFunctionExpression: `const h = function tools() { tools.x() }; tools.y()`,
    classMembers: `class A { tools() {} static tools = 1; get [tools.k()]() {} } tools.z()`,
    classField: `class A { tools = 1 }`,
    classStaticVar: `tools.a(); x = class { static { var tools = 1 } }`,
    staticBlockVar: `class A { static { var tools } } tools.a()`,
    // D514-D516: a static block is a var scope, a function body's declarations stay out of its parameter defaults,
    // and paths are in source order.
    staticBlockInner: `x = class { static { var tools; tools.inner() } }; await tools.outer()`,
    staticBlockLet: `class A { static { let tools; tools.x() } } tools.y()`,
    staticBlockFunction: `class A { static { function tools() {} } } tools.a()`,
    staticBlockNestedVar: `class A { static { if (x) { var tools } tools.a() } } tools.b()`,
    paramDefaultVar: `const f = function (a = tools.danger({})) { var tools }; await f()`,
    paramDefaultLet: `const f = (a = tools.early()) => { let tools; return tools.late() }`,
    paramShadows: `const f = function (tools = 1) { return tools.none() }`,
    paramDefaultNestedVar: `function f(a = tools.a()) { if (x) { var tools } tools.b() } tools.c()`,
    methodParamDefault: `class A { m(a = tools.a()) { var tools; tools.b() } } ({ n(b = tools.c()) { const tools = 1 } })`,
    bodyBlockLet: `function f() { { let tools } tools.a() }`,
    caseOrder: `switch (x) { case tools.b(): tools.c(); case tools.d(): tools.e() }`,
    memberProperty: `x.tools.a(); x?.tools`,
    labels: `tools: { break tools } tools.a()`,
    continueLabel: `tools: for (;;) { continue tools }`,
    importBinding: `import { tools } from "x"`,
    importAs: `import { a as tools } from "x"`,
    importOther: `import fs from "fs"; tools.a()`,
    importNamespace: `import * as tools from "x"`,
    metaProperty: `function f() { new.target } tools.a()`,
    templateExpr: "`${tools.a()} and ${tools.b()}`",
    spreadArgs: `tools.a(...tools.b())`,
    sequence: `(tools.a(), tools.b())`,
    computedLiteralNumber: `tools.a[1]()`,
    privateName: `class A { #x; m() { tools.#x } }`,
    unicodeEscape: `tools.\\u0061()`,
    escapedToolsName: `t\\u006fols.a()`,
    stringLiteralUnicode: `tools["\\u{1F600}"]()`,
    usingDecl: `{ using tools = x } tools.a()`,
    deep: `${"(".repeat(200)}tools.a()${")".repeat(200)}`,
    deepArrays: `${"[".repeat(500)}tools.a()${"]".repeat(500)}`,
    withStatement: `with (x) { tools.a() }`,
    exportNested: `{ export const a = tools.a() }`,
    yieldIn: `function* g() { yield tools.a() }`,
    getterSetter: `const o = { get tools() { return tools.a() }, set tools(v) {} }`,
    multiline: `const a = 1\n\n   const b = tools\n.c\n.d\n(tools.e)`,
    errorAfterFound: `tools.a(); tools.b(); const s = tools`,
    duplicate: `tools.a(); tools.a(); tools["a"](); tools.b()`,
    callee: `(0, tools.a)()`,
    conditional: `(x ? tools.a : tools.b)()`,
    // Depth: TS's recursive walks throw past about 7,250 levels (up to 11,500 warm); these stay clear of the band.
    deepMembersOk: `tools.a(); x${".b".repeat(6000)}`,
    deepMembers: `tools.a(); x${".b".repeat(20000)}`,
    deepInArrow: `tools.a(); (() => x${".b".repeat(20000)})`,
    deepInFunction: `(function () { tools.a(); x${"()".repeat(20000)} })`,
    deepToolPath: `tools${".b".repeat(6000)}()`,
    deepParens: `${"(".repeat(5000)}1${")".repeat(5000)}`,
  }
  for (const [name, code] of Object.entries(programs)) add(name, code, { scanOnly: true })
}

if (kind === "chains") {
  // Chains the parser builds in a loop (acorn takes any length) and the walks over them: short ones run, long ones
  // overflow TS's JS stack and fail with a RangeError the program catches. The lengths stay clear of the thresholds,
  // which move with JIT warm-up (about 17,000 to 30,000 for `+` and `.b`, 15,000 for TS's tree walks, 7,250 to
  // 11,500 for scanTools, whose walk these programs also go through).
  for (const n of [3000, 45000]) {
    const plus = "1" + "+1".repeat(n)
    const member = "x" + ".b".repeat(n)
    const shapes: Record<string, string> = {
      top: `try { return ${plus} } catch (e) { return [e.name, e.message] }`,
      uncaught: `const x = {}; ${member}`,
      argumentsWalk: `${T}function g() { console.log("body"); return ${plus} + arguments.length } return t(g)`,
      arrowNoWalk: `${T}const g = () => { console.log("body"); return ${plus} }; return t(g)`,
      loopWalk: `${T}return t(() => { for (let i = 0; i < 1; i++) { console.log("body"); ${plus} } })`,
      forOfWalk: `${T}const x = {}; return t(() => { for (const v of [1]) { console.log("body"); ${member} } })`,
      notAFunction: `${T}const x = { b: {} }; return t(() => x${".b".repeat(Math.min(n, 4000))}.c())`,
      callChain: `${T}const f = () => f; return t(() => typeof f${"()".repeat(Math.min(n, 3000) * (n > 3000 ? 5 : 1))})`,
      optional: `${T}const x = {}; return t(() => x${"?.b".repeat(n / 3)})`,
      assignTarget: `${T}const x = {}; return t(() => { ${member} = 1 })`,
      logical: `${T}return t(() => 1${"&&1".repeat(n / 3)})`,
      tagged: `${T}const tag = () => tag; return t(() => typeof tag${"`x`".repeat(n > 3000 ? n / 3 : n / 5)})`,
      inCallback: `${T}return t(() => [1].map(() => ${plus}))`,
      comma: `return (${Array(n).fill("1").join(",")})`,
    }
    for (const [name, code] of Object.entries(shapes)) add(`${name} ${n}`, code)
  }
}

if (kind === "deep") {
  // Deep program structure on a 2 MiB stack: 1,000 levels run in TS (the undeclared `x` shows the innermost level
  // was reached), 20,000 overflow acorn or the interpreter. Between about 2,500 and 5,000 levels TS's outcome
  // depends on JIT warm-up, so no case sits there.
  const shapes: Record<string, (n: number) => string> = {
    blocks: (n) => "{".repeat(n) + "x = 1" + "}".repeat(n) + "; return 1",
    ifs: (n) => "if (1) ".repeat(n) + "x = 1; return 1",
    elseIfs: (n) => "if (0) {} " + "else if (0) {} ".repeat(n) + "else { x = 2 } return 1",
    labels: (n) => Array.from({ length: n }, (_, i) => `l${i}: `).join("") + "x = 1; return 1",
    whiles: (n) => "while (true) { ".repeat(n) + "break; " + "} break; ".repeat(n - 1) + "} return 1",
    tries: (n) => "try { ".repeat(n) + "x = 1" + " } finally {}".repeat(n) + "; return 1",
    switches: (n) => "switch (1) { case 1: ".repeat(n) + "x = 1" + " }".repeat(n) + "; return 1",
    fors: (n) => "for (let i = 0; i < 1; i++) ".repeat(n) + "x = 1; return 1",
    parens: (n) => "return " + "(".repeat(n) + "1" + ")".repeat(n),
    arrays: (n) => "return " + "[".repeat(n) + "1" + "]".repeat(n) + ".length",
    arrows: (n) => "return " + "(() => ".repeat(n) + "1" + ")()".repeat(n),
    functions: (n) => "return " + "(function () { return ".repeat(n) + "1" + " })()".repeat(n),
    unary: (n) => "return " + "!".repeat(n) + "1",
    ternary: (n) => "return " + "1 ? ".repeat(n) + "1" + " : 0".repeat(n),
    assign: (n) => "let a; return " + "a = ".repeat(n) + "1",
    exponent: (n) => "return " + "1 ** ".repeat(n) + "1",
    template: (n) => "return " + "`${".repeat(n) + "1" + "}`".repeat(n),
    spread: (n) => "return [" + "...[".repeat(n) + "1" + "]".repeat(n) + "]",
    arrayPattern: (n) => "const " + "[".repeat(n) + "a" + "]".repeat(n) + " = " + "[".repeat(n) + "1" + "]".repeat(n) + "; return a",
    objectPattern: (n) => "const " + "{a:".repeat(n) + "b" + "}".repeat(n) + " = " + "{a:".repeat(n) + "1" + "}".repeat(n) + "; return b",
    callArgs: (n) => "const f = (x) => x; return " + "f(".repeat(n) + "1" + ")".repeat(n),
  }
  for (const [name, make] of Object.entries(shapes)) add(`${name} 1000`, make(1000))
  for (const name of ["blocks", "ifs", "tries", "parens", "arrays", "arrayPattern"]) add(`${name} 20000`, shapes[name](20000))
}

if (kind === "matrix") {
  const values = [`undefined`, `null`, `true`, `false`, `0`, `-0`, `1`, `-1.5`, `NaN`, `Infinity`, `""`, `"0"`, `"1e3"`, `" 12 "`, `"abc"`, `[]`, `[1]`, `[1,2]`, `{}`, `{ valueOf() { return 3 } }`, `{ toString() { return "s" } }`, `new Date(0)`, `/x/g`, `Symbol.iterator`]
  const binary = ["+", "-", "*", "/", "%", "**", "==", "!=", "===", "!==", "<", "<=", ">", ">=", "&", "|", "^", "<<", ">>", ">>>"]
  for (const op of binary) {
    const rows = values.map((a) => `[${values.map((b) => `t(() => (${a}) ${op} (${b}))`).join(", ")}]`).join(",\n")
    add(`binary ${op}`, `${T}return [\n${rows}\n]`)
  }
  for (const op of ["+", "-", "~", "!", "typeof ", "void "]) {
    add(`unary ${op.trim()}`, `${T}return [${values.map((v) => `t(() => ${op}(${v}))`).join(", ")}]`)
  }
  const coercions = ["String", "Number", "Boolean", "parseInt", "parseFloat", "isNaN", "isFinite", "JSON.stringify", "Object.keys", "Array.isArray", "Number.isInteger", "Math.abs", "Math.round", "encodeURIComponent"]
  for (const fn of coercions) add(`coerce ${fn}`, `${T}return [${values.map((v) => `t(() => ${fn}(${v}))`).join(", ")}]`)
  const numbers = [`0`, `-0`, `1`, `0.1`, `-1.5`, `123.456`, `1e21`, `1e-7`, `5e-324`, `1.7976931348623157e308`, `NaN`, `-Infinity`, `2**53`, `0.000001`, `1234.5678`, `-0.0005`, `255`, `1/3`]
  for (const method of ["toFixed(2)", "toFixed()", "toPrecision(3)", "toPrecision()", "toExponential(2)", "toExponential()", "toString(16)", "toString(2)", "toString(36)", "toLocaleString()"]) {
    add(`number ${method}`, `${T}return [${numbers.map((n) => `t(() => (${n}).${method})`).join(", ")}]`)
  }
  const strings = [`""`, `"abc"`, `"  Hello World  "`, `"a,b,,c"`, `"ß"`, `"İ"`, `"😀x"`, `"\\ud800"`, `"aAbB"`, `"12px"`, `"x".repeat(5)`]
  for (const method of ["toUpperCase()", "toLowerCase()", "trim()", "split(',')", "split('')", "split()", "slice(1, -1)", "substring(2, 1)", "substr(-2)", "at(-1)", "charAt(1)", "charCodeAt(0)", "codePointAt(0)", "indexOf('b')", "lastIndexOf('a')", "includes('b')", "startsWith('a')", "endsWith('c')", "padStart(8, '-')", "padEnd(6)", "repeat(2)", "replace('a', '$&$&')", "replaceAll('a', 'x')", "normalize('NFD')", "localeCompare('b')", "match(/./g)", "search(/b/)", "isWellFormed()", "toWellFormed()", "concat(1, [2])", "[Symbol.iterator]().next()"]) {
    add(`string ${method}`, `${T}return [${strings.map((s) => `t(() => (${s}).${method})`).join(", ")}]`)
  }
  const arrays = [`[]`, `[3, 1, 2]`, `[1, , 3]`, `["b", "a", 10, 9]`, `[undefined, null, 0]`, `[[1, [2]], [3]]`, `Array(3)`, `[{ a: 1 }, { a: 0 }]`]
  for (const method of ["join()", "join('-')", "toString()", "slice(1)", "concat([9], 8)", "flat()", "flat(Infinity)", "includes(undefined)", "indexOf(1)", "lastIndexOf(3)", "at(-1)", "toSorted()", "toReversed()", "with(0, 'w')", "toSpliced(1, 1)", "keys().toArray()", "entries().toArray()", "map((x) => typeof x)", "filter(Boolean)", "reduce((a, b) => a + '' + b, '')", "findLast((x) => x)", "some((x) => x === undefined)", "every(Boolean)", "fill(7, 1)", "copyWithin(0, 1)", "reverse()", "sort()", "sort((a, b) => a - b)", "splice(1)", "flatMap((x) => [x, x])", "toLocaleString()"]) {
    add(`array ${method}`, `${T}return [${arrays.map((a) => `t(() => ${a}.${method})`).join(", ")}]`)
  }
  add("collation ascii", `const cs = []; for (let c = 32; c < 127; c++) cs.push(String.fromCharCode(c)); return cs.map((a) => cs.map((b) => a.localeCompare(b)).join(""))`)
  add("collation words", `const w = ["a", "A", "b", "B", "á", "Á", "ab", "aB", "Ab", "a b", "a-b", "a_b", "a1", "a10", "a2", "é", "e", "f", "ß", "ss", "z", "Z", "Ö", "o", "ö", "ő", "0", "9", "", " ", "zebra", "Zebra", "apple", "Apple", "file.txt", "file-1", "file_2", "File10", "résumé", "resume", "naïve", "naive", "😀", "中", "日本", "Ωmega", "omega", "αβ", "Ä", "ä", "Æ", "æ"]; return [...w].sort((a, b) => a.localeCompare(b)).concat(w.map((a) => w.map((b) => a.localeCompare(b)).join(",")))`)
  for (const [label, n] of [["calls ok", 1000], ["calls deep", 30000]] as const) add(`nesting ${label}`, `const f = () => f; f${"()".repeat(n)}; return 1`)
  for (const [label, n] of [["binary ok", 5000], ["binary deep", 100000]] as const) add(`nesting ${label}`, `return 1${"+1".repeat(n)}`)
  for (const [label, n] of [["logical ok", 3000], ["logical deep", 60000]] as const) add(`nesting ${label}`, `return 1${"&&1".repeat(n)}`)
  add("nesting caught", `try { return 1${"+1".repeat(100000)} } catch (e) { return [e.name, e.message] }`)
  add("nesting string deep", `${T}let a = []; for (let i = 0; i < 50000; i++) a = [a]; return [t(() => String(a).length), t(() => a.flat(Infinity).length), t(() => structuredClone(a) && 1)]`)
  add("nesting string ok", `${T}let a = []; for (let i = 0; i < 500; i++) a = [a]; return [t(() => String(a).length), t(() => a.flat(Infinity).length), t(() => structuredClone(a) && 1)]`)
  add("console formats", `console.log("%s|%d|%i|%f|%o|%O|%c|%%|%x", "s", "42.9px", 3.7, "1.5e2", {a: 1}, [1], "css", "extra", 5); console.log(1, "two", [3], { four: 4 }, null, undefined, () => 1, new Map([[1, {a: 1}]]), new Set([1]), new Date(0), /r/g, new Error("e"), new TypeError(), Symbol.iterator); console.warn("w"); console.error("e"); console.debug("d"); console.info("i"); console.dir({ deep: { er: [1, { x: "y" }] } }); console.table([{ a: 1, b: 2 }, { a: 3, c: 4 }]); console.table([[1, 2], [3]]); console.table({ x: { v: 1 }, y: 2 }, ["v"]); console.log(); return null`)
  add("dates", `${T}const d = new Date(Date.UTC(2024, 1, 29, 13, 5, 9, 7)); return [d.toISOString(), d.toString(), d.toUTCString(), d.toDateString(), d.toTimeString(), d.toLocaleString(), d.toLocaleDateString(), d.toLocaleTimeString(), d.getDay(), d.getTimezoneOffset(), JSON.stringify(d), t(() => new Date(NaN).toISOString()), String(new Date(NaN)), Date.parse("2020-01-02"), Date.parse("Tue, 01 Mar 2022 10:00:00 GMT"), Date.parse("March 7, 2021"), new Date(2020, 0, 31).setMonth(1), Date.UTC(99), new Date(8.64e15 + 1).getTime(), d - new Date(0), d + 1]`)
  add("urls", `${T}const u = new URL("https://user:pw@example.com:8080/a/b/../c?x=1&y=2#frag"); const before = u.href; u.searchParams.append("z", "3 4"); u.hash = ""; u.pathname = "/new path"; const p = new URLSearchParams("a=1&b=2&a=3"); p.sort(); return [before, u.href, u.origin, u.host, u.searchParams.get("x"), [...u.searchParams], p.toString(), p.getAll("a"), t(() => new URL("not a url")), URL.canParse("/x", "https://e.com"), new URL("/p?q", "https://e.com/base/").href, encodeURI("a b/é?#"), encodeURIComponent("a b/é?#"), decodeURIComponent("%E2%82%AC"), t(() => decodeURIComponent("%E2%82")), JSON.stringify({ u }), new URLSearchParams({ k: "v", n: 1 }).toString()]`)
  // Non-special authorities the `url` crate accepts and WHATWG rejects: credentials without a host, a port that is
  // not digits (a backslash belongs to a non-special authority).
  add("url authorities", `${T}return ["foo://@", "foo://-:\\\\x", "foo://a\\\\b", "foo://a:1:2", "foo://x:abc", "foo://u@", "foo://@/x", "foo://x:1/y", "https://@host", "foo://", "foo://u:p@h:12/x", "sc://@h", "sc://:@/", "foo://h:\\\\1"].map((s) => [s, URL.canParse(s), URL.parse(s)?.href ?? null, t(() => new URL(s).href), URL.canParse(s, "https://b.example/")])`)
  add("bytes", `${T}const b = new Uint8Array([1, 255, 256, -1, 3.7]); return [[...b], b.length, b.toHex(), b.toBase64(), [...Uint8Array.fromBase64("AQID")], [...new TextEncoder().encode("é😀")], new TextDecoder().decode(new Uint8Array([0xe2, 0x82, 0xac, 0xff])), t(() => new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array([0xff]))), String(b), JSON.stringify(b), b.subarray(1, 3).fill(9) && [...b], atob("aGVsbG8="), btoa("hi"), t(() => btoa("é😀")), t(() => atob("***")), structuredClone(new Map([[1, [2]]])).get(1)]`)
  add("collections", `${T}const m = new Map([[NaN, 1], [0, "zero"]]); m.set(-0, "negzero"); const s = new Set([1, 1, "1", NaN, NaN]); const seen = []; const live = new Map([[1, 1], [2, 2]]); live.forEach((v, k) => { seen.push(k); if (k === 1) { live.delete(2); live.set(3, 3) } }); return [m.get(NaN), m.get(0), m.size, [...m.keys()], [...s], s.size, seen, Object.groupBy([1, 2, 3, 4], (x) => (x % 2 ? "odd" : "even")), [...Map.groupBy([1, 2, 3], (x) => x > 1)], new Set([1, 2]).union(new Set([2, 3])).size, t(() => new WeakMap().set(1, 1)), JSON.stringify([m, s])]`)
  add("objects", `${T}const o = { b: 1, a: 2, 10: "x", 2: "y", [Symbol.iterator]: null }; const frozen = Object.freeze({ k: 1 }); return [Object.keys(o), JSON.stringify(o), Object.entries({ x: [1] }), Object.assign({}, { a: 1 }, null, "hi"), Object.fromEntries([["a", 1], ["b", 2]]), t(() => { "use strict"; frozen.k = 2 }), Object.isFrozen(frozen), Object.getPrototypeOf([]) === Array.prototype, ({}).toString.call([]), Object.prototype.toString.call(null), t(() => Object.create(1)), { ...[1, 2], ..."hi", ...null }, structuredClone({ d: new Date(0), r: /x/gi })]`)
  add("errors", `${T}const e = new Error("msg", { cause: 1 }); const te = new TypeError("tt"); const ag = new AggregateError([1, 2], "agg"); class_ = 1; return [String(e), e.cause, Object.keys(e), te instanceof Error, te.name, String(ag), ag.errors, Error("no new").message, t(() => { throw { custom: 1 } }), t(() => null.x), t(() => undefined.y.z), t(() => x.y), t(() => { const c = 1; c = 2 }), t(() => [].reduce((a, b) => a)), JSON.stringify(e), Error.isError(e), Error.isError({})]`)
  add("uncaught object", `throw { custom: 1, list: [1, 2] }`)
  add("uncaught string", `throw "plain"`)
  add("uncaught number", `throw 42`)
  add("uncaught error subclass", `throw new RangeError("ranged")`)
  add("uncaught function", `throw () => 1`)
  add("uncaught located", `const o = null;\n\n  o.prop`)
  add("rethrow keeps location", `try { null.x } catch (e) { throw e }`)
  add("unsupported class", `class A {}`)
  add("unsupported getter", `const o = { get x() { return 1 } }`)
  add("unsupported new.target", `function f() { return new.target }`)
  add("parse error", `return 1 +`)
  add("parse error 2", `let x = ;`)
  add("parse error regex", `/(/`)
  add("export", `export const x = 1`)
  add("empty", `   \n `)
  add("imports", `${T}import { URL as U } from "url"; import * as util from "node:util"; const u = require("url"); return [typeof U, Object.keys(util), Object.keys(u), t(() => require("fs")), t(() => require(1)), typeof (await import("buffer")).atob]`)
  add("import missing", `import fs from "fs"`)
  add("this and arguments", `${T}function f() { return [typeof this, arguments.length, [...arguments]] } const o = { m() { return this === o }, a: () => typeof this }; return [f(1, 2), o.m(), o.a(), f.call("x", 1), f.apply(null, [1, 2, 3]), f.bind(null, 9)(8), f.bind(null).name, f.length]`)
  add("hoisting", `${T}return [t(() => before), typeof fn, fn(), t(() => { tdz; let tdz = 1 })]; var before = 1; function fn() { return "hoisted" }`)
  add("destructuring", `${T}const [a, , b = 5, ...rest] = [1, 2, undefined, 4, 5]; const { x, y: { z } = { z: 9 }, ...others } = { x: 1, w: 2, v: 3 }; let p, q; [p, q] = [q, p] = [1, 2]; return [a, b, rest, x, z, others, p, q, t(() => { const { m } = null }), t(() => { const [n] = 1 })]`)
  add("templates", `function tag(s, ...v) { return [s, s.raw, v] } const n = 1; return [\`a\${n}b\${{ toString() { return "S" } }}\`, tag\`x\${1}y\\n\${2}\`, String.raw\`\\u{41}\${n}\`]`)
  add("closures in loops", `const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); const gs = []; for (const k of [7, 8]) gs.push(() => k); return [fs.map((f) => f()), gs.map((g) => g())]`)
  add("switch", `const r = []; for (const v of [1, 2, 3, "1"]) { switch (v) { case 1: r.push("one"); case 2: r.push("two"); break; case "1": r.push("str"); break; default: r.push("default") } } return r`)
  add("getters builtin", `const re = /a/g; re.lastIndex = "3"; const m = new Map([[1, 2]]); return [re.lastIndex, m.size, re.source, re.flags, [].length, "abc".length]`)
  add("flat and concat", `${T}const a = [1, [2, [3, [4, , 5]]], , 6]; let c = [0]; for (let i = 0; i < 13000; i++) c = [c]; return [a.flat(), a.flat(2), a.flat(Infinity), a.flat(0), a.flat(NaN), a.flat(-1), a.flat("2"), [, 1].flat(), t(() => c.flat(Infinity)), c.flat(3).length, [1].concat(2, [3, , 4], [[5]], new Array(2)), [].concat.length, [, ,].concat([, 1]), Object.keys([1, , 2].concat([, 3])), t(() => new Array(9999999).concat([1, 2])), t(() => [new Array(9999999).fill(1), [1, 2]].flat()).slice(0, 30), new Array(9999999).concat([1]).length]`)
  add("regex", `${T}const re = /(?<y>\\d{4})-(?<m>\\d{2})/g; const s = "2020-01 and 2021-12"; return [s.replace(re, "$<m>/$<y>"), [...s.matchAll(re)].map((m) => [m.index, m.groups.y]), re.exec(s), re.lastIndex, s.split(/(\\d)/, 4), "aBc".replace(/b/i, (m) => m.toUpperCase() + "!"), t(() => new RegExp("[")), t(() => new RegExp("a", "gg")), /\\u{1F600}/u.test("😀"), "x".replace(/(?:)/g, "-"), RegExp.escape("a.b*c")]`)
}

for (const item of out) process.stdout.write(JSON.stringify(item) + "\n")
