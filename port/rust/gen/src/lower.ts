// Lowers Effect Schema ASTs to a small intermediate representation of their wire (encoded, JSON) side.
//
// Decoding a node with an encoding chain runs, in Effect v4: parse(last link's `to`) -> transformation.decode ->
// parse(the node with the remaining links) (see effect/src/internal/schema/interpreter.ts). The lowering follows
// the same structure: it lowers the wire-most node, then applies each recognized transformation. A transformation
// is either transparent on the wire (classes, the `optional` helper, bijective literal mappings), or one whose
// decode the Rust side reproduces (NumberFromString, Trim, DateTimeUtcFromMillis, ArrayEnsure, decoding defaults,
// catchDecoding, JSON strings), or a known custom one that is reported as not reproduced. Anything else stops the
// generator, so an upstream change that adds a transformation cannot slip through unnoticed.
import { Effect, Exit, Option, Schema, SchemaAST, SchemaIssue, SchemaTransformation } from "effect"
import type { Wire } from "./roots.ts"

export type Check =
  | { readonly k: "minLength"; readonly n: number }
  | { readonly k: "maxLength"; readonly n: number }
  | { readonly k: "pattern"; readonly source: string; readonly flags: string }
  | { readonly k: "startsWith"; readonly s: string }
  | { readonly k: "endsWith"; readonly s: string }
  | { readonly k: "includes"; readonly s: string }
  | { readonly k: "trimmed" }
  | { readonly k: "int" }
  | { readonly k: "finite" }
  | { readonly k: "gt"; readonly n: number }
  | { readonly k: "gte"; readonly n: number }
  | { readonly k: "lt"; readonly n: number }
  | { readonly k: "lte"; readonly n: number }
  | { readonly k: "unique" }

export type Lit = string | number | boolean

export type Ty =
  | { readonly t: "ref"; readonly name: string }
  | { readonly t: "string"; readonly checks: Check[] }
  /** A JSON number. With an `int` check it is a safe integer. */
  | { readonly t: "number"; readonly checks: Check[] }
  /** `Schema.Number` under the JSON codec: a finite number, or "NaN" / "Infinity" / "-Infinity". */
  | { readonly t: "jsnumber"; readonly checks: Check[] }
  | { readonly t: "boolean" }
  | { readonly t: "null" }
  | { readonly t: "literal"; readonly value: Lit }
  | { readonly t: "never" }
  /** Any JSON value; `nonNull` for an empty struct, which accepts and returns any non-null value unchanged. */
  | { readonly t: "json"; readonly nonNull: boolean }
  | { readonly t: "array"; readonly item: Ty; readonly checks: Check[] }
  | { readonly t: "tuple"; readonly elements: ReadonlyArray<{ ty: Ty; optional: boolean }>; readonly rest: Ty[]; readonly checks: Check[] }
  | { readonly t: "record"; readonly key: Ty; readonly value: Ty; readonly checks: Check[] }
  | { readonly t: "nullable"; readonly inner: Ty }
  /** `Schema.NumberFromString`: a string on the wire, read with JS `Number(s)`, written back with `String(n)`. */
  | { readonly t: "numberString"; readonly wire: Check[]; readonly num: Check[] }
  /** `Schema.Trim`: decoding trims (JS `String.prototype.trim`), then the checks apply. */
  | { readonly t: "trim"; readonly wire: Check[]; readonly checks: Check[] }
  /** `DateTimeUtcFromMillis`: epoch milliseconds, truncated to an integer by `new Date(n)`, |n| <= 8.64e15. */
  | { readonly t: "millis" }
  /** `Schema.ArrayEnsure`: a single item or an array of items on input, always an array on output. */
  | { readonly t: "arrayEnsure"; readonly item: Ty }
  /** Internal: the single-item branch of ArrayEnsure before it is folded. */
  | { readonly t: "wrap1"; readonly item: Ty }
  /** `Schema.fromJsonString`: a string holding JSON text of `inner`. */
  | { readonly t: "jsonString"; readonly inner: Ty }
  /** `Uint8Array` under the JSON codec: base64 text. */
  | { readonly t: "base64" }
  /** `Schema.DurationFromString`: Effect duration text, normalized to `<n> millis` / `<n> nanos` when written. */
  | { readonly t: "duration" }
  /** Field-level: absent on input means `value` (the wire JSON of the decoding default). */
  | { readonly t: "default"; readonly inner: Ty; readonly value: unknown }
  /** Field-level: a value that fails to decode is dropped, as if the key were absent (`Schema.catchDecoding`). */
  | { readonly t: "catchNone"; readonly inner: Ty }
  /** A transformation the Rust side does not reproduce; only its wire shape is checked. */
  /**
   * A custom transformation. With a `hook`, Rust reproduces it with a hand-written JSON -> JSON function
   * (crate::wire::hook): the input must decode as `wire`, the hook's output as `target`. Without one, only `wire`
   * is checked.
   */
  | { readonly t: "custom"; readonly name: string; readonly wire: Ty; readonly hook?: string; readonly target?: Ty }

