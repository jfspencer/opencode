// The TS side of the opencode-tui-markup differential harness: runs each case through the modules the reference
// TUI runs (@opentui/core 0.8.1 and its marked, from port/.reference/node_modules) and returns plain JSON.
//
// Each case is `{ op, ...input }`; see `run` for the ops. Outputs use the shapes of examples/markup-driver.rs.
import { join } from "node:path"

const port = join(import.meta.dir, "../../../..")
const coreDir = join(port, ".reference/node_modules/.bun/@opentui+core@0.8.1+eff4d38d6a4a2cd9/node_modules/@opentui/core")
export const core = await import(join(coreDir, "index.bun.js"))
const markedPath = Bun.resolveSync("marked", coreDir)
export const marked = await import(markedPath)

/** JSON-ready token: drops undefined like JSON.stringify, and turns the `links` array property into a field. */
function plain(value: unknown): unknown {
  return JSON.parse(JSON.stringify(value))
}

export function lex(src: string) {
  try {
    const tokens = marked.Lexer.lex(src, { gfm: true })
    return { tokens: plain(tokens), links: Object.entries(tokens.links).map(([k, v]) => [k, plain(v)]) }
  } catch (error) {
    return { error: String((error as Error).message) }
  }
}

export function lexInline(src: string) {
  try {
    return { tokens: plain(marked.Lexer.lexInline(src)) }
  } catch (error) {
    return { error: String((error as Error).message) }
  }
}

// ---- the renderer the renderables need (no frames are drawn)
const testing = await import(join(coreDir, "testing.bun.js"))
const setup = await testing.createTestRenderer({ width: 100, height: 40 })
export const renderer = setup.renderer
const theme = JSON.parse(await Bun.file(join(import.meta.dir, "theme.json")).text())
export const syntaxStyle = core.SyntaxStyle.fromStyles(theme, renderer.nativeScene)

export async function dispose() {
  await setup.dispose()
}

const rgba = (c: any) => (c ? Array.from(c.buffer as Uint16Array) : null)
export const chunk = (c: any) => ({
  text: c.text,
  fg: rgba(c.fg),
  bg: rgba(c.bg),
  attributes: c.attributes ?? null,
  link: c.link?.url ?? null,
})
const chunks = (list: any[] | undefined | null) => (list ? list.map(chunk) : null)

const EDGE_TOP = 1
const EDGE_BOTTOM = 3
const margin = (r: any, edge: number) => {
  const v = r.getMargin(edge).value
  return Number.isNaN(v) || v === undefined ? 0 : v
}

/** The child tree of a MarkdownRenderable, with object identities numbered in order of first appearance. */
function dumpTree(md: any, uids: WeakMap<object, number>, counter: { n: number }) {
  const uid = (r: object) => {
    if (!uids.has(r)) uids.set(r, ++counter.n)
    return uids.get(r)!
  }
  const node = (r: any): any => {
    const base = { uid: uid(r), id: r.id, marginTop: margin(r, EDGE_TOP), marginBottom: margin(r, EDGE_BOTTOM) }
    if (r instanceof core.CodeRenderable) {
      return {
        kind: "code",
        ...base,
        content: r.content,
        filetype: r.filetype ?? null,
        fg: rgba(r._defaultFg),
        bg: rgba(r._defaultBg),
        conceal: r.conceal,
        drawUnstyledText: r.drawUnstyledText,
        streaming: r.streaming,
        initialStyledText: chunks(r._initialStyledText?.chunks),
        baseHighlight: r.baseHighlight ?? null,
        hooks: r.onChunks === md._linkifyMarkdownChunks && r.onHighlight === md._highlightMarkdownLinks,
      }
    }
    if (r instanceof core.TextTableRenderable) {
      return {
        kind: "table",
        ...base,
        content: r.content.map((row: any[]) => row.map((cell: any) => (Array.isArray(cell) ? cell.map(chunk) : null))),
        columnWidthMode: r.columnWidthMode,
        columnFitter: r.columnFitter,
        wrapMode: r.wrapMode,
        cellPaddingX: r.cellPaddingX,
        cellPaddingY: r.cellPaddingY,
        columnGap: r.columnGap,
        border: r.border,
        outerBorder: r.outerBorder,
        showBorders: r.showBorders,
        borderStyle: r.borderStyle,
        borderColor: rgba(r.borderColor),
        fg: rgba(r._defaultFg),
        selectable: r.selectable,
      }
    }
    if (r instanceof core.TextRenderable) {
      return { kind: "text", ...base, chunks: chunks(r.chunks), width: r.width }
    }
    if (r instanceof core.BoxRenderable) {
      return {
        kind: "box",
        ...base,
        border: r.border,
        borderColor: r.border ? rgba(r.borderColor) : null,
        children: r.getChildren().map(node),
      }
    }
    return { kind: "other", ...base }
  }
  return md.getChildren().map(node)
}

const tableStyle = (o: any) => o

export function markdown(c: any) {
  const uids = new WeakMap<object, number>()
  const counter = { n: 0 }
  renderer._capabilities = { ...(renderer._capabilities ?? {}), hyperlinks: !!c.hyperlinks }
  const steps = c.steps as any[]
  const first = steps[0]
  const md = new core.MarkdownRenderable(renderer, {
    id: "md",
    syntaxStyle,
    content: first.content,
    streaming: first.streaming,
    internalBlockMode: c.internalBlockMode,
    tableOptions: c.tableOptions ? tableStyle(c.tableOptions) : undefined,
    conceal: c.conceal,
    concealCode: c.concealCode,
    fg: c.fg,
    bg: c.bg,
  })
  const out: any[] = []
  try {
    out.push({ tree: dumpTree(md, uids, counter), stable: md._stableBlockCount, tokens: md._parseState ? plain(md._parseState.tokens) : null, stableTokens: md._parseState?.stableTokenCount ?? null })
    for (const step of steps.slice(1)) {
      if (step.content !== undefined) md.content = step.content
      if (step.streaming !== undefined) md.streaming = step.streaming
      if (step.conceal !== undefined) {
        md.conceal = step.conceal
        md.refreshStyles()
      }
      if (step.hyperlinks !== undefined) {
        renderer._capabilities = { ...(renderer._capabilities ?? {}), hyperlinks: step.hyperlinks }
        md.handleCapabilities()
      }
      out.push({ tree: dumpTree(md, uids, counter), stable: md._stableBlockCount, tokens: md._parseState ? plain(md._parseState.tokens) : null, stableTokens: md._parseState?.stableTokenCount ?? null })
    }
  } finally {
    md.destroyRecursively()
  }
  return out
}

export async function run(c: any): Promise<unknown> {
  switch (c.op) {
    case "lex":
      return lex(c.src)
    case "lexInline":
      return lexInline(c.src)
    case "markdown":
      return markdown(c)
    default:
      throw new Error(`unknown op ${c.op}`)
  }
}
