// The TS side of the theme cases: what the reference computes for a case, in the shape the Rust driver prints
// (examples/tui-driver.rs).
import path from "node:path"

const reference = process.env.OPENCODE_TUI_REFERENCE!
const tui = path.join(reference, "packages/tui/src")
const themeIndex = await import(path.join(tui, "theme/index.ts"))
const system = await import(path.join(tui, "theme/system.ts"))
const color = await import(path.join(tui, "theme/color.ts"))
const pkg = await import(path.join(reference, "packages/theme/src/tui/index.ts"))
const core = await import(Bun.resolveSync("@opentui/core", tui))
const { RGBA, SyntaxStyle } = core
// generateSyntax builds a native SyntaxStyle from its rules; the port returns the rules, so capture them.
SyntaxStyle.fromTheme = (rules: unknown) => rules

export type Case =
  | { kind: "builtin"; name: string; newDesigns: boolean; mode: "dark" | "light" }
  | { kind: "document"; input: unknown; name: string }
  | { kind: "v1"; input: unknown; newDesigns: boolean }
  | { kind: "system"; colors: { palette: (string | null)[]; defaultForeground: string | null; defaultBackground: string | null }; mode: "dark" | "light"; newDesigns: boolean }
  | { kind: "discover"; directories: string[] }
  | { kind: "tint"; base: string; overlay: string; alpha: number }

function hex(rgba: any) {
  const [r, g, b, a] = rgba.toInts()
  const h = (x: number) => x.toString(16).padStart(2, "0")
  return `#${h(r)}${h(g)}${h(b)}${a === 255 ? "" : h(a)}`
}

function viewJson(view: any) {
  const c = (rgba: any) => {
    if (!(rgba instanceof RGBA)) return null
    const source = view.source(rgba)
    return source ? { hex: hex(rgba), hue: source.hue, step: source.step } : hex(rgba)
  }
  const scale = (s: any) => [100, 200, 300, 400, 500, 600, 700, 800, 900].map((step) => hex(s[step]))
  const stateful = (s: any) => ({
    base: c(s.base),
    disabled: c(s.disabled),
    pressed: c(s.pressed),
    focused: c(s.focused),
    selected: c(s.selected),
    hovered: c(s.hovered),
  })
  const actions = (a: any) => ({ primary: stateful(a.primary), secondary: stateful(a.secondary), destructive: stateful(a.destructive) })
  const kinds = ["error", "warning", "success", "info"]
  const t = view
  return {
    hue: Object.fromEntries(Object.entries(t.hue).map(([n, s]) => [n, scale(s)])),
    categorical: t.categorical.map(scale),
    text: {
      base: c(t.text.base),
      muted: c(t.text.muted),
      faint: c(t.text.faint),
      action: actions(t.text.action),
      formfield: stateful(t.text.formfield),
      feedback: Object.fromEntries(kinds.map((k) => [k, { base: c(t.text.feedback[k].base), muted: c(t.text.feedback[k].muted) }])),
    },
    background: {
      base: c(t.background.base),
      raised: { base: c(t.background.raised.base), high: c(t.background.raised.high), max: c(t.background.raised.max) },
      action: actions(t.background.action),
      formfield: stateful(t.background.formfield),
      feedback: Object.fromEntries(kinds.map((k) => [k, { base: c(t.background.feedback[k].base) }])),
    },
    border: { base: c(t.border.base) },
    scrollbar: { base: c(t.scrollbar.base) },
    diff: {
      text: { added: c(t.diff.text.added), removed: c(t.diff.text.removed), context: c(t.diff.text.context), hunkHeader: c(t.diff.text.hunkHeader) },
      background: { added: c(t.diff.background.added), removed: c(t.diff.background.removed), context: c(t.diff.background.context) },
      highlight: { added: c(t.diff.highlight.added), removed: c(t.diff.highlight.removed) },
      lineNumber: {
        text: c(t.diff.lineNumber.text),
        background: { added: c(t.diff.lineNumber.background.added), removed: c(t.diff.lineNumber.background.removed) },
      },
    },
    syntax: Object.fromEntries(pkg.SyntaxToken.literals.map((k: string) => [k, c(t.syntax[k])])),
    markdown: Object.fromEntries(pkg.MarkdownToken.literals.map((k: string) => [k, c(t.markdown[k])])),
  }
}