export interface Field {
  readonly key: string
  readonly ty: Ty
  /** May be absent on input. */
  readonly optional: boolean
  /** Wire JSON written when the key was absent on input (decoding default). */
  readonly default?: unknown
  /** Invalid values are dropped instead of rejected. */
  readonly catchNone?: boolean
  readonly doc?: string
}

export interface DefMeta {
  name: string
  /** Name before collision suffixes: the identifier (prio 0), the root hint (prio 1) or a derived hint (prio 2). */
  readonly base: string
  readonly prio: number
  readonly wire: Wire
  readonly origin: string
}

export type Def = DefMeta &
  (
  | {
      readonly kind: "struct"
      readonly doc?: string
      readonly fields: Field[]
      /** Index signatures (records / struct-with-rest), in declaration order. */
      readonly rest: Array<{ key: Ty; value: Ty }>
      readonly checks: Check[]
    }
  | { readonly kind: "enum"; readonly doc?: string; readonly values: Lit[] }
  | { readonly kind: "union"; readonly doc?: string; readonly members: Ty[]; readonly oneOf: boolean }
  | { readonly kind: "newtype"; readonly doc?: string; readonly ty: Ty }
  )

type Own = { readonly base: string; readonly prio: number }

export interface Note {
  readonly kind: "unenforced" | "custom" | "hook" | "limit"
  readonly where: string
  readonly what: string
}

export const typeName = (identifier: string) => {
  const name = identifier
    .split(/[^A-Za-z0-9]+/)
    .filter((p) => p.length > 0)
    .map((p) => p[0].toUpperCase() + p.slice(1))
    .join("")
  return /^[0-9]/.test(name) ? `T${name}` : name || "Anonymous"
}

type GetterLike = { readonly _tag?: string; readonly transform?: (...args: never[]) => unknown }
const src = (g: GetterLike | undefined) => (g?.transform ? String(g.transform).replace(/\s+/g, " ") : "")

export class Lowering {
  readonly defs = new Map<string, Def>()
  readonly notes: Note[] = []
  private readonly cache = new Map<SchemaAST.AST, Ty>()
  private readonly lowering = new Set<SchemaAST.AST>()
  private seq = 0
  private wire: Wire = "json"
  private origin = ""

  lowerRoot(ast: SchemaAST.AST, hint: string, wire: Wire, origin: string): Ty {
    this.wire = wire
    this.origin = origin
    return this.lower(ast, hint, { base: hint, prio: 1 })
  }

  private note(kind: Note["kind"], what: string) {
    const where = this.origin
    if (!this.notes.some((n) => n.kind === kind && n.what === what)) this.notes.push({ kind, where, what })
  }

  /** A provisional name; final names are assigned after deduplication (model.ts). */
  private reserve(own: Own | undefined, hint: string): DefMeta {
    return { name: `#${this.seq++}`, base: own?.base ?? hint, prio: own?.prio ?? 2, wire: this.wire, origin: this.origin }
  }

