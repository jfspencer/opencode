// The TS side of the opencode-tui-input keymap check: generates programs and runs them, as generated, through the
// reference's own @opentui/keymap 0.8.1, configured as the TUI configures it (the default keys, `enabled` and
// metadata fields, comma bindings, the base-layout fallback, Escape and Backspace on a pending sequence, a `mode`
// layer field requiring `opencode.mode`, a timed leader), on a test host with a target tree and fake timers. Random
// layers (bindings in the binding syntax, commands, targets, priorities, conditions) and random key presses,
// releases, focus changes, data and condition changes, command dispatch, layer replacement and queries. Writes the
// programs and the answers; examples/keymap-driver.rs replays the programs through the Rust port.
//
// usage: bun harness/keymap-oracle.ts --programs <file> --results <file> [--count N] [--seed S]
import { writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const here = dirname(fileURLToPath(import.meta.url))
const tui = join(here, "../../../../.reference/packages/tui/src")
const { Keymap, stringifyKeySequence, KEYMAP_EXTENSION_CONTEXT } = await import(Bun.resolveSync("@opentui/keymap", tui))
const addons = await import(Bun.resolveSync("@opentui/keymap/addons", tui))
const opentuiAddons = await import(Bun.resolveSync("@opentui/keymap/addons/opentui", tui))
const { formatKeySequence, formatCommandBindings } = await import(Bun.resolveSync("@opentui/keymap/extras", tui))

// ---- fake timers for the timed leader (it uses the global setTimeout)
let now = 0
let nextTimer = 1
const timers = new Map<number, { at: number; fn: () => void }>()
globalThis.setTimeout = ((fn: () => void, ms: number) => {
  const id = nextTimer++
  timers.set(id, { at: now + ms, fn })
  return id
}) as any
globalThis.clearTimeout = ((id: number) => void timers.delete(id)) as any
const advance = (ms: number) => {
  const target = now + ms
  while (true) {
    let best: [number, { at: number; fn: () => void }] | undefined
    for (const entry of timers) if (entry[1].at <= target && (!best || entry[1].at < best[1].at)) best = entry
    if (!best) break
    timers.delete(best[0])
    now = best[1].at
    best[1].fn()
  }
  now = target
}

const enc = (text: string) => encodeURIComponent(text) || "%"
const dec = (text: string) => (text === "%" ? "" : decodeURIComponent(text))
const esc = (text: string) => {
  let out = ""
  for (const ch of text) {
    let code = ch.codePointAt(0)!
    if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd
    out += code >= 0x20 && code <= 0x7e && ch !== "\\" ? ch : `\\u{${code.toString(16)}}`
  }
  return out
}

class Target {
  parent: Target | null = null
  isDestroyed = false
  constructor(readonly id: string) {}
}

class Event {
  propagationStopped = false
  defaultPrevented = false
  constructor(
    readonly name: string,
    readonly ctrl: boolean,
    readonly shift: boolean,
    readonly meta: boolean,
    readonly superKey: boolean,
    readonly hyperKey: boolean,
    readonly baseCode: number | undefined,
  ) {}
  get super() {
    return this.superKey
  }
  get hyper() {
    return this.hyperKey || undefined
  }
  preventDefault() {
    this.defaultPrevented = true
  }
  stopPropagation() {
    this.propagationStopped = true
  }
}

class Host {
  metadata = { platform: "linux", primaryModifier: "ctrl", modifiers: { ctrl: "supported", shift: "supported", meta: "supported", super: "unknown", hyper: "unknown" } }
  rootTarget = new Target("root")
  isDestroyed = false
  focused: Target | null = null
  press = new Set<(event: Event) => void>()
  release = new Set<(event: Event) => void>()
  focusListeners = new Set<(target: Target | null) => void>()
  destroyListeners = new Map<Target, Set<() => void>>()
  getFocusedTarget() {
    return this.focused && !this.focused.isDestroyed ? this.focused : null
  }
  getParentTarget(target: Target) {
    return target.parent
  }
  isTargetDestroyed(target: Target) {
    return target.isDestroyed
  }
  onKeyPress(listener: (event: Event) => void) {
    this.press.add(listener)
    return () => this.press.delete(listener)
  }
  onKeyRelease(listener: (event: Event) => void) {
    this.release.add(listener)
    return () => this.release.delete(listener)
  }
  onFocusChange(listener: (target: Target | null) => void) {
    this.focusListeners.add(listener)
    return () => this.focusListeners.delete(listener)
  }
  onTargetDestroy(target: Target, listener: () => void) {
    let set = this.destroyListeners.get(target)
    if (!set) this.destroyListeners.set(target, (set = new Set()))
    set.add(listener)
    return () => set!.delete(listener)
  }
  createCommandEvent() {
    return new Event("command", false, false, false, false, false, undefined)
  }
}

const seq = (parts: readonly any[]) =>
  parts.map((p) => `${esc(p.display)}[${esc(stringifyKeySequence([p]))}${p.tokenName ? `<${p.tokenName}>` : ""}]`).join(" ") || "-"
const attrs = (a: any) => (a ? Object.entries(a).map(([k, v]) => `${k}=${esc(String(v))}`).join(",") : "-")
const cmdName = (c: any) => (c === undefined ? "-" : typeof c === "function" ? "<fn>" : esc(c))
const FORMAT = {
  tokenDisplay: { leader: "ctrl+x" },
  keyNameAliases: { up: "↑", down: "↓", left: "←", right: "→", pageup: "pgup", pagedown: "pgdn", delete: "del" },
  modifierAliases: { meta: "alt" },
}

type LayerSpec = { slot: boolean; target: string; targetMode: string; priority: string; enabled: string; mode: string; bindings: string[][]; commands: string[][] }

class Program {
  ops: string[] = []
  out: string[] = []
  host = new Host()
  keymap: any
  targets = new Map<string, Target>([["root", this.host.rootTarget]])
  vars = new Map<string, boolean>()
  specs = new Map<string, LayerSpec>()
  offs = new Map<string, () => void>()
  slots = new Map<string, (layer: any) => void>()
  nested = false

  constructor() {
    this.keymap = new Keymap(this.host)
    addons.registerDefaultKeys(this.keymap)
    addons.registerEnabledFields(this.keymap)
    addons.registerMetadataFields(this.keymap)
    addons.registerCommaBindings(this.keymap)
    opentuiAddons.registerBaseLayoutFallback(this.keymap)
    addons.registerEscapeClearsPendingSequence(this.keymap)
    addons.registerBackspacePopsPendingSequence(this.keymap)
    this.keymap.registerLayerFields({ mode: (value: unknown, ctx: any) => ctx.require("opencode.mode", value) })
    this.keymap.on("error", (e: any) => this.out.push(`error ${e.code} ${esc(e.message)}`))
    this.keymap.on("warning", (e: any) => this.out.push(`warning ${e.code} ${esc(e.message)}`))
    this.keymap.on("pendingSequence", (parts: any[]) => this.out.push(`P ${seq(parts)}`))
    this.keymap.setData("opencode.mode", "base")
    timers.clear()
    now = 0
  }

  enabled(token: string) {
    if (token === "-") return undefined
    if (token === "t") return true
    if (token === "f") return false
    return () => this.vars.get(token) ?? false
  }

  run(spec: string, name: string) {
    return (ctx: any) => {
      this.out.push(
        `run ${esc(name)} input=${esc(ctx.input)} target=${ctx.target?.id ?? "-"} focused=${ctx.focused?.id ?? "-"} event=${esc(ctx.event.name)} command=${ctx.command ? esc(ctx.command.name) : "-"}`,
      )
      switch (spec) {
        case "reject":
          return false
        case "ok":
          return { ok: true }
        case "fail":
          return { ok: false, reason: "rejected" }
        case "clear":
          this.keymap.clearPendingSequence()
          return
        case "mode":
          this.keymap.setData("opencode.mode", this.keymap.getData("opencode.mode") === "base" ? "other" : "base")
          return
        case "dispatch":
          if (this.nested) return true
          this.nested = true
          this.out.push(`nested ${this.result(this.keymap.dispatchCommand("c0"))}`)
          this.nested = false
          return true
      }
      return undefined
    }
  }

  build(id: string) {
    const spec = this.specs.get(id)!
    const layer: Record<string, unknown> = {}
    if (spec.target !== "-") layer.target = this.targets.get(spec.target)
    if (spec.targetMode !== "-") layer.targetMode = spec.targetMode
    if (spec.priority !== "-") layer.priority = Number(spec.priority)
    const enabled = this.enabled(spec.enabled)
    if (enabled !== undefined) layer.enabled = enabled
    if (spec.mode !== "-") layer.mode = dec(spec.mode)
    layer.bindings = spec.bindings.map(([key, cmd, event, pd, ft, desc, group, extra]) => {
      const binding: Record<string, unknown> = { key: dec(key) }
      if (cmd.startsWith("n:")) binding.cmd = dec(cmd.slice(2))
      if (cmd.startsWith("f:")) binding.cmd = this.run(cmd.slice(2), "<inline>")
      if (event !== "-") binding.event = event
      if (pd !== "-") binding.preventDefault = pd === "1"
      if (ft !== "-") binding.fallthrough = ft === "1"
      if (desc !== "-") binding.desc = dec(desc)
      if (group !== "-") binding.group = dec(group)
      if (extra !== "-") binding[dec(extra)] = true
      return binding
    })
    layer.commands = spec.commands.map(([name, run, en, title, desc, category]) => {
      const command: Record<string, unknown> = { name: dec(name), run: this.run(run, dec(name)) }
      const enabled = this.enabled(en)
      if (enabled !== undefined) command.enabled = enabled
      if (title !== "-") command.title = dec(title)
      if (desc !== "-") command.desc = dec(desc)
      if (category !== "-") command.category = dec(category)
      return command
    })
    return layer
  }

  result(r: any) {
    return `ok=${r.ok} reason=${r.reason ?? "-"} command=${r.command ? esc(r.command.name) : "-"}`
  }

  activeKeys() {
    const keys = this.keymap.getActiveKeys({ includeMetadata: true, includeBindings: true })
    return keys
      .map(
        (k: any) =>
          `${esc(k.display)}[${esc(stringifyKeySequence([{ stroke: k.stroke, display: "", match: "" }]))}]${k.tokenName ? `<${k.tokenName}>` : ""} c=${Number(k.continues)} cmd=${cmdName(k.command)} b=${attrs(k.bindingAttrs)} ca=${attrs(k.commandAttrs)} n=${k.bindings?.length ?? 0}`,
      )
      .join(" ; ")
  }

  exec(op: string) {
    this.ops.push(op)
    const [name, ...args] = op.split(" ")
    const out = this.out
    const km = this.keymap
    switch (name) {
      case "target": {
        const target = new Target(args[0])
        target.parent = this.targets.get(args[1]) ?? null
        this.targets.set(args[0], target)
        return
      }
      case "var":
        this.vars.set(args[0], args[1] === "1")
        return
      case "leader":
        try {
          addons.registerTimedLeader(km, { trigger: dec(args[0]), name: "leader", timeoutMs: Number(args[1]) })
        } catch (error) {
          out.push(`leader-error ${esc((error as Error).message)}`)
        }
        return
      case "layer":
        this.specs.set(args[0], {
          slot: args[1] === "slot",
          target: args[2],
          targetMode: args[3],
          priority: args[4],
          enabled: args[5],
          mode: args[6],
          bindings: [],
          commands: [],
        })
        return
      case "bind":
        this.specs.get(args[0])!.bindings.push(args.slice(1))
        return
      case "cmd":
        this.specs.get(args[0])!.commands.push(args.slice(1))
        return
      case "commit": {
        const spec = this.specs.get(args[0])!
        const layer = this.build(args[0])
        if (spec.slot) {
          if (!this.slots.has(args[0])) {
            // createLayerSlot (services/extension-context.ts), as useBindings makes one.
            const { layers } = km[KEYMAP_EXTENSION_CONTEXT]()
            const slot = layers.createSlot()
            this.slots.set(args[0], (next: unknown) => layers.replaceLayer(slot, next))
          }
          this.slots.get(args[0])!(layer)
        } else {
          this.offs.set(args[0], km.registerLayer(layer))
        }
        out.push(`committed ${args[0]}`)
        return
      }
      case "unreg": {
        const spec = this.specs.get(args[0])
        if (spec?.slot) this.slots.get(args[0])?.(undefined)
        else this.offs.get(args[0])?.()
        out.push(`unregistered ${args[0]}`)
        return
      }
      case "press":
      case "release": {
        const [n, mods, base] = args
        const event = new Event(dec(n), mods[0] === "1", mods[1] === "1", mods[2] === "1", mods[3] === "1", mods[4] === "1", base === "-" ? undefined : Number(base))
        for (const listener of [...(name === "press" ? this.host.press : this.host.release)]) listener(event)
        out.push(`${name} prevented=${Number(event.defaultPrevented)} stopped=${Number(event.propagationStopped)} pending=${seq(km.getPendingSequence())}`)
        return
      }
      case "focus":
        this.host.focused = args[0] === "-" ? null : this.targets.get(args[0])!
        for (const listener of [...this.host.focusListeners]) listener(this.host.focused)
        out.push(`focused ${args[0]}`)
        return
      case "destroy": {
        const target = this.targets.get(args[0])!
        target.isDestroyed = true
        for (const listener of [...(this.host.destroyListeners.get(target) ?? [])]) listener()
        out.push(`destroyed ${args[0]}`)
        return
      }
      case "data":
        km.setData(args[0], args[1] === "-" ? undefined : dec(args[1]))
        out.push(`data ${args[0]}`)
        return
      case "adv":
        advance(Number(args[0]))
        out.push(`now ${now} pending=${seq(km.getPendingSequence())}`)
        return
      case "dispatch":
        out.push(`dispatched ${this.result(km.dispatchCommand(dec(args[0]), { includeCommand: true }))}`)
        return
      case "runcmd":
        out.push(`ran ${this.result(km.runCommand(dec(args[0]), { includeCommand: true }))}`)
        return
      case "pending":
        out.push(`pending ${seq(km.getPendingSequence())} has=${Number(km.hasPendingSequence())}`)
        return
      case "pop":
        out.push(`pop ${Number(km.popPendingSequence())} ${seq(km.getPendingSequence())}`)
        return
      case "clear":
        km.clearPendingSequence()
        out.push("cleared")
        return
      case "active":
        out.push(`active ${this.activeKeys()}`)
        return
      case "commands":
        out.push(`commands ${km.getCommands({ visibility: args[0] }).map((c: any) => `${esc(c.name)}:${c.title ?? "-"}`).join(",")}`)
        return
      case "entries":
        out.push(
          `entries ${km
            .getCommandEntries({ visibility: args[0] })
            .map((e: any) => `${esc(e.command.name)}=${e.bindings.map((b: any) => `${seq(b.sequence)}/${attrs(b.commandAttrs)}`).join("|")}`)
            .join(" ; ")}`,
        )
        return
      case "bindings": {
        const names = args.slice(1).map(dec)
        const map = km.getCommandBindings({ visibility: args[0], commands: names })
        const rows: string[] = []
        for (const [command, bindings] of map)
          rows.push(
            `${esc(command)}=${bindings.map((b: any) => `${seq(b.sequence)}/${b.event}/${Number(b.preventDefault)}${Number(b.fallthrough)}/${attrs(b.attrs)}/${attrs(b.commandAttrs)}`).join("|")} fmt=${esc(formatCommandBindings(bindings, FORMAT) ?? "-")} first=${esc(formatKeySequence(bindings[0]?.sequence, FORMAT))}`,
          )
        out.push(`bindings ${rows.join(" ; ")}`)
        return
      }
      case "parse":
        try {
          out.push(`parsed ${seq(km.parseKeySequence(dec(args[0])))} fmt=${esc(formatKeySequence(km.parseKeySequence(dec(args[0])), FORMAT))}`)
        } catch (error) {
          out.push(`parse-error ${esc((error as Error).message)}`)
        }
        return
      case "matcher":
        try {
          const matcher = km.createKeyMatcher(dec(args[0]))
          const [n, mods] = [dec(args[1]), args[2]]
          const event = new Event(n, mods[0] === "1", mods[1] === "1", mods[2] === "1", mods[3] === "1", mods[4] === "1", undefined)
          out.push(`matcher ${Number(matcher(event))}`)
        } catch (error) {
          out.push(`matcher-error ${esc((error as Error).message)}`)
        }
        return
    }
    throw new Error(`op ${op}`)
  }
}

// ---- generation
class Rng {
  private s: number
  constructor(seed: number) {
    this.s = seed >>> 0 || 1
  }
  next() {
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

const KEYS = [
  "a", "b", "g", "j", "k", "x", "s", "q", "1", "?", "/", "A", "enter", "return", "escape", "up", "down", "pageup",
  "f5", "f12", "F3", "tab", "space", " ", "backspace", "delete", "ctrl+x", "ctrl+s", "ctrl+c", "ctrl+1", "ctrl+shift+k",
  "shift+tab", "meta+a", "alt+x", "option+q", "super+s", "hyper+h", "ctrl+alt+delete", "CTRL+X", "Ctrl+K", "control+j",
  "gg", "dd", "ab", "jk", "<leader>a", "<leader>q", "<leader>", "<LEADER>g", "<other>x", "{pat}", "ctrl+x ctrl+s",
  "ctrl + x", "shift + tab", "j,k", "a, b", "x,,y", "<>", "", "ctrl+", "+", "a+b", "f1", "kp5", "ctrl+kp5",
  "leftbracket", "lt", "comma", "ctrl+f1", "<", "{", "xg", "gx", "ctrl+xa", "ctrl+x,ctrl+c", "é", "ctrl+é", "😀",
]
const PRESSES: [string, string, string][] = [
  ["a", "00000", "-"], ["b", "00000", "-"], ["g", "00000", "-"], ["j", "00000", "-"], ["k", "00000", "-"],
  ["x", "00000", "-"], ["s", "00000", "-"], ["q", "00000", "-"], ["1", "00000", "-"], ["?", "01000", "-"],
  ["/", "00000", "-"], ["a", "01000", "-"], ["return", "00000", "-"], ["escape", "00000", "-"], ["up", "00000", "-"],
  ["pageup", "00000", "-"], ["f5", "00000", "-"], ["f12", "00000", "-"], ["f3", "00000", "-"], ["tab", "00000", "-"],
  ["tab", "01000", "-"], ["space", "00000", "-"], ["backspace", "00000", "-"], ["delete", "00000", "-"],
  ["x", "10000", "-"], ["s", "10000", "-"], ["c", "10000", "-"], ["k", "11000", "-"], ["a", "00100", "-"],
  ["x", "00100", "-"], ["s", "00010", "-"], ["h", "00001", "-"], ["delete", "10100", "-"], ["ㅊ", "10000", "99"],
  ["&", "10000", "49"], ["ж", "00000", "106"], ["X", "00000", "-"], ["kp5", "00000", "-"], ["leftbracket", "00000", "-"],
  ["é", "00000", "-"], ["é", "10000", "-"], ["1", "10000", "-"], ["", "00000", "-"], ["j", "00000", "107"],
]
const COMMON = ["a", "g", "j", "k", "x", "gg", "jk", "ab", "gx", "ctrl+x", "ctrl+c", "<leader>a", "<leader>q", "escape", "enter", "return", "up", "tab", "shift+tab", "space", "j,k", "ctrl+x,ctrl+c"]
const COMMON_PRESSES: [string, string, string][] = [
  ["a", "00000", "-"], ["b", "00000", "-"], ["g", "00000", "-"], ["j", "00000", "-"], ["k", "00000", "-"],
  ["x", "00000", "-"], ["q", "00000", "-"], ["x", "10000", "-"], ["c", "10000", "-"], ["escape", "00000", "-"],
  ["return", "00000", "-"], ["up", "00000", "-"], ["tab", "00000", "-"], ["tab", "01000", "-"], ["space", "00000", "-"],
  ["backspace", "00000", "-"], ["ㅊ", "10000", "99"],
]
const RUNS = ["handled", "handled", "handled", "reject", "ok", "fail", "clear", "mode", "dispatch"]

function program(rng: Rng): Program {
  const p = new Program()
  const targetIds = ["root"]
  for (let i = 0; i < rng.int(1, 4); i++) {
    const id = `t${i}`
    p.exec(`target ${id} ${rng.pick(targetIds)}`)
    targetIds.push(id)
  }
  if (rng.chance(0.6)) p.exec(`leader ${enc(rng.pick(["ctrl+x", "ctrl+x", "ctrl+x", "space", "ctrl+g", "<leader>", "a b"]))} ${rng.pick([2000, 1500, 50])}`)
  for (let v = 0; v < 3; v++) p.exec(`var v${v} ${rng.int(0, 1)}`)
  const layerCount = rng.int(1, 6)
  const commandNames = ["c0", "c1", "c2", "c3", "nav.up", "missing"]
  const layers: string[] = []
  for (let l = 0; l < layerCount; l++) {
    const id = `L${l}`
    layers.push(id)
    const target = rng.chance(0.6) ? "-" : rng.pick(targetIds)
    const targetMode = rng.chance(0.8) ? "-" : rng.pick(["focus", "focus-within"])
    const priority = rng.chance(0.6) ? "-" : String(rng.int(-2, 3))
    const enabled = rng.pick(["-", "-", "-", "t", "f", "v0", "v1", "v2"])
    const mode = rng.pick(["-", "-", enc("base"), enc("other")])
    p.exec(`layer ${id} ${rng.chance(0.4) ? "slot" : "reg"} ${target} ${targetMode} ${priority} ${enabled} ${mode}`)
    for (let c = 0; c < rng.int(0, 3); c++) {
      const name = rng.chance(0.05) ? rng.pick(["", " c4", "a b"]) : rng.pick(commandNames.slice(0, 5))
      p.exec(
        `cmd ${id} ${enc(name)} ${rng.pick(RUNS)} ${rng.pick(["-", "-", "t", "f", "v0", "v1"])} ${rng.chance(0.3) ? enc(rng.pick(["Title", "Title", " Spaced ", ""])) : "-"} ${rng.chance(0.3) ? enc("desc") : "-"} ${rng.chance(0.2) ? enc("cat") : "-"}`,
      )
    }
    for (let b = 0; b < rng.int(0, 6); b++) {
      const cmd = rng.chance(0.15) ? "-" : rng.chance(0.25) ? `f:${rng.pick(RUNS)}` : `n:${enc(rng.chance(0.05) ? " " : rng.pick(commandNames))}`
      p.exec(
        `bind ${id} ${enc(rng.chance(0.75) ? rng.pick(COMMON) : rng.pick(KEYS))} ${cmd} ${rng.chance(0.85) ? "-" : rng.pick(["press", "release", "bogus"])} ${rng.chance(0.8) ? "-" : rng.pick(["0", "1"])} ${rng.chance(0.8) ? "-" : rng.pick(["0", "1"])} ${rng.chance(0.2) ? enc(rng.pick(["Desc", "Desc", "Desc", " Desc ", ""])) : "-"} ${rng.chance(0.1) ? enc("Group") : "-"} ${rng.chance(0.05) ? enc("unknownField") : "-"}`,
      )
    }
    p.exec(`commit ${id}`)
  }
  const ops = rng.int(5, 40)
  for (let i = 0; i < ops; i++) {
    const r = rng.int(0, 30)
    if (r < 14) {
      const [n, mods, base] = rng.chance(0.75) ? rng.pick(COMMON_PRESSES) : rng.pick(PRESSES)
      p.exec(`${rng.chance(0.08) ? "release" : "press"} ${enc(n)} ${mods} ${base}`)
      if (rng.chance(0.4)) {
        const [n2, mods2, base2] = rng.pick(COMMON_PRESSES)
        p.exec(`press ${enc(n2)} ${mods2} ${base2}`)
      }
    } else if (r === 14) p.exec(`focus ${rng.chance(0.2) ? "-" : rng.pick(targetIds)}`)
    else if (r === 15) p.exec(`var v${rng.int(0, 2)} ${rng.int(0, 1)}`)
    else if (r === 16) p.exec(`data opencode.mode ${rng.pick([enc("base"), enc("other"), "-"])}`)
    else if (r === 17) p.exec(`dispatch ${enc(rng.pick([...commandNames, "", " c1 "]))}`)
    else if (r === 18) p.exec(`runcmd ${enc(rng.pick(commandNames))}`)
    else if (r === 19) p.exec(rng.pick(["pending", "pop", "clear"]))
    else if (r === 20) p.exec("active")
    else if (r === 21) p.exec(`commands ${rng.pick(["registered", "reachable", "active"])}`)
    else if (r === 22) p.exec(`entries ${rng.pick(["registered", "reachable", "active"])}`)
    else if (r === 23) p.exec(`bindings ${rng.pick(["registered", "reachable", "active"])} ${commandNames.slice(0, rng.int(1, 6)).map(enc).join(" ")}`)
    else if (r === 24) p.exec(`adv ${rng.pick([10, 49, 50, 1000, 1999, 2000, 2500])}`)
    else if (r === 25) p.exec(`parse ${enc(rng.pick(KEYS))}`)
    else if (r === 26) {
      const [n, mods] = rng.pick(PRESSES)
      p.exec(`matcher ${enc(rng.pick(KEYS))} ${enc(n)} ${mods}`)
    } else if (r === 27) {
      const id = rng.pick(layers)
      if (rng.chance(0.6)) p.exec(`commit ${id}`)
      else p.exec(`unreg ${id}`)
    } else if (r === 28 && targetIds.length > 1) p.exec(`destroy ${rng.pick(targetIds.slice(1))}`)
    else {
      // change a layer's data, then commit (a slot replace that is not kept)
      const id = rng.pick(layers)
      p.exec(`bind ${id} ${enc(rng.pick(KEYS))} n:${enc(rng.pick(commandNames))} - - - - - -`)
      p.exec(`commit ${id}`)
    }
  }
  p.exec("pending")
  p.exec("active")
  return p
}

const programs: string[] = []
const results: string[] = []
const count = Number(arg("count", "20000"))
const seed = Number(arg("seed", "1"))
for (let index = 0; index < count; index++) {
  const p = program(new Rng(seed * 1000003 + index))
  programs.push(...p.ops, "end")
  results.push(...p.out, "end")
}
writeFileSync(arg("programs")!, programs.join("\n") + "\n")
writeFileSync(arg("results")!, results.join("\n") + "\n")
process.stderr.write(`keymap oracle: ${count} random programs, ${programs.length - count} ops\n`)
