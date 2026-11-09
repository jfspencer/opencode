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

// ---- tree-sitter: OpenTUI's default parsers plus the TUI's (parsers-config.ts), read from local copies of what
// the TUI downloads: the WASMs fetched by run.sh and the bundled query files (the same bytes as the URLs serve).
const crate = join(import.meta.dir, "..")
const work = process.env.MARKUP_WORK ?? join(port, "rust/target/opencode-tui-markup")
const parsersConfig = (await import(join(port, ".reference/packages/tui/src/parsers-config.ts"))).default
core.addDefaultParsers(
  parsersConfig.parsers.map((p: any) => ({
    filetype: p.filetype,
    ...(p.aliases ? { aliases: p.aliases } : {}),
    wasm: join(work, "wasm", p.wasm.split("/").pop()),
    queries: {
      highlights: p.queries.highlights.map((_: string, i: number) => join(crate, "queries", p.filetype, `highlights-${i}.scm`)),
      ...(p.queries.injections
        ? { injections: p.queries.injections.map((_: string, i: number) => join(crate, "queries", p.filetype, `injections-${i}.scm`)) }
        : {}),
    },
  })),
)
export const client = new core.TreeSitterClient({ dataPath: join(work, "ts-data") })
await client.initialize()

export async function dispose() {
  await client.destroy()
  await setup.dispose()
}

const highlightJson = (h: any[]) => (h.length > 3 ? [h[0], h[1], h[2], plain(h[3])] : [h[0], h[1], h[2]])

/** The shared client, or for `fresh` cases a new one: some grammar scanners read uninitialised state, so their
 * results depend on what the same parser parsed before (FRESH_FILETYPES in diff.ts). */
async function withClient<T>(c: any, f: (client: any) => Promise<T>): Promise<T> {
  if (!c.fresh) return f(client)
  const own = new core.TreeSitterClient({ dataPath: join(work, "ts-data") })
  await own.initialize()
  try {
    return await f(own)
  } finally {
    await own.destroy()
  }
}

export async function highlight(c: any) {
  const result: any = await withClient(c, (client) => client.highlightOnce(c.content, c.filetype))
  return {
    highlights: result.highlights ? result.highlights.map(highlightJson) : null,
    warning: result.warning ?? null,
    error: result.error ?? null,
  }
}

/** Runs a CodeRenderable to the end of its highlighting and returns what it last gave its text buffer. */
export async function code(c: any) {
  return withClient(c, (client) => runCode(c, client))
}