  /** `hint` names anonymous definitions created below this node; `adopt` is a name reserved for this node itself. */
  private lower(ast: SchemaAST.AST, hint: string, adopt?: Own): Ty {
    const key = this.cacheKey(ast)
    const cached = this.cache.get(key)
    if (cached) return cached
    // Structs reserve their definition before lowering their fields, so recursion through a struct resolves to a
    // reference. Recursion that never passes through a struct is not supported.
    if (this.lowering.has(key)) throw new Error(`recursive schema without a struct in the cycle at ${this.origin}`)
    this.lowering.add(key)
    const identifier = SchemaAST.resolveIdentifier(ast)
    const own = identifier === undefined ? adopt : { base: typeName(identifier), prio: 0 }
    const out = ast.encoding ? this.lowerChain(ast, hint, own) : this.lowerLocal(ast, hint, own)
    this.lowering.delete(key)
    // An identified primitive, array, record or codec becomes a newtype so the brand survives in Rust.
    // (A hooked custom transformation already named its target after the identifier.)
    const named = out.t === "custom" && out.target?.t === "ref"
    if (identifier !== undefined && out.t !== "ref" && !fieldLevel(out) && out.t !== "never" && !named) {
      const meta = this.reserve(own, hint)
      this.defs.set(meta.name, { ...meta, kind: "newtype", doc: docOf(ast), ty: out })
      const ref: Ty = { t: "ref", name: meta.name }
      this.cache.set(key, ref)
      return ref
    }
    this.cache.set(key, out)
    return out
  }

  // A node's wire type depends on its body, its encoding chain and the wire context, not on its context (optional
  // keys). Effect's "context owner" identifies the body: copies that change only context or encoding share it, so
  // the encoding chain is part of the key.
  private cacheKey(ast: SchemaAST.AST): SchemaAST.AST {
    const owner = SchemaAST.getContextOwner(ast)
    let byWire = this.keys.get(this.wire)
    if (!byWire) this.keys.set(this.wire, (byWire = new WeakMap()))
    let entry = byWire.get(owner)
    if (!entry) byWire.set(owner, (entry = { plain: Object.create(owner), byEncoding: new WeakMap() }))
    if (!ast.encoding) return entry.plain
    let key = entry.byEncoding.get(ast.encoding)
    if (!key) entry.byEncoding.set(ast.encoding, (key = Object.create(owner) as SchemaAST.AST))
    return key
  }
  private readonly keys = new Map<
    Wire,
    WeakMap<SchemaAST.AST, { plain: SchemaAST.AST; byEncoding: WeakMap<object, SchemaAST.AST> }>
  >()

  private lowerLocal(ast: SchemaAST.AST, hint: string, own: Own | undefined): Ty {
    const checks = this.checks(ast.checks)
    switch (ast._tag) {
      case "String":
        return { t: "string", checks }
      case "Number":
        return { t: "number", checks }
      case "Boolean":
        return { t: "boolean" }
      case "Null":
        return { t: "null" }
      case "Undefined":
      case "Void":
      case "Never":
        // JSON has no undefined: such members never decode from JSON text.
        return { t: "never" }
      case "Literal":
        if (typeof ast.literal === "bigint") throw new Error(`bigint literal at ${this.origin}`)
        return { t: "literal", value: ast.literal }
      case "Unknown":
      case "Any":
        return { t: "json", nonNull: false }
      case "ObjectKeyword":
        throw new Error(`ObjectKeyword at ${this.origin}`)
      case "Declaration":
        return this.lowerDeclaration(ast)
      case "TemplateLiteral":
        return { t: "string", checks: [{ k: "pattern", source: templatePattern(ast), flags: "" }, ...checks] }
      case "Arrays":
        if (ast.elements.length === 0 && ast.rest.length === 1)
          return { t: "array", item: this.lower(ast.rest[0], `${hint}Item`), checks }
        return {
          t: "tuple",
          elements: ast.elements.map((e, i) => ({
            ty: this.lower(e, `${hint}${i}`),
            optional: SchemaAST.isOptional(SchemaAST.getLastEncoding(e)),
          })),
          rest: ast.rest.map((r, i) => this.lower(r, `${hint}Rest${i}`)),
          checks,
        }
      case "Objects":
        return this.lowerObjects(ast, hint, own, checks)
      case "Union":
        return this.lowerUnion(ast, hint, own, checks)
      case "Suspend":
        return this.lower(ast.thunk(), hint, own)
      case "Enum":
        return this.literals(
          ast.enums.map(([, v]) => v),
          hint,
          own,
        )
      default:
        throw new Error(`unsupported AST ${ast._tag} at ${this.origin}`)
    }
  }

