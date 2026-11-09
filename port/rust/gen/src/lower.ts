// Lowers Effect Schema ASTs to a small intermediate representation of their wire (encoded, JSON) side.
//
// Decoding a node with an encoding chain runs, in Effect v4: parse(last link's `to`) -> transformation.decode ->
// parse(the node with the remaining links) (see effect/src/internal/schema/interpreter.ts). The lowering follows
// the same structure: it lowers the wire-most node, then applies each recognized transformation. A transformation
// is either transparent on the wire (classes, the `optional` helper, bijective literal mappings), or one whose
// decode the Rust side reproduces (NumberFromString, Trim, DateTimeUtcFromMillis, ArrayEnsure, decoding defaults,
// catchDecoding, JSON strings), or a known custom one that is reported as not reproduced. Anything else stops the
// generator, so an upstream change that adds a transformation cannot slip through unnoticed.
//
// The IR also carries what Effect reports when decoding fails (port/rust/crates/opencode-protocol/src/wire/
// issue.rs): `te` is a node's type error (`InvalidType`: its `message` annotation, or `Expected ` and its expected
// text, the identifier when it has one), each check its filter's message, each required key its missing-key
// message, and each union a `Site`: its expected text and, per TS member, what Effect's candidate index needs
// (`getCandidateIndex`). The texts are computed from the AST the way effect/src/SchemaIssue.ts and
// effect/src/internal/schema/annotations.ts compute them.
import { Effect, Exit, Option, Schema, SchemaAST, SchemaIssue, SchemaTransformation } from "effect"
import { identity } from "effect/Function"
import type { Wire } from "./roots.ts"

export type Check = (
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
) & {
  /** What Effect reports when the check fails. */
  readonly msg: string
}

export type Lit = string | number | boolean

/** A TS union member as Effect's candidate index sees it (from its wire-most AST, `toCandidate`). */
export interface SiteMember {
  /** A `Never` member: never a candidate. */
  readonly never?: true
  readonly literal?: Lit
  /** Required literal fields (`collectSentinels`). */
  readonly sentinels: ReadonlyArray<readonly [string, Lit]>
  /** JS runtime types the member takes, as the bit set of `wire::union::types`. */
  readonly types: number
}

/** A TS union: its `AnyOf` message when no member is a candidate, and its members in declaration order. */
export interface Site {
  readonly te: string
  readonly members: SiteMember[]
}

/** Where a TS union member goes in the Rust value: an IR member index, `null` (the `None` of `X | null`), or nowhere. */
export type MemberMap = Array<number | "null" | "none">

