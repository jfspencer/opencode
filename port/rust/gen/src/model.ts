// Builds the whole generated model: roots -> IR definitions, deduplicated and named deterministically.
import { collect, type Endpoint, type Root } from "./roots.ts"
import { Lowering, type Def, type Note, type Ty } from "./lower.ts"

export interface RootTy {
  readonly root: Root
  /** The Rust type name the root is known by: its definition, or an alias of an inline type. */
  name: string
  ty: Ty
}

export interface Model {
  readonly defs: Def[]
  readonly roots: RootTy[]
  readonly endpoints: ReadonlyArray<Endpoint>
  readonly events: ReadonlyArray<{ type: string; version?: number; root: Root }>
  readonly sse: Root
  readonly config: Root
  readonly notes: Note[]
}

export function build(): Model {
  const collected = collect()
  const lowering = new Lowering()
  const roots: RootTy[] = collected.roots.map((root) => ({
    root,
    name: "",
    ty: lowering.lowerRoot(root.ast, root.hint, root.wire, root.origin),
  }))
  const defs = dedupe([...lowering.defs.values()], roots)
  const names = assignNames(defs)
  const rename = renamer(names)
  const taken = new Set(names.values())
  for (const root of roots) {
    root.ty = rename(root.ty) as Ty
    if (root.ty.t === "ref") {
      root.name = root.ty.name
      continue
    }
    let name = root.root.hint
    for (let i = 2; taken.has(name); i++) name = `${root.root.hint}${i}`
    taken.add(name)
    root.name = name
  }
  return {
    defs: defs.map((d) => ({ ...(rename(d) as Def), name: names.get(d.name)! })),
    roots,
    endpoints: collected.endpoints,
    events: collected.events,
    sse: collected.sse,
    config: collected.config,
    notes: lowering.notes,
  }
}

// One schema lowered in two wire contexts, or two copies of one schema, gives definitions with the same base name.
// Merge those that are the same type: partition refinement over (base name, shape), refined by the classes of what
// they reference, so mutually recursive definitions merge too. The first definition in declaration order stays.
function dedupe(defs: Def[], roots: RootTy[]): Def[] {
  const refsOf = new Map<string, string[]>()
  const shapeOf = new Map<string, string>()
  for (const def of defs) {
    const refs: string[] = []
    const shape = JSON.stringify(def, (key, value) => {
      if (key === "name" || key === "origin" || key === "doc" || key === "wire" || key === "prio") return undefined
      if (value && typeof value === "object" && value.t === "ref") {
        refs.push(value.name)
        return { t: "ref" }
      }
      return value
    })
    refsOf.set(def.name, refs)
    shapeOf.set(def.name, shape)
  }
  let cls = new Map(defs.map((d) => [d.name, shapeOf.get(d.name)!]))
  let count = new Set(cls.values()).size
  for (;;) {
    const ids = new Map<string, number>()
    const next = new Map<string, string>()
    for (const def of defs) {
      const key = `${cls.get(def.name)}|${refsOf.get(def.name)!.map((r) => cls.get(r)).join(",")}`
      if (!ids.has(key)) ids.set(key, ids.size)
      next.set(def.name, String(ids.get(key)))
    }
    const nextCount = new Set(next.values()).size
    cls = next
    if (nextCount === count) break
    count = nextCount
  }
  const representative = new Map<string, string>()
  const alias = new Map<string, string>()
  for (const def of defs) {
    const c = cls.get(def.name)!
    const rep = representative.get(c)
    if (rep === undefined) representative.set(c, def.name)
    else alias.set(def.name, rep)
  }
  const rename = renamer(alias)
  for (const root of roots) root.ty = rename(root.ty) as Ty
  return defs.filter((d) => !alias.has(d.name)).map((d) => ({ ...(rename(d) as Def), name: d.name }))
}

// Names the generated module imports or takes from the Rust prelude.
const RESERVED = new Set(["Json", "Issue", "Out", "Wire", "Option", "Vec", "String", "Box", "Result", "Some", "None", "Ok", "Err", "Default"])

// Identifier names first, then root hints, then derived names, each in declaration order. A name already taken by
// the same schema in another wire context gets the context as suffix (`Plain`, `Json`, `Query`); otherwise a number.
function assignNames(defs: Def[]): Map<string, string> {
  const seq = (d: Def) => Number(d.name.slice(1))
  const order = [...defs].sort((a, b) => a.prio - b.prio || seq(a) - seq(b))
  const owner = new Map<string, Def>()
  const names = new Map<string, string>()
  const suffix = { json: "Json", plain: "Plain", tree: "Query" } as const
  const taken = (name: string) => owner.has(name) || RESERVED.has(name)
  for (const def of order) {
    let name = def.base
    if (taken(name)) {
      const other = owner.get(name)
      const contextual = `${def.base}${suffix[def.wire]}`
      name = other && other.wire !== def.wire && other.prio === def.prio && !taken(contextual) ? contextual : def.base
      for (let i = 2; taken(name); i++) name = `${def.base}${i}`
    }
    owner.set(name, def)
    names.set(def.name, name)
  }
  return names
}

function renamer(names: ReadonlyMap<string, string>) {
  const rename = (ty: unknown): unknown => {
    if (Array.isArray(ty)) return ty.map(rename)
    if (!ty || typeof ty !== "object") return ty
    const o = ty as Record<string, unknown>
    if (o.t === "ref") return { t: "ref", name: names.get(o.name as string) ?? o.name }
    return Object.fromEntries(Object.entries(o).map(([k, v]) => [k, k === "default" ? v : rename(v)]))
  }
  return rename
}