/// Every token of the base view shifted with decrease(1), increase(1), decrease(2.7), increase(-3), as hex.
function shifts(view: any) {
  const out: string[] = []
  const walk = (value: any) => {
    if (value instanceof RGBA) {
      out.push([view.decrease(value), view.increase(value), view.decrease(value, 2.7), view.increase(value, -3)].map(hex).join(" "))
      return
    }
    if (value && typeof value === "object" && !Array.isArray(value)) for (const key of Object.keys(value).sort()) walk(value[key])
  }
  walk({ text: view.text, background: view.background, border: view.border, scrollbar: view.scrollbar, diff: view.diff, syntax: view.syntax, markdown: view.markdown })
  return out
}

function syntaxRules(theme: any) {
  return pkg.generateSyntax(theme, undefined).map((rule: any) => {
    const { foreground, background, ...flags } = rule.style
    const style: any = { foreground: hex(foreground) }
    if (background) style.background = hex(background)
    for (const flag of ["bold", "italic", "underline"]) if (flags[flag]) style[flag] = true
    return { scope: rule.scope, style }
  })
}

function resolved(document: any) {
  const modes = pkg.themeModes(document)
  const out: any = { modes }
  for (const mode of modes) {
    const theme = pkg.resolveThemeDocument(document, mode)
    out[mode] = {
      base: viewJson(theme),
      dialog: viewJson(theme.surface("dialog")),
      footer: viewJson(theme.surface("dialog.footer")),
      shifts: shifts(theme),
      syntax: syntaxRules(theme),
    }
  }
  return out
}

function load(source: any, name: string, requested: "dark" | "light", newDesigns: boolean) {
  const document = themeIndex.parseTheme(source, name, { newDesigns })
  const modes = pkg.themeModes(document)
  const mode = modes.includes(requested) ? requested : (modes[0] ?? requested)
  const theme = pkg.resolveThemeDocument(document, mode)
  return {
    modes,
    mode,
    base: viewJson(theme),
    dialog: viewJson(theme.surface("dialog")),
    footer: viewJson(theme.surface("dialog.footer")),
    shifts: shifts(theme),
    syntax: syntaxRules(theme),
  }
}

const guard = (fn: () => unknown) => {
  try {
    return { ok: fn() }
  } catch (error: any) {
    return { error: error instanceof Error ? error.message : String(error) }
  }
}

export function runTheme(c: Case): unknown {
  switch (c.kind) {
    case "builtin":
      return guard(() => {
        const sources = c.newDesigns ? { opencode2: themeIndex.getOpenCode2Theme(), ...themeIndex.allThemes() } : themeIndex.allThemes()
        return load(sources[c.name], c.name, c.mode, c.newDesigns)
      })
    case "document":
      return guard(() => resolved(pkg.parseThemeDocument(c.input, c.name)))
    case "v1":
      return guard(() => {
        const document = c.newDesigns ? pkg.migrateV1NewDesigns(c.input) : pkg.migrateV1(c.input)
        return resolved(document)
      })
    case "system":
      return guard(() => {
        const source = system.generateSystem(c.colors, c.mode)
        return { mode: system.terminalMode(c.colors) ?? null, ...load(source, "system", c.mode, c.newDesigns) }
      })
    case "tint":
      return hex(color.tint(RGBA.fromHex(c.base), RGBA.fromHex(c.overlay), c.alpha))
    case "discover":
      return null
  }
}

export const builtinNames = () => [...Object.keys(themeIndex.allThemes()), "opencode2"]