async function runCode(c: any, client: any) {
  const r = new core.CodeRenderable(renderer, {
    content: c.content,
    filetype: c.filetype,
    syntaxStyle,
    conceal: c.conceal,
    baseHighlight: c.baseHighlight,
    treeSitterClient: client,
    drawUnstyledText: true,
  })
  let captured: unknown = null
  const buffer = r.textBuffer
  const setStyledText = buffer.setStyledText.bind(buffer)
  const setText = buffer.setText.bind(buffer)
  buffer.setStyledText = (st: any) => {
    captured = st.chunks.map(chunk)
    return setStyledText(st)
  }
  buffer.setText = (t: string) => {
    captured = "plain"
    return setText(t)
  }
  renderer.root.add(r)
  try {
    for (let i = 0; i < 200 && (r._highlightsDirty || r.isHighlighting || i === 0); i++) {
      await setup.renderOnce()
      await r.highlightingDone
    }
    return { chunks: captured, lineSources: r._renderedLineSources ?? null }
  } finally {
    r.destroyRecursively()
  }
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

/** Renders markdown with highlighting until every code child is done, and returns what each code child last gave
 * its text buffer, in tree order. */
export async function markdownRender(c: any) {
  renderer._capabilities = { ...(renderer._capabilities ?? {}), hyperlinks: !!c.hyperlinks }
  const md = new core.MarkdownRenderable(renderer, {
    id: "md",
    syntaxStyle,
    content: c.content,
    streaming: false,
    internalBlockMode: c.internalBlockMode,
    tableOptions: c.tableOptions,
    conceal: c.conceal,
    fg: c.fg,
    bg: c.bg,
    treeSitterClient: client,
  })
  const codes: { r: any; captured: unknown }[] = []
  const walk = (r: any) => {
    if (r instanceof core.CodeRenderable) {
      const entry = { r, captured: null as unknown }
      const buffer = r.textBuffer
      const setStyledText = buffer.setStyledText.bind(buffer)
      const setText = buffer.setText.bind(buffer)
      buffer.setStyledText = (st: any) => {
        entry.captured = st.chunks.map(chunk)
        return setStyledText(st)
      }
      buffer.setText = (t: string) => {
        entry.captured = "plain"
        return setText(t)
      }
      codes.push(entry)
    }
    for (const child of r.getChildren()) walk(child)
  }
  walk(md)
  renderer.root.add(md)
  try {
    for (let i = 0; i < 200; i++) {
      await setup.renderOnce()
      await Promise.all(codes.map((e) => e.r.highlightingDone))
      if (i > 0 && codes.every((e) => !e.r._highlightsDirty && !e.r.isHighlighting)) break
    }
    return codes.map((e) => ({ id: e.r.id, chunks: e.captured }))
  } finally {
    md.destroyRecursively()
  }
}

// ---- diffs: jsdiff as @opentui/core resolves it, and the arguments DiffRenderable hands each pane's gutter.
export const jsdiff = await import(Bun.resolveSync("diff", coreDir))
let sides: any[] = []
const diffProto = core.DiffRenderable.prototype as any
const createOrUpdateSide = diffProto.createOrUpdateSide
diffProto.createOrUpdateSide = function (side: string, target: any, colors: Map<number, any>, signs: Map<number, any>, numbers: Map<number, number>, hide: Set<number>, width: string) {
  sides.push({
    side,
    content: target.content,
    lineColors: [...colors].map(([k, v]) => [k, { gutter: rgba(v.gutter), content: rgba(v.content) }]),
    lineSigns: [...signs].map(([k, v]) => [k, { after: v.after, afterColor: rgba(v.afterColor) }]),
    lineNumbers: [...numbers],
    hideLineNumbers: [...hide].sort((a, b) => a - b),
    width,
  })
  return createOrUpdateSide.call(this, side, target, colors, signs, numbers, hide, width)
}

export function diff(c: any) {
  let patches: unknown
  try {
    patches = { patches: plain(jsdiff.parsePatch(c.diff)) }
  } catch (error) {
    patches = { error: String((error as Error).message) }
  }
  sides = []
  const r = new core.DiffRenderable(renderer, { diff: c.diff, view: c.view, syntaxStyle, treeSitterClient: client })
  try {
    const error = r.errorTextRenderable ? r.errorTextRenderable.chunks.map((ch: any) => ch.text).join("") : null
    return { ...(patches as object), sides, hunkStartLines: [...r._hunkStartLines], errorText: error }
  } finally {
    r.destroyRecursively()
  }
}

// ---- tables: TextTableRenderable.computeLayout with every cell measure it asks for recorded.
export function table(c: any) {
  const content = c.cells.map((row: (string | null)[]) => row.map((t) => (t === null ? null : [{ __isChunk: true, text: t }])))
  const r: any = new core.TextTableRenderable(renderer, { content, ...c.options })
  try {
    const measures: any[] = []
    r._cells.forEach((row: any[], ri: number) =>
      row.forEach((cell: any, ci: number) => {
        const view = cell.textBufferView
        const measure = view.measureForDimensions.bind(view)
        view.measureForDimensions = (width: number, height: number) => {
          const m = measure(width, height)
          measures.push([ri, ci, width, m ? { lineCount: m.lineCount, widthColsMax: m.widthColsMax } : null])
          return m
        }
      }),
    )
    const layout = r.computeLayout(r.resolveLayoutWidthConstraint(c.maxWidth ?? undefined))
    return {
      layout: {
        columnWidths: layout.columnWidths,
        rowHeights: layout.rowHeights,
        columnOffsets: layout.columnOffsets,
        rowOffsets: layout.rowOffsets,
        tableWidth: layout.tableWidth,
        tableHeight: layout.tableHeight,
      },
      measures,
    }
  } finally {
    r.destroyRecursively()
  }
}

export async function run(c: any): Promise<unknown> {
  switch (c.op) {
    case "lex":
      return lex(c.src)
    case "lexInline":
      return lexInline(c.src)
    case "markdown":
      return markdown(c)
    case "highlight":
      return highlight(c)
    case "code":
      return code(c)
    case "markdownRender":
      return markdownRender(c)
    case "diff":
      return diff(c)
    case "table":
      return table(c)
    default:
      throw new Error(`unknown op ${c.op}`)
  }
}