  private lowerDeclaration(ast: SchemaAST.Declaration): Ty {
    const expected = (ast.annotations as { expected?: string } | undefined)?.expected
    const id = (ast.annotations as { representation?: { id?: string } } | undefined)?.representation?.id
    if (id === "effect/schema/Json" || id === "effect/schema/MutableJson" || expected === "JSON value")
      return { t: "json", nonNull: false }
    throw new Error(`unsupported declaration ${expected ?? id ?? SchemaAST.resolveIdentifier(ast)} at ${this.origin}`)
  }

  private lowerObjects(ast: SchemaAST.Objects, hint: string, own: Own | undefined, checks: Check[]): Ty {
    if (ast.propertySignatures.length === 0 && ast.indexSignatures.length === 0) {
      if (checks.length) this.note("unenforced", "checks on an empty struct")
      return { t: "json", nonNull: true }
    }
    if (ast.propertySignatures.length === 0 && own === undefined) {
      if (ast.indexSignatures.length !== 1) throw new Error(`record with several index signatures at ${this.origin}`)
      const index = ast.indexSignatures[0]
      return {
        t: "record",
        key: this.lowerKey(index.parameter, `${hint}Key`),
        value: this.lower(index.type, `${hint}Value`),
        checks,
      }
    }
    const meta = this.reserve(own, hint)
    const ref: Ty = { t: "ref", name: meta.name }
    this.cache.set(this.cacheKey(ast), ref)
    const fields: Field[] = []
    const def = { ...meta, kind: "struct" as const, doc: docOf(ast), fields, rest: [] as Array<{ key: Ty; value: Ty }>, checks }
    this.defs.set(meta.name, def)
    for (const ps of ast.propertySignatures) {
      if (typeof ps.name !== "string") throw new Error(`non-string property key at ${this.origin}`)
      // Effect looks keys up with `key in input`: an absent key named like an Object.prototype member reads the
      // inherited function instead, which Rust (an own-key lookup) does not reproduce.
      if (ps.name !== "__proto__" && ps.name in Object.prototype)
        this.note("limit", `field \`${ps.name}\` is an Object.prototype name: TS reads the inherited value when it is absent`)
      const wire = SchemaAST.getLastEncoding(ps.type)
      const optional = SchemaAST.isOptional(wire)
      const ty = this.lower(ps.type, `${def.base}${typeName(ps.name)}`)
      const doc = docOf(ps.type) ?? (ps.type.context?.annotations as { description?: string } | undefined)?.description
      if (ty.t === "default") fields.push({ key: ps.name, ty: ty.inner, optional, default: ty.value, doc })
      else if (ty.t === "catchNone") {
        if (!optional) throw new Error(`catchDecoding on a required key at ${this.origin}`)
        fields.push({ key: ps.name, ty: ty.inner, optional, catchNone: true, doc })
      } else fields.push({ key: ps.name, ty, optional, doc })
    }
    for (const index of ast.indexSignatures) {
      def.rest.push({ key: this.lowerKey(index.parameter, `${def.base}Key`), value: this.lower(index.type, `${def.base}Value`) })
    }
    return ref
  }

  private lowerKey(parameter: SchemaAST.AST, hint: string): Ty {
    const key = this.lower(parameter, hint)
    const resolved = key.t === "ref" ? this.defs.get(key.name) : undefined
    const inner = resolved?.kind === "newtype" ? resolved.ty : key
    if (inner.t !== "string") throw new Error(`unsupported record key ${JSON.stringify(key)} at ${this.origin}`)
    return key
  }

