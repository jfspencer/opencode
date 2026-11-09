// Generates src/generated/ops.rs: the operations of the TS promise client (packages/client/src/promise/generated/
// client.ts), with their request descriptors and a namespace facade. `bun harness/gen-client.ts check` fails when the files are out of date.
//
// The TS client is itself generated, so its descriptors are regular: each operation is `name: (input...) =>
// request<...>({ method, path, query?, headers?, body?, binaryBody?, successStatus, declaredStatuses, empty,
// binary? }, requestOptions)`, optionally followed by `.then((value) => value.data)`; `sse<...>` for streams.
import { readFileSync, writeFileSync, existsSync } from "node:fs"
import path from "node:path"

const crate = path.resolve(import.meta.dir, "..")
const port = path.resolve(crate, "../../..")
const reference = path.join(port, ".reference")
const clientFile = path.join(reference, "packages/client/src/promise/generated/client.ts")

export type Pair = [key: string, input: string]
export type Segment = { literal: string } | { param: string; path: boolean }
export type Body = { kind: "none" } | { kind: "fields"; fields: Pair[] } | { kind: "input"; key: string; binary: boolean }
export type Op = {
  name: string[]
  optional: boolean
  method: string
  path: Segment[]
  query: Pair[]
  headers: Pair[]
  body: Body
  success: number
  declared: number[]
  empty: boolean
  binary: boolean
  sse: boolean
  data: boolean
}

// Every key of the evaluated input reads as "\u0001<key>\u0001"; encodeURIComponent and encodePath wrap it as
// "\u0002c<key>\u0002" and "\u0002p<key>\u0002".
const input = new Proxy({}, { get: (_, key) => `\u0001${String(key)}\u0001` })
type Descriptor = {
  method: string
  path: string
  query?: Record<string, string>
  headers?: Record<string, string>
  body?: string | Record<string, string>
  binaryBody?: boolean
  successStatus: number
  declaredStatuses: number[]
  empty: boolean
  binary?: boolean
}
function evaluate(text: string): Descriptor {
  const encode = (kind: string) => (value: string) => `\u0002${kind}${marker(value)}\u0002`
  return new Function("input", "encodeURIComponent", "encodePath", `return (${text})`)(input, encode("c"), encode("p"))
}

function marker(value: string) {
  const match = /^\u0001(.*)\u0001$/.exec(value)
  if (!match) throw new Error(`not an input key: ${JSON.stringify(value)}`)
  return match[1]
}

function markers(object: Record<string, string> | undefined): Pair[] {
  return Object.entries(object ?? {}).map(([key, value]) => [key, marker(value)])
}

function template(text: string): Segment[] {
  return text
    .split(/(\u0002[cp][^\u0002]*\u0002)/)
    .filter((part) => part !== "")
    .map((part) =>
      part.startsWith("\u0002") ? { param: part.slice(2, -1), path: part[1] === "p" } : { literal: part },
    )
}

