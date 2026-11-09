// The TS side of the differential check: runs each case's ops on @opentui/core 0.8.1's own classes (which call
// libopentui.so) and answers each op as examples/text-driver.rs does.
import { join } from "node:path"

const reference = process.env.OPENCODE_REFERENCE ?? join(import.meta.dir, "../../../../.reference")
const core = await import(join(reference, "packages/tui/node_modules/@opentui/core/index.bun.js"))
const { ResourceContext, TextBuffer, TextBufferView, RGBA, SyntaxStyle, NativeError, NativeStatus, StyledText, OptimizedBuffer } =
  core

export type Op = [string, ...any[]]
export type Case = { m: string; ops: Op[] }

const color = (c: number[] | null | undefined) => (c ? RGBA.fromInts(c[0], c[1], c[2], c[3]) : undefined)

function fail(error: unknown) {
  if (error instanceof NativeError) return { err: NativeStatus[(error as any).status] }
  return { err: `js:${(error as Error).message}` }
}

const lineInfo = (i: any) => [i.lineStartCols, i.lineWidthCols, i.lineSources, i.lineWraps, i.lineWidthColsMax]

type TextState = { tb: any; view: any; style: any; method: string }

/** Draws into a cleared buffer and reads it back as `getSpanLines()` runs. */
export function draw(method: string, paint: (buffer: any) => void, width: number, height: number) {
  const buffer = OptimizedBuffer.create(width, height, method, { owner: context })
  try {
    buffer.clear(RGBA.fromInts(0, 0, 0, 255))
    paint(buffer)
    return buffer
      .getSpanLines()
      .map((line: any) => line.spans.map((s: any) => [s.text, [...s.fg.buffer], [...s.bg.buffer], s.attributes, s.width]))
  } finally {
    buffer.destroy()
  }
}

function textOp(s: TextState, op: Op): unknown {
  const [name, ...a] = op
  const { tb, view } = s
  switch (name) {
    case "setText":
      return tb.setText(a[0]), null
    case "append":
      return tb.append(a[0]), null
    case "setStyledText":
      return (
        tb.setStyledText(
          new StyledText(
            a[0].map((c: any) => ({
              __isChunk: true,
              text: c.text,
              fg: color(c.fg),
              bg: color(c.bg),
              attributes: c.attributes,
              link: c.link === undefined ? undefined : { url: c.link },
            })),
          ),
        ),
        null
      )
    case "clear":
      return tb.clear(), null
    case "reset":
      return tb.reset(), null
    case "tabWidth":
      return tb.setTabWidth(a[0]), null
    case "addHighlight":
      return tb.addHighlight(a[0], hl(a[1])), null
    case "addHighlightByCharRange":
      return tb.addHighlightByCharRange(hl(a[0])), null
    case "removeHighlightsByRef":
      return tb.removeHighlightsByRef(a[0]), null
    case "clearLineHighlights":
      return tb.clearLineHighlights(a[0]), null
    case "clearAllHighlights":
      return tb.clearAllHighlights(), null
    case "registerStyle": {
      const spec = a[1]
      return s.style.registerStyle(a[0], { ...spec, fg: color(spec.fg), bg: color(spec.bg) })
    }
    case "syntaxStyle":
      return tb.setSyntaxStyle(a[0] ? s.style : null), null
    case "info":
      return [tb.length, tb.byteSize, tb.getLineCount(), tb.getHighlightCount(), tb.getTabWidth()]
    case "text":
      return tb.getPlainText()
    case "range":
      return tb.getTextRange(a[0], a[1])
    case "hl":
      return tb.getLineHighlights(a[0]).map((h: any) => [h.start, h.end, h.styleId, h.priority, h.hlRef])
    case "wrapMode":
      return view.setWrapMode(a[0]), null
    case "wrapWidth":
      return view.setWrapWidth(a[0]), null
    case "viewport":
      return view.setViewport(a[0], a[1], a[2], a[3]), null
    case "viewportSize":
      return view.setViewportSize(a[0], a[1]), null
    case "firstLineOffset":
      return view.setFirstLineOffset(a[0]), null
    case "truncate":
      return view.setTruncate(a[0]), null
    case "align":
      return view.setTextAlign(a[0]), null
    case "tabIndicator":
      return view.setTabIndicator(a[0]), null
    case "lineInfo":
      return lineInfo(view.lineInfo)
    case "logical":
      return lineInfo(view.logicalLineInfo)
    case "sources":
      return view.getLineSources(a[0], a[1])
    case "measure": {
      const m = view.measureForDimensions(a[0], a[1])
      return [m.lineCount, m.widthColsMax]
    }
    case "vcount":
      return view.getVirtualLineCount()
    case "sel":
      return view.setSelection(a[0], a[1], color(a[2]), color(a[3])), null
    case "usel":
      return view.updateSelection(a[0], color(a[1]), color(a[2])), null
    case "rsel":
      return view.resetSelection(), null
    case "lsel":
      return view.setLocalSelection(a[0], a[1], a[2], a[3], undefined, undefined, a[4])
    case "ulsel":
      return view.updateLocalSelection(a[0], a[1], a[2], a[3], undefined, undefined, a[4])
    case "rlsel":
      return view.resetLocalSelection(), null
    case "occ":
      return view.setSelectionOccupancy(a[0]), null
    case "selinfo": {
      const sel = view.getSelection()
      return [sel ? [sel.start, sel.end] : null, view.getSelectedText(), view.getSelectionOccupancy()]
    }
    case "defaults":
      return tb._setDefaults(color(a[0]) ?? null, color(a[1]) ?? null, a[2]), null
    case "tabColor":
      return view.setTabIndicatorColor(color(a[0])), null
    case "draw":
      return draw(s.method, (buffer) => buffer.drawTextBuffer(view, a[2], a[3]), a[0], a[1])
    case "newView":
      s.view.destroy()
      s.view = TextBufferView.create(tb)
      return null
  }
  throw new Error(`unknown op ${name}`)
}

const hl = (h: number[]) => ({ start: h[0], end: h[1], styleId: h[2], priority: h[3], hlRef: h[4] })

let context: any
export function runCase(c: Case): unknown[] {
  context ??= new ResourceContext({ objectCapacity: 1 << 16, renderCellsMax: 1 << 22 })
  const tb = TextBuffer.create(c.m, context)
  const state: TextState = { tb, view: TextBufferView.create(tb), style: SyntaxStyle.create(context), method: c.m }
  const results: unknown[] = []
  try {
    for (const op of c.ops) {
      try {
        results.push(textOp(state, op))
      } catch (error) {
        results.push(fail(error))
      }
    }
  } finally {
    state.view.destroy()
    tb.destroy()
    state.style.destroy()
  }
  return results
}