  private lowerUnion(ast: SchemaAST.Union, hint: string, own: Own | undefined, checks: Check[]): Ty {
    if (checks.length) this.note("unenforced", "checks on a union")
    const reservedName = own?.base ?? hint
    const members = ast.types
      .map((m, i) => this.lower(m, `${reservedName}${i}`))
      .filter((m) => m.t !== "never")
    const oneOf = ast.options?.mode === "oneOf"
    if (members.length === 0) return { t: "never" }
    // ArrayEnsure: [one item, many items]
    if (
      members.length === 2 &&
      members[0].t === "wrap1" &&
      members[1].t === "array" &&
      JSON.stringify(members[0].item) === JSON.stringify(members[1].item)
    )
      return { t: "arrayEnsure", item: members[1].item }
    if (!oneOf && members.length === 1) return members[0]
    if (!oneOf && members.every((m) => m.t === "literal"))
      return this.literals(
        members.map((m) => (m as { value: Lit }).value),
        hint,
        own,
      )
    const nulls = members.filter((m) => m.t === "null")
    const rest = members.filter((m) => m.t !== "null")
    // `X | null`: only when no other member could also take null.
    if (!oneOf && nulls.length > 0 && rest.length > 0 && rest.every((m) => !this.acceptsNull(m))) {
      const inner = rest.length === 1 ? rest[0] : this.lowerUnionDef(rest, hint, own, false)
      return { t: "nullable", inner }
    }
    return this.lowerUnionDef(members, hint, own, oneOf)
  }

  private lowerUnionDef(members: Ty[], hint: string, own: Own | undefined, oneOf: boolean): Ty {
    if (members.every((m) => m.t === "literal"))
      return this.literals(
        members.map((m) => (m as { value: Lit }).value),
        hint,
        own,
      )
    const meta = this.reserve(own, hint)
    this.defs.set(meta.name, { ...meta, kind: "union", members, oneOf })
    return { t: "ref", name: meta.name }
  }

  private acceptsNull(ty: Ty): boolean {
    if (ty.t === "null" || (ty.t === "json" && !ty.nonNull)) return true
    if (ty.t === "nullable") return true
    if (ty.t === "ref") {
      const def = this.defs.get(ty.name)
      if (def?.kind === "newtype") return this.acceptsNull(def.ty)
      if (def?.kind === "union") return def.members.some((m) => this.acceptsNull(m))
      if (def?.kind === "enum") return false
    }
    if (ty.t === "custom") return this.acceptsNull(ty.wire)
    return false
  }

  private literals(values: Lit[], hint: string, own: Own | undefined): Ty {
    if (values.length === 1) return { t: "literal", value: values[0] }
    const meta = this.reserve(own, hint)
    this.defs.set(meta.name, { ...meta, kind: "enum", values })
    return { t: "ref", name: meta.name }
  }