function parse(source: string): Op[] {
  const start = source.indexOf("  const client = {\n")
  const end = source.indexOf("\n  return {\n", start)
  if (start < 0 || end < 0) throw new Error("client.ts: no `const client = {` section")
  const lines = source.slice(start, end).split("\n").slice(1)
  const ops: Op[] = []
  const stack: string[] = []
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    const indent = line.length - line.trimStart().length
    const depth = (indent - 4) / 2
    const ns = /^\s*([A-Za-z0-9_]+): \{$/.exec(line)
    if (ns) {
      stack.length = depth
      stack.push(ns[1])
      continue
    }
    const fn = /^\s*([A-Za-z0-9_]+): \((input(\?)?: [A-Za-z0-9_]+, )?requestOptions\?: RequestOptions\)(?:: AsyncIterable<[A-Za-z0-9_]+>)? =>$/.exec(
      line,
    )
    if (!fn) continue
    stack.length = depth
    // The descriptor runs to the closing `requestOptions,` of the call and the `)` after it. It is evaluated with
    // an input whose every key reads as a marker, so the path, query, header and body keys come out as markers.
    let j = i + 1
    const body: string[] = []
    while (!/^\s*requestOptions,$/.test(lines[j])) body.push(lines[j++])
    const close = lines[j + 1]
    const call = /^\s*(request|sse)<[^\n]*\($/.exec(body[0])
    if (!call) throw new Error(`client.ts: ${[...stack, fn[1]].join(".")}: no request call`)
    const descriptor = evaluate(body.slice(1).join("\n").replace(/,\s*$/, ""))
    const op: Op = {
      name: [...stack, fn[1]],
      optional: fn[3] === "?" || fn[2] === undefined,
      method: descriptor.method,
      path: template(descriptor.path),
      query: markers(descriptor.query),
      headers: markers(descriptor.headers),
      body:
        descriptor.body === undefined
          ? { kind: "none" }
          : typeof descriptor.body === "string"
            ? { kind: "input", key: marker(descriptor.body), binary: descriptor.binaryBody === true }
            : { kind: "fields", fields: markers(descriptor.body) },
      success: descriptor.successStatus,
      declared: descriptor.declaredStatuses,
      empty: descriptor.empty,
      binary: descriptor.binary === true,
      sse: call[1] === "sse",
      data: /\.then\(\(value\) => value\.data\)/.test(close ?? ""),
    }
    const known = new Set(["method", "path", "query", "headers", "body", "binaryBody", "successStatus", "declaredStatuses", "empty", "binary"])
    const unknown = Object.keys(descriptor).filter((key) => !known.has(key))
    if (unknown.length) throw new Error(`client.ts: ${op.name.join(".")}: unknown descriptor keys ${unknown}`)
    ops.push(op)
    i = j
  }
  return ops
}

const KEYWORDS = new Set([
  "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
  "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
  "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
  "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
])
const snake = (name: string) => {
  const out = name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").replace(/-/g, "_").toLowerCase()
  return KEYWORDS.has(out) ? `r#${out}` : out
}
const camel = (parts: string[]) => parts.map((part) => part[0].toUpperCase() + part.slice(1)).join("")
const str = (text: string) => JSON.stringify(text)
const pairList = (list: Pair[]) => `&[${list.map(([k, v]) => `(${str(k)}, ${str(v)})`).join(", ")}]`

function rust(ops: Op[]) {
  const out: string[] = []
  out.push(
    "// @generated by port/rust/crates/opencode-tui-sync/harness/gen-client.ts from the TS reference's promise client",
    "// (packages/client/src/promise/generated/client.ts). Do not edit: regenerate instead.",
    "",
    "use super::super::client::{Body, Client, Operation, Segment};",
    "use super::super::client::{Call, EventStream, RequestOptions};",
    "use opencode_protocol::Json;",
    "",
  )
  ops.forEach((op, index) => {
    const id = op.name.map((part) => snake(part).replace(/^r#/, "").toUpperCase()).join("_")
    const segments = op.path
      .map((segment) =>
        "literal" in segment
          ? `Segment::Literal(${str(segment.literal)})`
          : segment.path
            ? `Segment::Path(${str(segment.param)})`
            : `Segment::Param(${str(segment.param)})`,
      )
      .join(", ")
    const body =
      op.body.kind === "none"
        ? "Body::None"
        : op.body.kind === "fields"
          ? `Body::Fields(${pairList(op.body.fields)})`
          : op.body.binary
            ? `Body::Binary(${str(op.body.key)})`
            : `Body::Input(${str(op.body.key)})`
    out.push(`/// \`${op.name.join(".")}\`: ${op.method} ${op.path.map((s) => ("literal" in s ? s.literal : `{${s.param}}`)).join("")}`)
    out.push(`pub const ${id}: Operation = Operation {`)
    out.push(`    name: ${str(op.name.join("."))},`)
    out.push(`    method: ${str(op.method)},`)
    out.push(`    path: &[${segments}],`)
    out.push(`    query: ${pairList(op.query)},`)
    out.push(`    headers: ${pairList(op.headers)},`)
    out.push(`    body: ${body},`)
    out.push(`    success: ${op.success},`)
    out.push(`    declared: &[${op.declared.join(", ")}],`)
    out.push(`    empty: ${op.empty},`)
    out.push(`    binary: ${op.binary},`)
    out.push(`    stream: ${op.sse},`)
    out.push(`    data: ${op.data},`)
    out.push(`};`)
    out.push("")
    ;(op as Op & { id: string; index: number }).id = id
    ;(op as Op & { id: string; index: number }).index = index
  })
  out.push("/// Every operation, in the TS client's declaration order.")
  out.push("pub const OPERATIONS: &[&Operation] = &[")
  for (const op of ops) out.push(`    &${(op as Op & { id: string }).id},`)
  out.push("];")
  out.push("")
  // The namespace facade: `client.api().session().revert().stage(input)` for TS `api.session.revert.stage(input)`.
  type Node = { children: Map<string, Node>; ops: Op[] }
  const root: Node = { children: new Map(), ops: [] }
  for (const op of ops) {
    let node = root
    for (const part of op.name.slice(0, -1)) {
      if (!node.children.has(part)) node.children.set(part, { children: new Map(), ops: [] })
      node = node.children.get(part)!
    }
    node.ops.push(op)
  }
  const emit = (node: Node, names: string[]) => {
    const type = names.length ? `${camel(names)}Api` : "Api"
    out.push(names.length ? `/// \`api.${names.join(".")}\`.` : "/// The operations of a client, by the TS client's namespaces (`client.api()`).")
    out.push("#[derive(Clone, Copy)]")
    out.push(`pub struct ${type}<'a>(pub(crate) &'a Client);`)
    out.push("")
    out.push(`impl<'a> ${type}<'a> {`)
    for (const [child] of node.children) {
      out.push(`    pub fn ${snake(child)}(self) -> ${camel([...names, child])}Api<'a> {`)
      out.push(`        ${camel([...names, child])}Api(self.0)`)
      out.push("    }")
      out.push("")
    }
    for (const op of node.ops) {
      const id = (op as Op & { id: string }).id
      const last = op.name.at(-1)!
      const kind = op.sse ? "EventStream" : "Call"
      const method = op.sse ? "stream" : "call"
      out.push(`    /// \`${op.name.join(".")}(${op.optional ? "input?" : "input"}, requestOptions?)\``)
      out.push(`    pub fn ${snake(last)}(self, input: &Json, options: RequestOptions) -> ${kind} {`)
      out.push(`        self.0.${method}(&${id}, input, options)`)
      out.push("    }")
      out.push("")
    }
    if (out.at(-1) === "") out.pop()
    out.push("}")
    out.push("")
    for (const [child, sub] of node.children) emit(sub, [...names, child])
  }
  emit(root, [])
  while (out.at(-1) === "") out.pop()
  return out.join("\n") + "\n"
}

/** The TS client's operations (for the harness's request cases). */
export function operations(): Op[] {
  return parse(readFileSync(clientFile, "utf8"))
}

if (import.meta.main) {
  const files: [string, string][] = [
    [path.join(crate, "src/generated/ops.rs"), rust(operations())],
  ]
  if (process.argv[2] === "check") {
    const stale = files.filter(([file, text]) => !existsSync(file) || readFileSync(file, "utf8") !== text)
    if (stale.length) {
      console.error(`out of date: ${stale.map(([file]) => path.relative(crate, file)).join(", ")} (run bun harness/gen-client.ts)`)
      process.exit(1)
    }
  } else for (const [file, text] of files) writeFileSync(file, text)
}