export type Ty =
  | { readonly t: "ref"; readonly name: string }
  | { readonly t: "string"; readonly te: string; readonly checks: Check[] }
  /** A JSON number. With an `int` check it is a safe integer. */
  | { readonly t: "number"; readonly te: string; readonly checks: Check[] }
  /**
   * `Schema.Number` under the JSON codec: a finite number, or "NaN" / "Infinity" / "-Infinity". The messages are
   * those of its wire union: neither a number nor a string, a non-finite number, another string.
   */
  | { readonly t: "jsnumber"; readonly te: string; readonly nonFinite: string; readonly str: string; readonly checks: Check[] }
  | { readonly t: "boolean"; readonly te: string }
  | { readonly t: "null"; readonly te: string }
  | { readonly t: "literal"; readonly te: string; readonly value: Lit }
  | { readonly t: "never"; readonly te: string }
  /** Any JSON value; `nonNull` for an empty struct, which accepts and returns any non-null value unchanged. */
  | { readonly t: "json"; readonly nonNull: boolean; readonly te?: string }
  | { readonly t: "array"; readonly te: string; readonly item: Ty; readonly checks: Check[] }
  | {
      readonly t: "tuple"
      readonly te: string
      readonly elements: ReadonlyArray<{ ty: Ty; optional: boolean; missing: string }>
      readonly rest: Ty[]
      readonly checks: Check[]
    }
  | { readonly t: "record"; readonly te: string; readonly key: Ty; readonly value: Ty; readonly checks: Check[] }
  /** `X | null`, decoded through the TS union's site; `map` sends each TS member to `inner` (an index into it when it is a union of several) or to `None`. */
  | { readonly t: "nullable"; readonly inner: Ty; readonly site: Site; readonly map: MemberMap }
  /**
   * A union with one member JSON can hold (`string | undefined`), whose failures still read like the union's
   * (`Expected string | undefined` when the member is no candidate). The Rust value is the member's.
   */
  | { readonly t: "union1"; readonly inner: Ty; readonly site: Site; readonly map: MemberMap }
  /**
   * HttpApi's flattened wire shape of a query or header property: the value must first decode as `wire` (whose
   * failures are reported), then it is decoded as `inner` from the same input.
   */
  | { readonly t: "pre"; readonly wire: Ty; readonly inner: Ty }
  /** `Schema.NumberFromString`: a string on the wire, read with JS `Number(s)`, written back with `String(n)`. */
  | { readonly t: "numberString"; readonly te: string; readonly wire: Check[]; readonly num: Check[] }
  /** `Schema.Trim`: decoding trims (JS `String.prototype.trim`), then the checks apply. */
  | { readonly t: "trim"; readonly te: string; readonly wire: Check[]; readonly checks: Check[] }
  /** `DateTimeUtcFromMillis`: epoch milliseconds, truncated to an integer by `new Date(n)`, |n| <= 8.64e15. */
  | { readonly t: "millis"; readonly te: string; readonly checks: Check[] }
  /**
   * `Schema.ArrayEnsure`: a single item or an array of items on input, always an array on output. `many` is the
   * array member (an `array`, or an `arrayFromSingle` in a query).
   */
  | { readonly t: "arrayEnsure"; readonly item: Ty; readonly many: Ty; readonly site: Site; readonly map: MemberMap }
  /** HttpApi's `toCodecArrayFromSingle` in a query: a string is read as a one-item array; `te` for anything else. */
  | { readonly t: "arrayFromSingle"; readonly te: string; readonly inner: Ty }
  /** Internal: the single-item branch of ArrayEnsure before it is folded. */
  | { readonly t: "wrap1"; readonly item: Ty }
  /** `Schema.fromJsonString`: a string holding JSON text of `inner`. */
  | { readonly t: "jsonString"; readonly te: string; readonly inner: Ty }
  /** `Uint8Array` under the JSON codec: base64 text. */
  | { readonly t: "base64"; readonly te: string; readonly invalid: string }
  /** `Schema.DurationFromString`: Effect duration text, normalized to `<n> millis` / `<n> nanos` when written. */
  | { readonly t: "duration"; readonly te: string; readonly invalid: string }
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
  /** What a missing required key reports (`messageMissingKey`, or `Missing key`). */
  readonly missing: string
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
      readonly te: string
      readonly fields: Field[]
      /** Index signatures (records / struct-with-rest), in declaration order. */
      readonly rest: Array<{ key: Ty; value: Ty }>
      readonly checks: Check[]
    }
  /** Literals; any other input fails with `te` (no literal is a candidate, or the `Enum` type error). */
  | { readonly kind: "enum"; readonly doc?: string; readonly te: string; readonly values: Lit[] }
  | {
      readonly kind: "union"
      readonly doc?: string
      readonly members: Ty[]
      /** `oneOf`: the message when a second member matches; absent for `anyOf`. */
      readonly oneOf?: string
      readonly site: Site
      readonly map: MemberMap
    }
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
// effect/src/internal/schema/toCodec.ts, `arrayFromSingleTransformation`.
const ARRAY_FROM_SINGLE = '(input) => typeof input === "string" ? [input] : input'

export class Lowering {
  readonly defs = new Map<string, Def>()
  readonly notes: Note[] = []
  private readonly cache = new Map<SchemaAST.AST, Ty>()
  private readonly lowering = new Set<SchemaAST.AST>()
  private seq = 0
  private wire: Wire = "json"
  private origin = ""