  private lowerChain(ast: SchemaAST.AST, hint: string, own: Own | undefined): Ty {
    const links = ast.encoding!
    const last = links[links.length - 1]
    const inner = last.to
    const t = last.transformation as unknown as {
      _tag: string
      decode: GetterLike & ((...args: never[]) => unknown)
      encode: GetterLike
    }
    // The node with the remaining links: what the transformed value is parsed with next.
    const rest = links.length > 1 ? SchemaAST.replaceEncoding(ast, links.slice(0, -1) as never) : SchemaAST.getContextOwner(ast)
    const restLocal = links.length > 1 ? undefined : rest
    const dec = src(t.decode)
    const enc = src(t.encode)
    const decTag = t.decode?._tag
    const encTag = t.encode?._tag

    if (t._tag === "Middleware") {
      if (this.isCatchNone(t)) return { t: "catchNone", inner: this.lower(inner, hint, own) }
      throw new Error(`unsupported middleware at ${this.origin}`)
    }
    if (t === (SchemaTransformation.numberFromString as unknown)) {
      const wire = this.lower(inner, hint)
      if (wire.t !== "string") throw new Error(`NumberFromString over ${wire.t} at ${this.origin}`)
      return this.refine({ t: "numberString", wire: wire.checks, num: [] }, rest, hint)
    }
    // Schema.Number under toCodecJson: finite | "NaN" | "Infinity" | "-Infinity"
    if (decTag === "Transform" && dec.includes("function Number()") && enc.includes("isFinite(n) ? n : globalThis.String(n)"))
      return { t: "jsnumber", checks: this.checks(restLocal?.checks) }
    if (dec === "() => { return; }" && enc === "() => null") return { t: "null" }
    if (dec.includes("self.trim()") && encTag === "Passthrough") {
      const wire = this.lower(inner, hint)
      if (wire.t !== "string") throw new Error(`Trim over ${wire.t} at ${this.origin}`)
      return this.refine({ t: "trim", wire: wire.checks, checks: [] }, rest, hint)
    }
    if (dec.includes("DateTime.makeUnsafe(value)") && enc.includes("DateTime.toEpochMillis(value)")) return { t: "millis" }
    if (dec === "(value) => [value]" && enc === "([value]) => value") return { t: "wrap1", item: this.lower(inner, hint) }
    if (dec.includes("JSON.parse(input")) return { t: "jsonString", inner: this.lower(rest, hint, own) }
    if (dec.includes("Base64.decode(input)")) return { t: "base64" }
    if (dec.includes("Duration_.fromInput(s)")) return { t: "duration" }
    if (decTag === "TransformOptionalEffect" && dec.includes("defaultValue")) {
      const wire = this.lower(inner, hint)
      return { t: "default", inner: wire, value: this.decodingDefault(t, ast) }
    }
    // Custom transformations the Rust side does not reproduce (their wire shape is still exact).
    const custom = customTransformation(dec, SchemaAST.resolveIdentifier(ast))
    if (custom?.hook) {
      if (restLocal === undefined) throw new Error(`custom transformation in a multi-stage chain at ${this.origin}`)
      this.note("hook", custom.note)
      const target = this.lower(rest, hint, own)
      const wire = this.lower(inner, `${own?.base ?? hint}Input`)
      return { t: "custom", name: custom.note, wire, hook: custom.hook, target }
    }
    if (custom) {
      this.note("custom", custom.note)
      return { t: "custom", name: custom.note, wire: this.lower(inner, hint, own) }
    }
    // Transparent on the wire: the value is unchanged (or mapped one-to-one between constants).
    const transparent =
      (decTag === "Passthrough" && (encTag === "Passthrough" || encTag === "TransformOptional")) ||
      (dec.includes("makeClassTrusted(self, input)") && encTag === "Passthrough") ||
      (dec === '(value) => value === "true"' && enc === '(value) => value ? "true" : "false"') ||
      (dec === "() => null" && enc === '() => "null"') ||
      (dec.startsWith("() => Version.make(") && enc === "() => input.durable.version")
    if (transparent) {
      // When `rest` still decodes something (HttpApi's optional query/header properties: the flattened wire
      // shape, then the property's own chain), the value is decoded by `rest` from the same wire value, and `inner`
      // only pre-validates its shape. Otherwise `rest` is a type-side stage: a class, the `optional` helper's
      // `T | undefined`, or extra checks.
      if (rest._tag !== "Declaration" && SchemaAST.toType(rest) !== rest) {
        this.scanCustom(inner)
        return this.lower(rest, hint, own)
      }
      return this.refine(this.lower(inner, hint, own), rest, hint)
    }
    throw new Error(`unsupported transformation at ${this.origin}: ${t._tag} decode=${decTag}:${dec} encode=${encTag}:${enc}`)
  }

  /** Notes custom transformations in an encoding chain that is not lowered itself. */
  private scanCustom(ast: SchemaAST.AST) {
    for (const link of ast.encoding ?? []) {
      const custom = customTransformation(src((link.transformation as { decode?: GetterLike }).decode), SchemaAST.resolveIdentifier(ast))
      if (custom) this.note(custom.hook ? "hook" : "custom", custom.note)
      this.scanCustom(link.to)
    }
  }

  /** After a transformation, the value is parsed by `rest`; for the codecs here that adds checks. */
  private refine(ty: Ty, rest: SchemaAST.AST, hint: string): Ty {
    if (rest.encoding) {
      // Another stage: rest = inner2 -> t2 -> rest2. Only transparent stages are expected here.
      const inner = this.lower(rest, hint)
      if (JSON.stringify(inner) === JSON.stringify(ty) || inner.t === "ref") return ty
      throw new Error(`unsupported multi-stage transformation at ${this.origin}`)
    }
    const checks = this.checks(rest.checks)
    switch (rest._tag) {
      case "Declaration":
      case "Undefined":
        return ty
      case "Union":
        // The `optional` helper's type side: the decoded type or undefined.
        return ty
      case "Objects":
      case "Arrays":
        if (SchemaAST.toType(rest) !== rest && ty.t !== "custom")
          throw new Error(`transparent transformation into a re-decoding structure at ${this.origin}`)
        return checks.length ? this.addChecks(ty, checks) : ty
      default:
        return checks.length ? this.addChecks(ty, checks) : ty
    }
  }

