// The TS side of the opencode-yoga differential check. Generates programs (random trees with random styles,
// measure and dirtied functions, mutations and relayouts, plus the hand-picked ones in cases.ts) and runs each op as
// it is generated through OpenTUI 0.8.1's own Yoga path in the reference's node_modules: the public `Yoga`
// Node/Config classes (yoga.ts) and, for what only the renderer's scene calls, the native exports of the same
// libopentui.so (move, dimension and position setters, the context config's absolute-edge rounding, native measure
// functions, measure invalidation). Writes the programs, one op per line and `end` after each, and what TS answered,
// in the same framing; examples/yoga-driver.rs replays the programs through the Rust crate.
//
// The process's stdout carries only what Yoga's default logger prints (run.sh compares it with the driver's).
//
// usage: bun harness/oracle.ts --programs <file> --results <file> [--count N] [--seed S]
import { dlopen, JSCallback } from "bun:ffi"
import { writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { handPicked } from "./cases"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const here = dirname(fileURLToPath(import.meta.url))
const reference = join(here, "../../../../.reference")
const tui = join(reference, "packages/tui/src")
const Y = (await import(Bun.resolveSync("@opentui/core/yoga", tui))).default
const coreDir = dirname(Bun.resolveSync("@opentui/core/package.json", tui))
const libPath: string = (await import(Bun.resolveSync("@opentui/core-linux-x64", coreDir))).default
const lib = dlopen(libPath, {
  otYogaConfigSetRoundsPositionsFromAbsoluteEdges: { args: ["ptr", "u32"], returns: "void" },
  otYogaNodeSetMeasureFunc: { args: ["ptr", "ptr", "u32"], returns: "u32" },
  otYogaNodeInvalidateMeasure: { args: ["ptr"], returns: "void" },
  otYogaNodeForgetMeasurements: { args: ["ptr"], returns: "void" },
  YGNodeSetNodeType: { args: ["ptr", "i32"], returns: "void" },
  yogaNodeMoveChildChecked: { args: ["ptr", "ptr", "u32"], returns: "u32" },
  yogaNodeCalculateLayoutChecked: { args: ["ptr", "f32", "f32", "u32"], returns: "u32" },
  yogaNodeStyleSetEnumChecked: { args: ["ptr", "u32", "u32"], returns: "u32" },
  yogaNodeStyleSetFloatChecked: { args: ["ptr", "u32", "f32"], returns: "u32" },
  yogaNodeStyleSetBorderChecked: { args: ["ptr", "u32", "f32"], returns: "u32" },
  yogaNodeStyleSetValueChecked: { args: ["ptr", "u32", "u32", "u32", "f32"], returns: "u32" },
  yogaNodeStyleSetDimensionChecked: { args: ["ptr", "u32", "u32", "f32", "u32"], returns: "u32" },
  yogaNodeStyleSetPositionsChecked: { args: ["ptr", "u32", "ptr", "ptr"], returns: "u32" },
  yogaNodeStyleGetEnumChecked: { args: ["ptr", "u32", "ptr"], returns: "u32" },
  yogaNodeStyleGetFloatChecked: { args: ["ptr", "u32", "ptr"], returns: "u32" },
  yogaNodeStyleGetBorderChecked: { args: ["ptr", "u32", "ptr"], returns: "u32" },
  yogaNodeStyleGetValueChecked: { args: ["ptr", "u32", "u32", "ptr"], returns: "u32" },
  yogaNodeLayoutGetEdgeChecked: { args: ["ptr", "u32", "u32", "ptr"], returns: "u32" },
  yogaNodeSetHasNewLayoutChecked: { args: ["ptr", "u32"], returns: "u32" },
  yogaNodeSetIsReferenceBaselineChecked: { args: ["ptr", "u32"], returns: "u32" },
  yogaNodeSetAlwaysFormsContainingBlockChecked: { args: ["ptr", "u32"], returns: "u32" },
})
const raw = lib.symbols

// ---- formatting shared with the driver
const f32 = new DataView(new ArrayBuffer(8))
export const hx = (x: number) => {
  if (Number.isNaN(x)) return "nan"
  f32.setFloat32(0, x, true)
  return f32.getUint32(0, true).toString(16).padStart(8, "0")
}
export const num = (x: number) => (Object.is(x, -0) ? "-0" : String(x))
const esc = (text: string) => {
  let out = ""
  for (const ch of text) {
    const code = ch.codePointAt(0)!
    out += code >= 0x20 && code <= 0x7e && ch !== "\\" ? ch : `\\u{${code.toString(16)}}`
  }
  return out
}
const errText = (error: unknown) => `err ${esc(error instanceof Error ? error.message : String(error))}`

// ---- measure functions, the same arithmetic in the driver
const jsMin = Math.min
const jsMax = Math.max
export function measureSpec(kind: string, a: number, b: number) {
  return (w: number, wm: number, h: number, hm: number): [number, number] => {
    switch (kind) {
      case "fixed":
        return [a, b]
      case "text": {
        const limit = wm === 0 ? a : jsMax(1, Math.floor(w))
        const width = jsMin(a, limit)
        let height = width > 0 ? Math.ceil(a / width) : 0
        if (hm === 2) height = jsMin(height, h)
        return [width, height]
      }
      case "frac":
        return [wm === 0 ? a : w * 0.5 + a, hm === 0 ? b : h * 0.25 + b]
      case "echo":
        return [wm === 0 ? a : w, hm === 0 ? b : h]
    }
    throw new Error(`measure kind ${kind}`)
  }
}

// ---- one program's state
type Config = any
type Node = any
export class State {
  configs: Config[] = []
  nodes: Node[] = []
  ids = new Map<number, number>()
  out: string[] = []
  ops: string[] = []
  native = new Map<number, (w: number, wm: number, h: number, hm: number) => [number, number]>()

  id(node: Node | null) {
    return node ? String(this.ids.get(Number(node.ptr))) : "null"
  }

  cleanup() {
    for (const node of this.nodes) if (node && !node.isFreed()) node.free()
    for (const config of this.configs) config?.free()
  }
}

let current: State | undefined
const packed = new DataView(new ArrayBuffer(8))
// Yoga's YGSize {float width, height} comes back in xmm0: a double whose low half is the width's bits.
const nativeMeasure = new JSCallback(
  (node: number, w: number, wm: number, h: number, hm: number) => {
    const state = current!
    const id = state.ids.get(Number(node))!
    state.out.push(`m ${id} ${hx(w)} ${wm} ${hx(h)} ${hm}`)
    const [width, height] = state.native.get(Number(node))!(w, wm, h, hm)
    packed.setFloat32(0, width, true)
    packed.setFloat32(4, height, true)
    return packed.getFloat64(0, true)
  },
  { args: ["ptr", "f32", "i32", "f32", "i32"], returns: "f64" },
)

const u32 = new Uint32Array(1)
const f32out = new Float32Array(1)
const u64 = new BigUint64Array(1)
const units = new Uint32Array(4)
const values = new Float32Array(4)

const unpack = (packedValue: bigint) => {
  const unit = Number(packedValue & 0xffffffffn)
  f32.setUint32(0, Number((packedValue >> 32n) & 0xffffffffn), true)
  return `${unit}:${hx(f32.getFloat32(0, true))}`
}
const value = (v: { unit: number; value: number }) => `${v.unit}:${hx(v.value)}`

/** `u`, `a`, `n:<number>`, `s:<encodeURIComponent string>`, `v:<unit>:<number>`. */
const valueInput = (token: string): unknown => {
  if (token === "u") return undefined
  if (token === "a") return "auto"
  if (token.startsWith("n:")) return Number(token.slice(2))
  if (token.startsWith("s:")) return decodeURIComponent(token.slice(2))
  if (token.startsWith("v:")) {
    const [, unit, v] = token.split(":")
    return { unit: Number(unit), value: Number(v) }
  }
  throw new Error(`value input ${token}`)
}
const optNumber = (token: string) => (token === "u" ? undefined : Number(token))
const layoutInput = (token: string) => (token === "u" ? undefined : token === "a" ? "auto" : Number(token))

export const enumSetters = [
  "setDirection",
  "setFlexDirection",
  "setJustifyContent",
  "setAlignContent",
  "setAlignItems",
  "setAlignSelf",
  "setPositionType",
  "setFlexWrap",
  "setOverflow",
  "setDisplay",
  "setBoxSizing",
]
export const floatSetters = ["setFlex", "setFlexGrow", "setFlexShrink", "setAspectRatio"]
export const valueSetters = ["setFlexBasis", "setWidth", "setHeight", "setMinWidth", "setMinHeight", "setMaxWidth", "setMaxHeight"]
export const percentSetters = valueSetters.map((name) => `${name}Percent`)
export const autoSetters = ["setFlexBasisAuto", "setWidthAuto", "setHeightAuto"]
export const edgeSetters = ["setMargin", "setPadding", "setPosition"]
export const edgePercentSetters = ["setMarginPercent", "setPaddingPercent", "setPositionPercent"]
export const edgeAutoSetters = ["setMarginAuto", "setPositionAuto"]

function tsCall(node: Node, method: string, args: string[]) {
  if (enumSetters.includes(method)) return node[method](Number(args[0]))
  if (floatSetters.includes(method) || percentSetters.includes(method)) return node[method](optNumber(args[0]))
  if (valueSetters.includes(method)) return node[method](valueInput(args[0]))
  if (autoSetters.includes(method)) return node[method]()
  if (edgeSetters.includes(method) || method === "setGap") return node[method](Number(args[0]), valueInput(args[1]))
  if (edgePercentSetters.includes(method) || method === "setGapPercent" || method === "setBorder")
    return node[method](Number(args[0]), optNumber(args[1]))
  if (edgeAutoSetters.includes(method)) return node[method](Number(args[0]))
  if (method === "setIsReferenceBaseline" || method === "setAlwaysFormsContainingBlock") return node[method](args[0] === "1")
  throw new Error(`method ${method}`)
}

function getters(node: Node): string {
  const parts: string[] = []
  const add = (f: () => string) => {
    try {
      parts.push(f())
    } catch (error) {
      parts.push(errText(error))
    }
  }
  for (const name of [
    "getDirection",
    "getFlexDirection",
    "getJustifyContent",
    "getAlignContent",
    "getAlignItems",
    "getAlignSelf",
    "getPositionType",
    "getFlexWrap",
    "getOverflow",
    "getDisplay",
    "getBoxSizing",
  ])
    add(() => String(node[name]()))
  for (const name of ["getFlex", "getFlexGrow", "getFlexShrink", "getAspectRatio"]) add(() => hx(node[name]()))
  for (const name of ["getFlexBasis", "getWidth", "getHeight", "getMinWidth", "getMinHeight", "getMaxWidth", "getMaxHeight"])
    add(() => value(node[name]()))
  for (const name of ["getMargin", "getPadding", "getPosition"])
    for (let edge = 0; edge <= 8; edge++) add(() => value(node[name](edge)))
  for (let gutter = 0; gutter <= 2; gutter++) add(() => value(node.getGap(gutter)))
  for (let edge = 0; edge <= 8; edge++) add(() => hx(node.getBorder(edge)))
  add(() => String(Number(node.isReferenceBaseline())))
  add(() => String(Number(node.getAlwaysFormsContainingBlock())))
  add(() => String(Number(node.hasMeasureFunc())))
  add(() => String(node.getChildCount()))
  return parts.join(" ")
}

function dump(state: State, node: Node, out: string[]) {
  const parts = [`n ${state.id(node)}`]
  try {
    const l = node.getComputedLayout()
    parts.push([l.left, l.top, l.right, l.bottom, l.width, l.height].map(hx).join(" "))
    for (const name of ["getComputedMargin", "getComputedPadding", "getComputedBorder"]) {
      const row: string[] = []
      for (let edge = 0; edge <= 5; edge++) row.push(hx(node[name](edge)))
      parts.push(row.join(" "))
    }
    parts.push(`${Number(node.hasNewLayout())} ${Number(node.isDirty())}`)
  } catch (error) {
    parts.push(errText(error))
  }
  out.push(parts.join(" | "))
  for (let index = 0; index < node.getChildCount(); index++) dump(state, node.getChild(index), out)
}

/** Runs one op, appending what it answered (after any measure and dirtied lines it caused). */
export function exec(state: State, op: string) {
  current = state
  state.ops.push(op)
  const [name, ...args] = op.split(" ")
  const n = (i: number) => state.nodes[Number(args[i])]
  const c = (i: number) => state.configs[Number(args[i])]
  const out = state.out
  const tsOp = (f: () => unknown) => {
    try {
      f()
      out.push("ok")
    } catch (error) {
      out.push(errText(error))
    }
  }
  const tsVal = (f: () => string) => {
    try {
      out.push(f())
    } catch (error) {
      out.push(errText(error))
    }
  }
  const rawOp = (status: number) => out.push(`s=${status}`)
  switch (name) {
    case "cfg": {
      state.configs[Number(args[0])] = Y.Config.create()
      out.push("ok")
      return
    }
    case "cfg.web":
      return tsOp(() => c(0).setUseWebDefaults(args[1] === "1"))
    case "cfg.scale":
      return tsOp(() => c(0).setPointScaleFactor(Number(args[1])))
    case "cfg.errata":
      return tsOp(() => c(0).setErrata(Number(args[1])))
    case "cfg.exp":
      return tsOp(() => c(0).setExperimentalFeatureEnabled(0, args[1] === "1"))
    case "cfg.rounds":
      raw.otYogaConfigSetRoundsPositionsFromAbsoluteEdges(c(0).ptr, Number(args[1]))
      out.push("ok")
      return
    case "cfg.get": {
      const config = c(0)
      out.push(
        `${Number(config.useWebDefaults())} ${hx(config.getPointScaleFactor())} ${config.getErrata()} ${Number(config.isExperimentalFeatureEnabled(0))}`,
      )
      return
    }
    case "new": {
      const which = Number(args[1])
      const node = which === -1 ? Y.Node.create() : which === -2 ? Y.Node.createForOpenTUI() : Y.Node.create(c(1))
      state.nodes[Number(args[0])] = node
      state.ids.set(Number(node.ptr), Number(args[0]))
      out.push("ok")
      return
    }
    case "ins":
      return tsOp(() => n(0).insertChild(n(1), Number(args[2])))
    case "rm":
      return tsOp(() => n(0).removeChild(n(1)))
    case "rmall":
      return tsOp(() => n(0).removeAllChildren())
    case "free":
      return tsOp(() => n(0).free())
    case "freerec":
      return tsOp(() => n(0).freeRecursive())
    case "reset":
      return tsOp(() => n(0).reset())
    case "copy":
      return tsOp(() => n(0).copyStyle(n(1)))
    case "mv":
      return rawOp(raw.yogaNodeMoveChildChecked(n(0).ptr, n(1).ptr, Number(args[2])))
    case "child":
      return tsVal(() => `child ${state.id(n(0).getChild(Number(args[1])))}`)
    case "count":
      out.push(String(n(0).getChildCount()))
      return
    case "parent":
      out.push(state.id(n(0).getParent()))
      return
    case "dirty":
      return tsVal(() => `dirty ${Number(n(0).isDirty())}`)
    case "newl":
      return tsVal(() => `newl ${Number(n(0).hasNewLayout())}`)
    case "seen":
      return tsOp(() => n(0).markLayoutSeen())
    case "mark":
      return tsOp(() => n(0).markDirty())
    case "hasm":
      out.push(String(Number(n(0).hasMeasureFunc())))
      return
    case "lay":
      return tsOp(() => n(0).calculateLayout(layoutInput(args[1]), layoutInput(args[2]), Number(args[3])))
    case "layr":
      return rawOp(raw.yogaNodeCalculateLayoutChecked(n(0).ptr, Number(args[1]), Number(args[2]), Number(args[3])))
    case "dump":
      return dump(state, n(0), out)
    case "get":
      out.push(getters(n(0)))
      return
    case "se":
      return rawOp(raw.yogaNodeStyleSetEnumChecked(n(0).ptr, Number(args[1]), Number(args[2])))
    case "sf":
      return rawOp(raw.yogaNodeStyleSetFloatChecked(n(0).ptr, Number(args[1]), Number(args[2])))
    case "sb":
      return rawOp(raw.yogaNodeStyleSetBorderChecked(n(0).ptr, Number(args[1]), Number(args[2])))
    case "sv":
      return rawOp(
        raw.yogaNodeStyleSetValueChecked(n(0).ptr, Number(args[1]), Number(args[2]), Number(args[3]), Number(args[4])),
      )
    case "sd":
      return rawOp(
        raw.yogaNodeStyleSetDimensionChecked(n(0).ptr, Number(args[1]), Number(args[2]), Number(args[3]), Number(args[4])),
      )
    case "sp": {
      for (let edge = 0; edge < 4; edge++) {
        units[edge] = Number(args[2 + edge * 2])
        values[edge] = Number(args[3 + edge * 2])
      }
      return rawOp(raw.yogaNodeStyleSetPositionsChecked(n(0).ptr, Number(args[1]), units, values))
    }
    case "flag": {
      const setter = [
        raw.yogaNodeSetHasNewLayoutChecked,
        raw.yogaNodeSetIsReferenceBaselineChecked,
        raw.yogaNodeSetAlwaysFormsContainingBlockChecked,
      ][Number(args[1])]
      return rawOp(setter(n(0).ptr, Number(args[2])))
    }
    case "ge": {
      const status = raw.yogaNodeStyleGetEnumChecked(n(0).ptr, Number(args[1]), u32)
      out.push(status ? `s=${status}` : `s=0 ${u32[0]}`)
      return
    }
    case "gf": {
      const status = raw.yogaNodeStyleGetFloatChecked(n(0).ptr, Number(args[1]), f32out)
      out.push(status ? `s=${status}` : `s=0 ${hx(f32out[0])}`)
      return
    }
    case "gb": {
      const status = raw.yogaNodeStyleGetBorderChecked(n(0).ptr, Number(args[1]), f32out)
      out.push(status ? `s=${status}` : `s=0 ${hx(f32out[0])}`)
      return
    }
    case "gv": {
      const status = raw.yogaNodeStyleGetValueChecked(n(0).ptr, Number(args[1]), Number(args[2]), u64)
      out.push(status ? `s=${status}` : `s=0 ${unpack(u64[0])}`)
      return
    }
    case "gl": {
      const status = raw.yogaNodeLayoutGetEdgeChecked(n(0).ptr, Number(args[1]), Number(args[2]), f32out)
      out.push(status ? `s=${status}` : `s=0 ${hx(f32out[0])}`)
      return
    }
    case "ts":
      return tsOp(() => tsCall(n(0), args[1], args.slice(2)))
    case "meas": {
      const id = Number(args[0])
      const f = measureSpec(args[1], Number(args[2]), Number(args[3]))
      return tsOp(() =>
        n(0).setMeasureFunc((w: number, wm: number, h: number, hm: number) => {
          out.push(`m ${id} ${hx(w)} ${wm} ${hx(h)} ${hm}`)
          const [width, height] = f(w, wm, h, hm)
          return { width, height }
        }),
      )
    }
    case "unmeas":
      return tsOp(() => n(0).unsetMeasureFunc())
    case "nmeas": {
      // yoga.zig yogaNodeSetNativeMeasureFunc: the bridge call, then a default node type.
      const node = n(0)
      const enabled = args[2] !== undefined
      const status = raw.otYogaNodeSetMeasureFunc(node.ptr, enabled ? nativeMeasure.ptr : null, Number(args[1]))
      if (status === 0) {
        raw.YGNodeSetNodeType(node.ptr, 0)
        if (enabled) state.native.set(Number(node.ptr), measureSpec(args[2], Number(args[3]), Number(args[4])))
      }
      return rawOp(status)
    }
    case "dirt": {
      const id = Number(args[0])
      return tsOp(() => n(0).setDirtiedFunc(() => out.push(`d ${id}`)))
    }
    case "undirt":
      return tsOp(() => n(0).unsetDirtiedFunc())
    case "inval":
      raw.otYogaNodeInvalidateMeasure(n(0).ptr)
      out.push("ok")
      return
    case "forget":
      raw.otYogaNodeForgetMeasurements(n(0).ptr)
      out.push("ok")
      return
  }
  throw new Error(`op ${op}`)
}

// ---- generation
class Rng {
  private s: number
  constructor(seed: number) {
    this.s = seed >>> 0 || 1
  }
  next() {
    // mulberry32
    let t = (this.s = (this.s + 0x6d2b79f5) >>> 0)
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(lo: number, hi: number) {
    return lo + Math.floor(this.next() * (hi - lo + 1))
  }
  chance(p: number) {
    return this.next() < p
  }
  pick<T>(items: readonly T[]): T {
    return items[Math.floor(this.next() * items.length)]
  }
}

const POINT_SCALES = [1, 1, 1, 1, 0, 2, 0.5, 1.5, 3, 1 / 3, 0.25]
const ERRATA = [0, 0, 0, 1, 2, 4, 3, 5, 6, 7, 2147483647, 2147483646, 8, 100]
const ENUM_MAX = [2, 3, 5, 8, 8, 8, 2, 2, 2, 2, 1]
const PERCENT_STRINGS = ["50%", "100%", "33.3%", " 25%", "12px%", ".5%", "1e1%", "-10%", "%", "abc%", "auto", "10", "Infinity%", " 7%", "1e40%", "0x10%", "5.%"]

function size(rng: Rng): number {
  switch (rng.int(0, 5)) {
    case 0:
      return rng.int(0, 10)
    case 1:
      return rng.int(0, 120)
    case 2:
      return Math.round(rng.next() * 1000) / 10
    case 3:
      return rng.int(0, 40) + rng.pick([0.25, 0.5, 0.75, 1 / 3, 0.1, 0.9, 0.49, 0.51])
    case 4:
      return -rng.int(0, 10) - rng.pick([0, 0.5, 0.3])
    default:
      return rng.next() * 60
  }
}

function percent(rng: Rng): number {
  return rng.pick([0, 10, 25, 33.3, 50, 66.6, 75, 100, 12.5, 120]) + (rng.chance(0.2) ? rng.next() : 0)
}

function valueToken(rng: Rng, allowAuto: boolean): string {
  switch (rng.int(0, 9)) {
    case 0:
      return "u"
    case 1:
      return allowAuto ? "a" : "u"
    case 2:
    case 3:
      return `s:${encodeURIComponent(rng.chance(0.7) ? `${num(percent(rng))}%` : rng.pick(PERCENT_STRINGS))}`
    case 4:
      return `v:${rng.int(0, allowAuto ? 3 : 2)}:${num(size(rng))}`
    default:
      return `n:${num(size(rng))}`
  }
}

const freed = (state: State, id: number) => state.nodes[id].isFreed()

/** A random style change: the yoga.ts setters or the native ABI, a few deliberately invalid. */
function styleOp(rng: Rng, state: State, id: number): string {
  const live = !freed(state, id)
  if (live && rng.chance(0.4)) {
    const bad = rng.chance(0.06)
    switch (rng.int(0, 7)) {
      case 0: {
        const kind = bad ? rng.int(0, 12) : rng.int(0, 10)
        const max = ENUM_MAX[kind] ?? 3
        return `se ${id} ${kind} ${bad ? rng.int(0, max + 2) : rng.int(0, max)}`
      }
      case 1:
        return `sf ${id} ${bad ? rng.int(0, 5) : rng.int(0, 3)} ${bad && rng.chance(0.5) ? "Infinity" : num(rng.chance(0.2) ? NaN : rng.int(0, 3) + (rng.chance(0.3) ? rng.next() : 0))}`
      case 2:
        return `sb ${id} ${bad ? rng.int(0, 10) : rng.int(0, 8)} ${num(rng.chance(0.1) ? NaN : rng.int(0, 2))}`
      case 3: {
        const kind = bad ? rng.int(0, 12) : rng.int(0, 10)
        const edge = kind === 10 ? rng.int(0, bad ? 4 : 2) : kind >= 7 ? rng.int(0, bad ? 10 : 8) : rng.int(0, 2)
        const unit = bad ? rng.int(0, 5) : rng.int(0, 3)
        return `sv ${id} ${kind} ${edge} ${unit} ${num(unit === 2 ? percent(rng) : size(rng))}`
      }
      case 4: {
        const unit = bad ? rng.int(0, 5) : rng.int(0, 3)
        return `sd ${id} ${bad ? rng.int(0, 3) : rng.int(0, 1)} ${unit} ${num(unit === 2 ? percent(rng) : size(rng))} ${bad ? rng.int(0, 2) : rng.int(0, 1)}`
      }
      case 5: {
        const mask = bad ? rng.int(0, 31) : rng.int(0, 15)
        const parts = [`sp ${id} ${mask}`]
        for (let edge = 0; edge < 4; edge++) {
          const unit = bad ? rng.int(0, 5) : rng.int(0, 3)
          parts.push(`${unit} ${num(unit === 2 ? percent(rng) : size(rng))}`)
        }
        return parts.join(" ")
      }
      case 6:
        return `flag ${id} ${rng.int(0, 2)} ${rng.int(0, 1)}`
      default:
        return `sv ${id} ${rng.pick([7, 8, 9])} ${rng.int(0, 8)} ${rng.pick([1, 1, 2, 3])} ${num(size(rng))}`
    }
  }
  switch (rng.int(0, 11)) {
    case 0:
    case 1: {
      const index = rng.int(0, enumSetters.length - 1)
      const weights: Record<string, number[]> = {
        setPositionType: [0, 1, 1, 1, 2],
        setDisplay: [0, 0, 0, 0, 0, 1, 2],
      }
      const method = enumSetters[index]
      const v = weights[method] ? rng.pick(weights[method]) : rng.int(0, ENUM_MAX[index])
      return `ts ${id} ${method} ${v}`
    }
    case 2:
      return `ts ${id} ${rng.pick(floatSetters)} ${rng.chance(0.15) ? "u" : num(rng.int(0, 3) + (rng.chance(0.3) ? Math.round(rng.next() * 100) / 100 : 0))}`
    case 3:
    case 4: {
      const method = rng.pick(valueSetters)
      return `ts ${id} ${method} ${valueToken(rng, method === "setFlexBasis" || method === "setWidth" || method === "setHeight")}`
    }
    case 5:
      return rng.chance(0.5)
        ? `ts ${id} ${rng.pick(percentSetters)} ${rng.chance(0.1) ? "u" : num(percent(rng))}`
        : `ts ${id} ${rng.pick(autoSetters)}`
    case 6:
    case 7: {
      const method = rng.pick(edgeSetters)
      return `ts ${id} ${method} ${rng.int(0, 8)} ${valueToken(rng, method !== "setPadding")}`
    }
    case 8:
      return rng.chance(0.5)
        ? `ts ${id} ${rng.pick(edgePercentSetters)} ${rng.int(0, 8)} ${num(percent(rng))}`
        : `ts ${id} ${rng.pick(edgeAutoSetters)} ${rng.int(0, 8)}`
    case 9:
      return rng.chance(0.5)
        ? `ts ${id} setGap ${rng.int(0, 2)} ${valueToken(rng, false)}`
        : `ts ${id} setGapPercent ${rng.int(0, 2)} ${num(percent(rng))}`
    case 10:
      return `ts ${id} setBorder ${rng.int(0, 8)} ${rng.chance(0.1) ? "u" : num(rng.int(0, 2))}`
    default:
      return `ts ${id} ${rng.pick(["setIsReferenceBaseline", "setAlwaysFormsContainingBlock"])} ${rng.int(0, 1)}`
  }
}

function measureArgs(rng: Rng): string {
  switch (rng.int(0, 3)) {
    case 0:
      return `text ${rng.int(0, 90)} 0`
    case 1:
      return `fixed ${num(Math.abs(size(rng)))} ${num(Math.abs(size(rng)))}`
    case 2:
      return `frac ${num(rng.int(0, 10) + rng.next())} ${num(rng.int(0, 5) + rng.next())}`
    default:
      return `echo ${num(rng.int(0, 30))} ${num(rng.int(0, 10))}`
  }
}

const layoutToken = (rng: Rng) =>
  rng.chance(0.35) ? rng.pick(["u", "u", "a"]) : num(rng.chance(0.7) ? rng.int(0, 120) : Math.round(rng.next() * 12000) / 100)

function roots(state: State): number[] {
  const out: number[] = []
  state.nodes.forEach((node, id) => {
    if (!node.isFreed() && !node.getParent()) out.push(id)
  })
  return out
}

function layoutAndDump(rng: Rng, state: State) {
  for (const root of roots(state)) {
    if (rng.chance(0.08)) {
      // The native call with unchecked dimensions and directions.
      const bad = () => rng.pick(["NaN", "-1", "Infinity", "1e39", "5", "-0"])
      exec(state, `layr ${root} ${bad()} ${bad()} ${rng.pick([0, 1, 2, 3, 7])}`)
    } else {
      exec(state, `lay ${root} ${layoutToken(rng)} ${layoutToken(rng)} ${rng.pick([1, 1, 1, 1, 2, 2, 0])}`)
    }
    exec(state, `dump ${root}`)
  }
}

function mutation(rng: Rng, state: State) {
  const count = state.nodes.length
  const id = rng.int(0, count - 1)
  const other = rng.int(0, count - 1)
  const live = !freed(state, id)
  const otherLive = !freed(state, other)
  switch (rng.int(0, 24)) {
    case 0:
    case 1:
    case 2:
    case 3:
    case 4:
    case 5:
      return exec(state, styleOp(rng, state, id))
    case 6:
      return exec(state, `ins ${id} ${other} ${rng.int(0, state.nodes[id].getChildCount() + (rng.chance(0.1) ? 1 : 0))}`)
    case 7:
      return exec(state, `rm ${id} ${other}`)
    case 8:
      if (rng.chance(0.3)) return exec(state, `rmall ${id}`)
      return exec(state, `child ${id} ${rng.int(0, 3)}`)
    case 9:
    case 10:
      if (live && otherLive)
        return exec(state, `mv ${id} ${other} ${rng.int(0, state.nodes[id].getChildCount() + (rng.chance(0.15) ? 1 : 0))}`)
      return exec(state, `parent ${id}`)
    case 11:
      return exec(state, rng.chance(0.2) ? `freerec ${id}` : rng.chance(0.5) ? `free ${id}` : `count ${id}`)
    case 12:
      return exec(state, rng.chance(0.5) ? `reset ${id}` : `copy ${id} ${other}`)
    case 13:
      return exec(state, rng.chance(0.6) ? `mark ${id}` : `seen ${id}`)
    case 14:
      if (live) return exec(state, rng.chance(0.5) ? `inval ${id}` : `forget ${id}`)
      return exec(state, `dirty ${id}`)
    case 15:
      return exec(state, rng.chance(0.4) ? `unmeas ${id}` : `meas ${id} ${measureArgs(rng)}`)
    case 16:
      if (live) return exec(state, rng.chance(0.3) ? `nmeas ${id} 0` : `nmeas ${id} ${rng.int(0, 1)} ${measureArgs(rng)}`)
      return exec(state, `hasm ${id}`)
    case 17:
      return exec(state, rng.chance(0.6) ? `dirt ${id}` : `undirt ${id}`)
    case 18:
      return exec(state, `get ${id}`)
    case 19:
      if (live) {
        const group = rng.int(0, 4)
        const kind = rng.int(0, 12)
        const edge = rng.int(0, 10)
        return exec(state, [`ge ${id} ${kind}`, `gf ${id} ${kind}`, `gb ${id} ${edge}`, `gv ${id} ${kind} ${edge}`, `gl ${id} ${rng.int(0, 3)} ${edge}`][group])
      }
      return exec(state, `newl ${id}`)
    case 20: {
      const config = rng.int(0, state.configs.length - 1)
      switch (rng.int(0, 4)) {
        case 0:
          return exec(state, `cfg.scale ${config} ${num(rng.pick([...POINT_SCALES, -1, Infinity, NaN, 1e39]))}`)
        case 1:
          return exec(state, `cfg.errata ${config} ${rng.pick(ERRATA)}`)
        case 2:
          return exec(state, `cfg.web ${config} ${rng.int(0, 1)}`)
        case 3:
          return exec(state, `cfg.rounds ${config} ${rng.int(0, 1)}`)
        default:
          return exec(state, `cfg.get ${config}`)
      }
    }
    case 21:
      return exec(state, `dirty ${id}`)
    case 22:
      return exec(state, `newl ${id}`)
    default: {
      // A fresh node into the tree.
      const fresh = state.nodes.length
      exec(state, `new ${fresh} ${rng.int(0, state.configs.length - 1)}`)
      return exec(state, `ins ${id} ${fresh} ${rng.int(0, state.nodes[id].getChildCount())}`)
    }
  }
}

function program(rng: Rng): State {
  const state = new State()
  const configs = rng.int(1, 2)
  const scene = rng.chance(0.5)
  for (let c = 0; c < configs; c++) {
    exec(state, `cfg ${c}`)
    if (scene && c === 0) {
      exec(state, `cfg.rounds 0 1`)
      continue
    }
    if (rng.chance(0.2)) exec(state, `cfg.web ${c} 1`)
    if (rng.chance(0.35)) exec(state, `cfg.scale ${c} ${num(rng.pick(POINT_SCALES))}`)
    if (rng.chance(0.25)) exec(state, `cfg.errata ${c} ${rng.pick(ERRATA)}`)
    if (rng.chance(0.3)) exec(state, `cfg.rounds ${c} 1`)
    if (rng.chance(0.1)) exec(state, `cfg.exp ${c} 1`)
  }
  const count = rng.int(1, rng.chance(0.2) ? 60 : 24)
  for (let id = 0; id < count; id++) {
    const which = rng.chance(0.05) ? rng.pick([-1, -2]) : rng.chance(0.8) ? 0 : rng.int(0, configs - 1)
    exec(state, `new ${id} ${which}`)
    if (scene) {
      exec(state, `sf ${id} 2 1`)
      for (let edge = 0; edge < 4; edge++) exec(state, `sb ${id} ${edge} 0`)
    }
  }
  for (let id = 1; id < count; id++) {
    if (rng.chance(0.08)) continue
    const parent = rng.chance(0.6) ? rng.int(Math.max(0, id - 4), id - 1) : rng.int(0, id - 1)
    exec(state, `ins ${parent} ${id} ${rng.int(0, state.nodes[parent].getChildCount())}`)
  }
  for (let id = 0; id < count; id++) {
    const styles = rng.int(0, scene ? 4 : 7)
    for (let i = 0; i < styles; i++) exec(state, styleOp(rng, state, id))
    if (scene && rng.chance(0.3)) {
      const unit = rng.pick([1, 1, 2, 3, 0])
      exec(state, `sd ${id} ${rng.int(0, 1)} ${unit} ${num(unit === 2 ? percent(rng) : size(rng))} ${rng.int(0, 1)}`)
    }
    if (state.nodes[id].getChildCount() === 0 && rng.chance(0.4)) {
      if (scene || rng.chance(0.3)) exec(state, `nmeas ${id} ${scene ? 1 : rng.int(0, 1)} ${measureArgs(rng)}`)
      else exec(state, `meas ${id} ${measureArgs(rng)}`)
    }
    if (rng.chance(0.15)) exec(state, `dirt ${id}`)
  }
  layoutAndDump(rng, state)
  const rounds = rng.int(1, 3)
  for (let round = 0; round < rounds; round++) {
    const ops = rng.int(1, 10)
    for (let i = 0; i < ops; i++) mutation(rng, state)
    layoutAndDump(rng, state)
  }
  if (rng.chance(0.2)) exec(state, `get ${rng.int(0, state.nodes.length - 1)}`)
  return state
}

const programs: string[] = []
const results: string[] = []
const finish = (state: State) => {
  programs.push(...state.ops, "end")
  results.push(...state.out, "end")
  state.cleanup()
}
let cases = 0
for (const ops of handPicked) {
  const state = new State()
  for (const op of ops) exec(state, op)
  finish(state)
  cases++
}
const handCount = cases
const count = Number(arg("count", "20000"))
const seed = Number(arg("seed", "1"))
for (let index = 0; index < count; index++) {
  finish(program(new Rng(seed * 1000003 + index)))
  cases++
}
writeFileSync(arg("programs")!, programs.join("\n") + "\n")
writeFileSync(arg("results")!, results.join("\n") + "\n")
const opCount = programs.length - cases
process.stderr.write(`oracle: ${handCount} hand-picked and ${count} random programs, ${opCount} ops\n`)