  /** `decode`: what HttpApi decodes the root with, a union of this one schema (see `Root.decode`). */
  lowerRoot(ast: SchemaAST.AST, hint: string, wire: Wire, origin: string, decode?: SchemaAST.AST): Ty {
    this.wire = wire
    this.origin = origin
    const ty = this.lower(ast, hint, { base: hint, prio: 1 })
    if (decode !== undefined && (decode._tag !== "Union" || decode.types.length !== 1 || !this.transparent(siteOf(decode), 0, ty)))
      throw new Error(`payload union that reports differently from its schema at ${origin}`)
    return ty
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
    // (A union of one member keeps the member's type: its own decoding only adds the union's messages.)
    const named = (out.t === "custom" && out.target?.t === "ref") || out.t === "union1"
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
    const te = typeError(ast)
    switch (ast._tag) {
      case "String":
        return { t: "string", te, checks }
      case "Number":
        return { t: "number", te, checks }
      case "Boolean":
        return { t: "boolean", te }
      case "Null":
        return { t: "null", te }
      case "Undefined":
      case "Void":
      case "Never":
        // JSON has no undefined: such members never decode from JSON text.
        if (ast._tag === "Void") throw new Error(`Void (accepts any value) at ${this.origin}`)
        return { t: "never", te }
      case "Literal":
        if (typeof ast.literal === "bigint") throw new Error(`bigint literal at ${this.origin}`)
        return { t: "literal", te, value: ast.literal }
      case "Unknown":
      case "Any":
        return { t: "json", nonNull: false }
      case "ObjectKeyword":
        throw new Error(`ObjectKeyword at ${this.origin}`)
      case "Declaration":
        return this.lowerDeclaration(ast)
      case "TemplateLiteral": {
        // Its parser decodes through a plain `String` (so the type error never names the template) and reports one
        // message for any string that does not match the parts; both are read off the parser.
        const [typeMismatch, mismatch] = [0, "\u0000"].map((v) => probe(ast, v))
        if (typeMismatch === undefined || mismatch === undefined) throw new Error(`template literal messages at ${this.origin}`)
        return {
          t: "string",
          te: typeMismatch,
          checks: [{ k: "pattern", source: templatePattern(ast), flags: "", msg: mismatch }, ...checks],
        }
      }
      case "Arrays":
        if (ast.elements.length === 0 && ast.rest.length === 1)
          return { t: "array", te, item: this.lower(ast.rest[0], `${hint}Item`), checks }
        return {
          t: "tuple",
          te,
          elements: ast.elements.map((e, i) => ({
            ty: this.lower(e, `${hint}${i}`),
            optional: SchemaAST.isOptional(SchemaAST.getLastEncoding(e)),
            missing: missingKey(e),
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
          te,
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
    const te = typeError(ast)
    if (ast.propertySignatures.length === 0 && ast.indexSignatures.length === 0) {
      if (checks.length) this.note("unenforced", "checks on an empty struct")
      return { t: "json", nonNull: true, te }
    }
    if (ast.propertySignatures.length === 0 && own === undefined) {
      if (ast.indexSignatures.length !== 1) throw new Error(`record with several index signatures at ${this.origin}`)
      if (checks.length) throw new Error(`checks on a record at ${this.origin}`)
      const index = ast.indexSignatures[0]
      return {
        t: "record",
        te,
        key: this.lowerKey(index.parameter, `${hint}Key`),
        value: this.lower(index.type, `${hint}Value`),
        checks,
      }
    }
    const meta = this.reserve(own, hint)
    const ref: Ty = { t: "ref", name: meta.name }
    this.cache.set(this.cacheKey(ast), ref)
    const fields: Field[] = []
    const def = { ...meta, kind: "struct" as const, doc: docOf(ast), te, fields, rest: [] as Array<{ key: Ty; value: Ty }>, checks }
    this.defs.set(meta.name, def)
    for (const ps of ast.propertySignatures) {
      if (typeof ps.name !== "string") throw new Error(`non-string property key at ${this.origin}`)
      // Effect looks keys up with `key in input`: an absent key named like an Object.prototype member reads the
      // inherited function instead, which Rust (an own-key lookup) does not reproduce.
      if (ps.name !== "__proto__" && ps.name in Object.prototype)
        this.note("limit", `field \`${ps.name}\` is an Object.prototype name: TS reads the inherited value when it is absent`)
      // Absent on the wire: optional there, or optional itself over a wire that passes absence through (HttpApi's
      // `toCodecArrayFromSingle` wraps an optional array without marking its wire union optional).
      const wire = SchemaAST.getLastEncoding(ps.type)
      const optional = SchemaAST.isOptional(wire) || (SchemaAST.isOptional(ps.type) && this.arrayFromSingle(ps.type))
      const ty = this.lower(ps.type, `${def.base}${typeName(ps.name)}`)
      const doc = docOf(ps.type) ?? (ps.type.context?.annotations as { description?: string } | undefined)?.description
      const missing = missingKey(ps.type)
      if (ty.t === "default") fields.push({ key: ps.name, ty: ty.inner, optional, missing, default: ty.value, doc })
      else if (ty.t === "catchNone") {
        if (!optional) throw new Error(`catchDecoding on a required key at ${this.origin}`)
        fields.push({ key: ps.name, ty: ty.inner, optional, missing, catchNone: true, doc })
      } else fields.push({ key: ps.name, ty, optional, missing, doc })
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

  // Effect tries only the members its candidate index selects (by runtime type and sentinel fields), so each union
  // keeps a site: the members' candidate data in TS order, and where each member went in the IR.
  private lowerUnion(ast: SchemaAST.Union, hint: string, own: Own | undefined, checks: Check[]): Ty {
    if (checks.length) this.note("unenforced", "checks on a union")
    const reservedName = own?.base ?? hint
    const lowered = ast.types.map((m, i) => this.lower(m, `${reservedName}${i}`))
    const site = siteOf(ast)
    lowered.forEach((m, i) => {
      // A member that cannot come from JSON must not be a candidate either, or Effect would report its failure.
      if (m.t === "never" && !site.members[i].never && site.members[i].types !== 0)
        throw new Error(`union member ${m.te} is a candidate for JSON at ${this.origin}`)
    })
    const live = lowered.flatMap((ty, i) => (ty.t === "never" ? [] : [{ ty, i }]))
    const members = live.map((m) => m.ty)
    const map = (target: (ty: Ty, k: number) => number | "null"): MemberMap =>
      lowered.map((ty, i) => {
        const k = live.findIndex((m) => m.i === i)
        return k === -1 ? "none" : target(ty, k)
      })
    const oneOf = ast.options?.mode === "oneOf" ? oneOfMessage(ast) : undefined
    if (members.length === 0) return { t: "never", te: site.te }
    // ArrayEnsure: [one item, many items]
    const many = members[1]?.t === "arrayFromSingle" ? members[1].inner : members[1]
    if (
      members.length === 2 &&
      members[0].t === "wrap1" &&
      many?.t === "array" &&
      JSON.stringify(members[0].item) === JSON.stringify(many.item)
    )
      return { t: "arrayEnsure", item: many.item, many: members[1], site, map: map((_, k) => k) }
    if (!oneOf && members.length === 1)
      return this.transparent(site, live[0].i, members[0]) ? members[0] : { t: "union1", inner: members[0], site, map: map((_, k) => k) }
    if (!oneOf && members.every((m) => m.t === "literal")) {
      // Literal members are candidates only when equal to the input, so every failure is the union's own.
      if (!live.every((m) => site.members[m.i].literal !== undefined))
        throw new Error(`literal union with non-literal candidates at ${this.origin}`)
      return this.literals(
        members.map((m) => (m as { value: Lit }).value),
        hint,
        own,
        site.te,
      )
    }
    const nulls = live.filter((m) => m.ty.t === "null")
    const rest = live.filter((m) => m.ty.t !== "null")
    // `X | null`: only when no other member could also take null.
    if (!oneOf && nulls.length > 0 && rest.length > 0 && rest.every((m) => !this.acceptsNull(m.ty))) {
      const nullMap = map((ty, k) => (ty.t === "null" ? "null" : rest.findIndex((m) => m.i === live[k].i)))
      if (rest.length === 1) return { t: "nullable", inner: rest[0].ty, site, map: nullMap }
      // The non-null members as a union of their own (for direct use); the nullable decodes them through the whole
      // union's site.
      const restSite: Site = {
        te: `Expected ${[...new Set(rest.map((m) => memberExpected(ast.types[m.i])))].join(" | ")}`,
        members: rest.map((m) => site.members[m.i]),
      }
      const inner = this.lowerUnionDef(
        rest.map((m) => m.ty),
        hint,
        own,
        undefined,
        restSite,
        rest.map((_, k) => k),
      )
      return { t: "nullable", inner, site, map: nullMap }
    }
    return this.lowerUnionDef(members, hint, own, oneOf, site, map((_, k) => k))
  }

  private lowerUnionDef(members: Ty[], hint: string, own: Own | undefined, oneOf: string | undefined, site: Site, map: MemberMap): Ty {
    if (members.every((m) => m.t === "literal"))
      return this.literals(
        members.map((m) => (m as { value: Lit }).value),
        hint,
        own,
        site.te,
      )
    const meta = this.reserve(own, hint)
    this.defs.set(meta.name, { ...meta, kind: "union", members, ...(oneOf === undefined ? {} : { oneOf }), site, map })
    return { t: "ref", name: meta.name }
  }

  /**
   * A union with one live member behaves like the member alone when the member is a candidate for every input its
   * own decoder would take, and an input outside its types fails with the union's text, which is also the member's
   * type error.
   */
  transparent(site: Site, index: number, member: Ty): boolean {
    const m = site.members[index]
    if (m.sentinels.length) return false
    if (m.literal !== undefined) return site.te === this.typeErrorOf(member)
    return m.types === ALL_TYPES || site.te === this.typeErrorOf(member)
  }

  /** The message a decoder reports for an input of the wrong JSON type, when it has a single one. */
  private typeErrorOf(ty: Ty): string | undefined {
    switch (ty.t) {
      case "ref": {
        const def = this.defs.get(ty.name)
        if (def?.kind === "struct" || def?.kind === "enum") return def.te
        if (def?.kind === "newtype") return this.typeErrorOf(def.ty)
        return undefined
      }
      case "custom":
      case "pre":
        return this.typeErrorOf(ty.wire)
      case "nullable":
      case "union1":
      case "arrayEnsure":
      case "wrap1":
      case "default":
      case "catchNone":
        return undefined
      default:
        return "te" in ty ? ty.te : undefined
    }
  }

  private acceptsNull(ty: Ty): boolean {
    if (ty.t === "null" || (ty.t === "json" && !ty.nonNull)) return true
    if (ty.t === "nullable") return true
    if (ty.t === "union1") return this.acceptsNull(ty.inner)
    if (ty.t === "pre") return this.acceptsNull(ty.wire) && this.acceptsNull(ty.inner)
    if (ty.t === "ref") {
      const def = this.defs.get(ty.name)
      if (def?.kind === "newtype") return this.acceptsNull(def.ty)
      if (def?.kind === "union") return def.members.some((m) => this.acceptsNull(m))
      if (def?.kind === "enum") return false
    }
    if (ty.t === "custom") return this.acceptsNull(ty.wire)
    return false
  }

  private literals(values: Lit[], hint: string, own: Own | undefined, te: string): Ty {
    if (values.length === 1) return { t: "literal", te, value: values[0] }
    const meta = this.reserve(own, hint)
    this.defs.set(meta.name, { ...meta, kind: "enum", te, values })
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
      return this.refine({ t: "numberString", te: wire.te, wire: wire.checks, num: [] }, rest, hint)
    }
    // Schema.Number under toCodecJson: finite | "NaN" | "Infinity" | "-Infinity". Its wire union reports one of
    // three messages, read off the union itself.
    if (decTag === "Transform" && dec.includes("function Number()") && enc.includes("isFinite(n) ? n : globalThis.String(n)")) {
      const [te, nonFinite, str] = [true, Infinity, "x"].map((v) => probe(inner, v))
      if (te === undefined || nonFinite === undefined || str === undefined) throw new Error(`JSON number messages at ${this.origin}`)
      return { t: "jsnumber", te, nonFinite, str, checks: this.checks(restLocal?.checks) }
    }
    if (dec === "() => { return; }" && enc === "() => null") return { t: "null", te: typeError(inner) }
    if (dec.includes("self.trim()") && encTag === "Passthrough") {
      const wire = this.lower(inner, hint)
      if (wire.t !== "string") throw new Error(`Trim over ${wire.t} at ${this.origin}`)
      return this.refine({ t: "trim", te: wire.te, wire: wire.checks, checks: [] }, rest, hint)
    }
    if (dec.includes("DateTime.makeUnsafe(value)") && enc.includes("DateTime.toEpochMillis(value)")) {
      const wire = this.lower(inner, hint)
      if (wire.t !== "number") throw new Error(`DateTimeUtcFromMillis over ${wire.t} at ${this.origin}`)
      return { t: "millis", te: wire.te, checks: wire.checks }
    }
    if (dec === "(value) => [value]" && enc === "([value]) => value") return { t: "wrap1", item: this.lower(inner, hint) }
    // HttpApi's toCodecArrayFromSingle (queries): the wire is `array | string`, a string becomes a one-item array.
    if (dec === ARRAY_FROM_SINGLE && encTag === "Passthrough")
      return { t: "arrayFromSingle", te: typeError(inner), inner: this.lower(rest, hint, own) }
    if (dec.includes("JSON.parse(input")) return { t: "jsonString", te: typeError(inner), inner: this.lower(rest, hint, own) }
    if (dec.includes("Base64.decode(input)")) return { t: "base64", te: typeError(inner), invalid: probeOrThrow(ast, "!", this.origin) }
    if (dec.includes("Duration_.fromInput(s)")) return { t: "duration", te: typeError(inner), invalid: probeOrThrow(ast, "x", this.origin) }
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
      // only pre-validates its shape (so its failures are the ones reported). Otherwise `rest` is a type-side
      // stage: a class, the `optional` helper's `T | undefined`, or extra checks.
      if (rest._tag !== "Declaration" && SchemaAST.toType(rest) !== rest) {
        this.scanCustom(inner)
        const value = this.lower(rest, hint, own)
        const wire = this.lower(inner, hint)
        if (JSON.stringify(wire) === JSON.stringify(value)) return value
        if (fieldLevel(value) || fieldLevel(wire)) throw new Error(`pre-validated field-level transformation at ${this.origin}`)
        return { t: "pre", wire, inner: value }
      }
      return this.refine(this.lower(inner, hint, own), rest, hint)
    }
    throw new Error(`unsupported transformation at ${this.origin}: ${t._tag} decode=${decTag}:${dec} encode=${encTag}:${enc}`)
  }

  private arrayFromSingle(ast: SchemaAST.AST): boolean {
    const last = ast.encoding?.[ast.encoding.length - 1]
    return src((last?.transformation as { decode?: GetterLike } | undefined)?.decode) === ARRAY_FROM_SINGLE
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
      case "union1":
      case "pre":
        // The checks run on the decoded value, which is the member's.
        return { ...ty, inner: this.addChecks(ty.inner, checks) }
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
      const msg = checkMessage(check)
      if (representation?.id === "effect/schema/isBetween") return out.push(...lowerBetween(representation.payload, msg))
      const c = representation && lowerCheck(representation.id, representation.payload, msg)
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

function lowerCheck(id: string, payload: Record<string, unknown> | null, msg: string): Check | undefined {
  const num = (v: unknown) => (typeof v === "number" ? v : Number(v))
  switch (id) {
    case "effect/schema/isMinLength":
      return { k: "minLength", n: num(payload!.minLength), msg }
    case "effect/schema/isMaxLength":
      return { k: "maxLength", n: num(payload!.maxLength), msg }
    case "effect/schema/isPattern":
      return { k: "pattern", source: String(payload!.source), flags: String(payload!.flags ?? ""), msg }
    case "effect/schema/isStartingWith":
      return { k: "startsWith", s: String(payload!.startsWith), msg }
    case "effect/schema/isEndingWith":
      return { k: "endsWith", s: String(payload!.endsWith), msg }
    case "effect/schema/isIncluding":
      return { k: "includes", s: String(payload!.includes), msg }
    case "effect/schema/isTrimmed":
      return { k: "trimmed", msg }
    case "effect/schema/isInt":
      return { k: "int", msg }
    case "effect/schema/isFinite":
      return { k: "finite", msg }
    case "effect/schema/isGreaterThan":
      return { k: "gt", n: num(payload!.exclusiveMinimum), msg }
    case "effect/schema/isGreaterThanOrEqualTo":
      return { k: "gte", n: num(payload!.minimum), msg }
    case "effect/schema/isLessThan":
      return { k: "lt", n: num(payload!.exclusiveMaximum), msg }
    case "effect/schema/isLessThanOrEqualTo":
      return { k: "lte", n: num(payload!.maximum), msg }
    case "effect/schema/isUnique":
      return { k: "unique", msg }
    // `isStringFinite` is implied by NumberFromString's own JS `Number()` parse plus the finite check.
    case "effect/schema/isStringFinite":
      return { k: "pattern", source: `^${SchemaAST.FINITE_PATTERN}$`, flags: "", msg }
    default:
      return undefined
  }
}

// `isBetween` carries both bounds in one check, so both halves report its message.
function lowerBetween(payload: Record<string, unknown>, msg: string): Check[] {
  const num = (v: unknown) => (typeof v === "number" ? v : Number(v))
  return [
    payload.exclusiveMinimum ? { k: "gt", n: num(payload.minimum), msg } : { k: "gte", n: num(payload.minimum), msg },
    payload.exclusiveMaximum ? { k: "lt", n: num(payload.maximum), msg } : { k: "lte", n: num(payload.maximum), msg },
  ]
}

// ---- Effect's messages ---------------------------------------------------------------------------------------
// These follow effect/src/SchemaIssue.ts (defaultLeafHook, getFilterMessage, findMessage, formatIssue) and
// effect/src/internal/schema/annotations.ts (getExpected), under HttpApi's parse options: errors "first", no
// reportInput (so no "got <input>"), onExcessProperty "ignore".

type Annotations = Record<string, unknown> | undefined
const annotationsOf = (x: { readonly annotations?: unknown }) => x.annotations as Annotations
const stringAt = (a: Annotations, key: string) => (typeof a?.[key] === "string" ? (a[key] as string) : undefined)

const expectedCache = new WeakMap<SchemaAST.AST, string>()
/** `InternalAnnotations.getExpected`: the identifier (of the last check, else of the node), else the node's own text. */
function expectedOf(ast: SchemaAST.AST): string {
  const cached = expectedCache.get(ast)
  if (cached !== undefined) return cached
  const resolved = ast.checks ? annotationsOf(ast.checks[ast.checks.length - 1]) : annotationsOf(ast)
  const out =
    stringAt(resolved, "identifier") ??
    (ast as unknown as { getExpected(f: (ast: SchemaAST.AST) => string): string }).getExpected(expectedOf)
  expectedCache.set(ast, out)
  return out
}

/** `InvalidType` (and an empty `AnyOf`): the node's `message` annotation, or `Expected <expected>`. */
function typeError(ast: SchemaAST.AST): string {
  return stringAt(annotationsOf(ast), "message") ?? `Expected ${expectedOf(ast)}`
}

/** One member's part of a union's expected text (`Union.getExpected`). */
function memberExpected(type: SchemaAST.AST): string {
  const encoded = SchemaAST.toEncoded(type)
  const readonly = (mutable: boolean | undefined) => (mutable ? "" : "readonly ")
  const optional = (isOptional: boolean | undefined) => (isOptional ? "?" : "")
  if (encoded._tag === "Arrays") {
    const literals = encoded.elements.filter(SchemaAST.isLiteral)
    if (literals.length > 0)
      return `${readonly(encoded.isMutable)}[ ${literals.map((e) => expectedOf(e) + optional(e.context?.isOptional)).join(", ")}, ... ]`
  }
  if (encoded._tag === "Objects") {
    const literals = encoded.propertySignatures.filter((ps) => SchemaAST.isLiteral(ps.type))
    if (literals.length > 0)
      return `{ ${literals
        .map(
          (ps) =>
            `${readonly(ps.type.context?.isMutable)}${typeof ps.name === "string" ? JSON.stringify(ps.name) : String(ps.name)}${optional(ps.type.context?.isOptional)}: ${expectedOf(ps.type)}`,
        )
        .join(", ")}, ... }`
  }
  return expectedOf(encoded)
}

/** `MissingKey`: the key's `messageMissingKey` annotation, or `Missing key`. */
function missingKey(type: SchemaAST.AST): string {
  return stringAt(type.context?.annotations as Annotations, "messageMissingKey") ?? "Missing key"
}

/** A built-in filter fails with `InvalidValue(undefined)`: its `message` annotation, or `Expected <expected>`. */
function checkMessage(check: SchemaAST.Check<unknown>): string {
  const a = annotationsOf(check)
  return stringAt(a, "message") ?? `Expected ${stringAt(a, "expected") ?? "<filter>"}`
}

/** `OneOf` (a second member matched): the union's `message` annotation, or Effect's text. */
function oneOfMessage(ast: SchemaAST.Union): string {
  return stringAt(annotationsOf(ast), "message") ?? "Expected exactly one member to match"
}

/** The formatted message TS reports for `value`, or `undefined` when TS accepts it. Only for leaf codecs. */
function probe(ast: SchemaAST.AST, value: unknown): string | undefined {
  const exit = Schema.decodeUnknownExit(Schema.make(ast) as Schema.Codec<unknown, unknown>)(value)
  if (Exit.isSuccess(exit)) return undefined
  const error = (exit.cause as unknown as { reasons: Array<{ error?: { _tag?: string; message?: string } }> }).reasons[0]?.error
  if (error?._tag !== "SchemaError" || error.message === undefined || error.message.includes("\n"))
    throw new Error(`probe of ${ast._tag} did not fail with a leaf schema issue`)
  return error.message
}

function probeOrThrow(ast: SchemaAST.AST, value: unknown, origin: string): string {
  const out = probe(ast, value)
  if (out === undefined) throw new Error(`probe ${JSON.stringify(value)} of ${ast._tag} decodes at ${origin}`)
  return out
}

// ---- union candidates ----------------------------------------------------------------------------------------
// Effect's candidate index (SchemaAST.ts: toCandidate, getCandidateTypes, collectSentinels, getCandidateIndex).
// The selection itself runs in Rust (wire/union.rs); here each member is described.

const TYPE_BITS: Record<string, number> = { null: 1, boolean: 2, number: 4, string: 8, array: 16, object: 32 }
const ALL_TYPES = 63

const candidateCache = new WeakMap<SchemaAST.AST, SchemaAST.AST>()
function toCandidate(ast: SchemaAST.AST): SchemaAST.AST {
  const cached = candidateCache.get(ast)
  if (cached) return cached
  let a = ast
  let out: SchemaAST.AST | undefined
  while (out === undefined) {
    if (a._tag === "Suspend") out = SchemaAST.unknown
    else if (!a.encoding) {
      const recur = (a as unknown as { recur?: (f: (ast: SchemaAST.AST) => SchemaAST.AST, g: unknown) => SchemaAST.AST }).recur
      out = recur ? recur.call(a, toCandidate, identity) : a
    } else if (a.encoding.some((link) => link.transformation._tag === "Middleware" && link.transformation.decode !== identity))
      out = SchemaAST.unknown
    else a = a.encoding[a.encoding.length - 1].to
  }
  candidateCache.set(ast, out)
  candidateCache.set(out, out)
  return out
}

function candidateTypes(ast: SchemaAST.AST): number {
  switch (ast._tag) {
    case "Null":
      return TYPE_BITS.null
    case "Undefined":
    case "Symbol":
    case "UniqueSymbol":
    case "BigInt":
      return 0
    case "String":
    case "TemplateLiteral":
      return TYPE_BITS.string
    case "Number":
      return TYPE_BITS.number
    case "Boolean":
      return TYPE_BITS.boolean
    case "Arrays":
      return TYPE_BITS.array
    case "ObjectKeyword":
      return TYPE_BITS.object | TYPE_BITS.array
    case "Objects":
      return ast.propertySignatures.length || ast.indexSignatures.length ? TYPE_BITS.object : ALL_TYPES & ~TYPE_BITS.null
    case "Enum":
      return ast.enums.reduce((bits, [, v]) => bits | (TYPE_BITS[typeof v] ?? 0), 0)
    case "Literal":
      return TYPE_BITS[typeof ast.literal] ?? 0
    case "Union":
      return ast.types.reduce((bits, t) => bits | candidateTypes(t), 0)
    default:
      return ALL_TYPES
  }
}

const isLit = (v: unknown): v is Lit => typeof v === "string" || typeof v === "number" || typeof v === "boolean"

function siteOf(ast: SchemaAST.Union): Site {
  const collect = (SchemaAST as unknown as { collectSentinels(ast: SchemaAST.AST): ReadonlyArray<{ key: PropertyKey; literal: unknown }> })
    .collectSentinels
  return {
    te: typeError(ast),
    members: ast.types.map((type): SiteMember => {
      const encoded = toCandidate(type)
      if (encoded._tag === "Never") return { never: true, sentinels: [], types: 0 }
      const sentinels = collect(encoded).map((s): readonly [string, Lit] => {
        if (typeof s.key !== "string" || !isLit(s.literal)) throw new Error(`unsupported sentinel ${String(s.key)}`)
        return [s.key, s.literal]
      })
      const literal = encoded._tag === "Literal" ? encoded.literal : undefined
      if (literal !== undefined && !isLit(literal)) throw new Error("bigint literal in a union")
      return { ...(literal === undefined ? {} : { literal }), sentinels, types: candidateTypes(encoded) }
    }),
  }
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