  private addChecks(ty: Ty, checks: Check[]): Ty {
    const merge = (a: Check[]) => [...a, ...checks.filter((c) => !a.some((x) => JSON.stringify(x) === JSON.stringify(c)))]
    switch (ty.t) {
      case "string":
      case "number":
      case "jsnumber":
      case "array":
      case "tuple":
      case "record":
        return { ...ty, checks: merge(ty.checks) }
      case "numberString":
        return { ...ty, num: merge(ty.num) }
      case "trim":
        return { ...ty, checks: merge(ty.checks) }
      case "literal":
        return ty
      case "ref": {
        const def = this.defs.get(ty.name)
        if (def?.kind === "newtype" && JSON.stringify(merge(checksOf(def.ty))) === JSON.stringify(checksOf(def.ty))) return ty
        if (def?.kind === "newtype") return this.addChecks(def.ty, checks)
        break
      }
    }
    throw new Error(`cannot add checks ${JSON.stringify(checks)} to ${ty.t} at ${this.origin}`)
  }

  private isCatchNone(t: { decode: (...args: never[]) => unknown }): boolean {
    const failing = Effect.fail(new SchemaIssue.InvalidValue(Option.some("x")))
    const decode = t.decode as unknown as (e: unknown, o: unknown) => Effect.Effect<Option.Option<unknown>, unknown>
    const exit = Effect.runSyncExit(decode(failing, {}))
    return Exit.isSuccess(exit) && Option.isNone(exit.value)
  }

  private decodingDefault(t: { decode: GetterLike }, ast: SchemaAST.AST): unknown {
    const transform = t.decode.transform as unknown as (o: Option.Option<unknown>, opts: unknown) => Effect.Effect<Option.Option<unknown>>
    const value = Effect.runSync(transform(Option.none(), {}))
    if (Option.isNone(value)) throw new Error(`decoding default produced nothing at ${this.origin}`)
    // The default is a decoded value; write it back as wire JSON.
    return JSON.parse(JSON.stringify(Schema.encodeSync(Schema.make(ast) as never)(value.value as never)))
  }

  private checks(checks: SchemaAST.Checks | undefined): Check[] {
    if (!checks) return []
    const out: Check[] = []
    const visit = (check: SchemaAST.Check<unknown>) => {
      if (check._tag === "FilterGroup") return check.checks.forEach(visit)
      const representation = (check.annotations as { representation?: { id: string; payload: never } } | undefined)
        ?.representation
      if (representation?.id === "effect/schema/isBetween") return out.push(...lowerBetween(representation.payload))
      const c = representation && lowerCheck(representation.id, representation.payload)
      if (c) out.push(c)
      else
        this.note(
          "unenforced",
          `check ${representation?.id ?? (check.annotations as { expected?: string } | undefined)?.expected ?? "custom filter"}`,
        )
    }
    checks.forEach(visit)
    return out
  }
}

function lowerCheck(id: string, payload: Record<string, unknown> | null): Check | undefined {
  const num = (v: unknown) => (typeof v === "number" ? v : Number(v))
  switch (id) {
    case "effect/schema/isMinLength":
      return { k: "minLength", n: num(payload!.minLength) }
    case "effect/schema/isMaxLength":
      return { k: "maxLength", n: num(payload!.maxLength) }
    case "effect/schema/isPattern":
      return { k: "pattern", source: String(payload!.source), flags: String(payload!.flags ?? "") }
    case "effect/schema/isStartingWith":
      return { k: "startsWith", s: String(payload!.startsWith) }
    case "effect/schema/isEndingWith":
      return { k: "endsWith", s: String(payload!.endsWith) }
    case "effect/schema/isIncluding":
      return { k: "includes", s: String(payload!.includes) }
    case "effect/schema/isTrimmed":
      return { k: "trimmed" }
    case "effect/schema/isInt":
      return { k: "int" }
    case "effect/schema/isFinite":
      return { k: "finite" }
    case "effect/schema/isGreaterThan":
      return { k: "gt", n: num(payload!.exclusiveMinimum) }
    case "effect/schema/isGreaterThanOrEqualTo":
      return { k: "gte", n: num(payload!.minimum) }
    case "effect/schema/isLessThan":
      return { k: "lt", n: num(payload!.exclusiveMaximum) }
    case "effect/schema/isLessThanOrEqualTo":
      return { k: "lte", n: num(payload!.maximum) }
    case "effect/schema/isUnique":
      return { k: "unique" }
    // `isStringFinite` is implied by NumberFromString's own JS `Number()` parse plus the finite check.
    case "effect/schema/isStringFinite":
      return { k: "pattern", source: `^${SchemaAST.FINITE_PATTERN}$`, flags: "" }
    default:
      return undefined
  }
}

// `isBetween` carries both bounds in one check.
function lowerBetween(payload: Record<string, unknown>): Check[] {
  const num = (v: unknown) => (typeof v === "number" ? v : Number(v))
  return [
    payload.exclusiveMinimum ? { k: "gt", n: num(payload.minimum) } : { k: "gte", n: num(payload.minimum) },
    payload.exclusiveMaximum ? { k: "lt", n: num(payload.maximum) } : { k: "lte", n: num(payload.maximum) },
  ]
}

function checksOf(ty: Ty): Check[] {
  return "checks" in ty ? ty.checks : []
}

function fieldLevel(ty: Ty) {
  return ty.t === "default" || ty.t === "catchNone"
}

function docOf(ast: SchemaAST.AST): string | undefined {
  return SchemaAST.resolveDescription(ast)
}

// Custom (non-Effect) transformations, recognized by their decode function's source. A hook names the hand-written
// Rust reproduction in crates/opencode-protocol/src/wire/hook.rs. The fingerprint is the exact (whitespace-collapsed)
// source: if upstream changes the function, the generator stops, so the hook is re-checked against the new code.
const CUSTOM: ReadonlyArray<{ match: string; fingerprint: string; hook?: string; note: string }> = [
  {
    match: "parse(input)",
    fingerprint: '(input) => typeof input === "string" ? parse(input) : input',
    hook: "config_model_selection",
    note: "Config.ModelSelection: a `provider/model#variant` string decodes to the object form (Model.Ref.parse)",
  },
  {
    match: 'policy.action !== "integration.use"',
    fingerprint:
      '(policy) => { if (policy.action !== "integration.use") return policy; const separator = policy.resource.indexOf(":"), action = separator === -1 ? void 0 : legacy.get(policy.resource.slice(0, separator)); if (!action) return policy; return { ...policy, action, resource: policy.resource.slice(separator + 1) }; }',
    hook: "config_policy",
    note: "Config.Policy: legacy `integration.use` with a `mcp:`/`skill:`/`plugin:` resource prefix decodes to the specific action",
  },
  {
    match: "message(value.type, value)",
    fingerprint: "function messages(values) { return values.map((value) => isRecord(value) ? message(value.type, value) : value); }",
    note: "session.import: TS upgrades messages exported before v2.1 (legacy names) before validating them; Rust validates the current Session.Message shape only, so legacy messages are rejected",
  },
]

function customTransformation(dec: string, identifier: string | undefined) {
  const custom = CUSTOM.find((c) => dec.includes(c.match))
  if (!custom) return undefined
  if (dec !== custom.fingerprint)
    throw new Error(
      `custom transformation changed upstream (${identifier ?? custom.note}): review its Rust reproduction, then update the fingerprint in gen/src/lower.ts\n  was: ${custom.fingerprint}\n  now: ${dec}`,
    )
  return custom
}

function escapeRegex(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")
}

function templatePattern(ast: SchemaAST.TemplateLiteral): string {
  const part = (p: SchemaAST.AST): string => {
    switch (p._tag) {
      case "Literal":
        return escapeRegex(String(p.literal))
      case "String":
        if (p.checks) throw new Error("template literal string part with checks")
        return SchemaAST.STRING_PATTERN
      case "Number":
        if (p.checks) throw new Error("template literal number part with checks")
        return SchemaAST.FINITE_PATTERN
      case "Union":
        return `(?:${p.types.map(part).join("|")})`
      default:
        throw new Error(`template literal part ${p._tag}`)
    }
  }
  return `^${ast.parts.map(part).join("")}$`
}
