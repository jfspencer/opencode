// The TS side of the differential check: runs each case's ops on @opentui/core 0.8.1's own classes (which call
// libopentui.so) and answers each op as examples/text-driver.rs does.
import { join } from "node:path"

const reference = process.env.OPENCODE_REFERENCE ?? join(import.meta.dir, "../../../../.reference")
const core = await import(join(reference, "packages/tui/node_modules/@opentui/core/index.bun.js"))
const {
  ResourceContext,
  TextBuffer,
  TextBufferView,
  RGBA,
  SyntaxStyle,
  NativeError,
  NativeStatus,
  StyledText,
  OptimizedBuffer,
  EditBuffer,
  EditorView,
  measureEditText,
} = core

export type Op = [string, ...any[]]
export type Case = { m: string; ops: Op[]; k?: "edit"; w?: number; h?: number }

const color = (c: number[] | null | undefined) => (c ? RGBA.fromInts(c[0], c[1], c[2], c[3]) : undefined)

function fail(error: unknown) {
  if (error instanceof NativeError) return { err: NativeStatus[(error as any).status] }
  if ((error as Error).message === "ExtmarksController is destroyed") return { err: "Destroyed" }
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
type EditState = { eb: any; view: any; style: any; method: string; events: string[] }

const mark = (e: any) => [e.id, e.start, e.end, e.virtual, e.styleId ?? null, e.priority ?? null, e.typeId]
const visual = (v: any) => [v.visualRow, v.visualCol, v.logicalRow, v.logicalCol, v.offset]
const logical = (c: any) => (c ? [c.row, c.col, c.offset] : null)

async function editOp(s: EditState, op: Op): Promise<unknown> {
  const [name, ...a] = op
  const { eb, view } = s
  switch (name) {
    case "eSetText":
      return eb.setText(a[0]), null
    case "eReplaceText":
      return eb.replaceText(a[0]), null
    case "eInsert":
      return eb.insertText(a[0]), null
    case "eDelChar":
      return eb.deleteChar(), null
    case "eBackspace":
      return eb.deleteCharBackward(), null
    case "eDelRange":
      return eb.deleteRange(a[0], a[1], a[2], a[3]), null
    case "eNewLine":
      return eb.newLine(), null
    case "eDelLine":
      return eb.deleteLine(), null
    case "eLeft":
      return eb.moveCursorLeft(), null
    case "eRight":
      return eb.moveCursorRight(), null
    case "eUp":
      return eb.moveCursorUp(), null
    case "eDown":
      return eb.moveCursorDown(), null
    case "eGotoLine":
      return eb.gotoLine(a[0]), null
    case "eSetCursor":
      return eb.setCursor(a[0], a[1]), null
    case "eCursorOffset":
      return eb.setCursorByOffset(a[0]), null
    case "eTabWidth":
      return eb.setTabWidth(a[0]), null
    case "eUndo":
      return eb.undo()
    case "eRedo":
      return eb.redo()
    case "eClear":
      return eb.clear(), null
    case "eClearHistory":
      return eb.clearHistory(), null
    case "eInfo": {
      const c = eb.getCursorPosition()
      return [eb.getLineCount(), eb.getTabWidth(), [c.row, c.col, c.offset], eb.canUndo(), eb.canRedo()]
    }
    case "eText":
      return eb.getText()
    case "eRange":
      return eb.getTextRange(a[0], a[1])
    case "eRangeCoords":
      return eb.getTextRangeByCoords(a[0], a[1], a[2], a[3])
    case "eNextWord":
      return logical(eb.getNextWordBoundary())
    case "ePrevWord":
      return logical(eb.getPrevWordBoundary())
    case "eEol":
      return logical(eb.getEOL())
    case "eEof":
      return logical(eb._getEOF())
    case "ePrevChar":
      return logical(eb._getPrevCharBoundary())
    case "eOffsetToPos": {
      const p = eb.offsetToPosition(a[0])
      return p ? [p.row, p.col] : null
    }
    case "ePosToOffset":
      return eb.positionToOffset(a[0], a[1])
    case "eLineStart":
      return eb.getLineStartOffset(a[0])
    case "eMeasure":
      return eb.measureText(a[0])
    case "eMeasureStatic":
      return measureEditText(a[0], { widthMethod: s.method, tabWidth: a[1] })
    case "eAddHl":
      return eb.addHighlight(a[0], hl(a[1])), null
    case "eAddHlRange":
      return eb.addHighlightByCharRange(hl(a[0])), null
    case "eRemoveRef":
      return eb.removeHighlightsByRef(a[0]), null
    case "eHl":
      return eb.getLineHighlights(a[0]).map((h: any) => [h.start, h.end, h.styleId, h.priority, h.hlRef])
    case "eDefaults":
      eb.setDefaultFg(color(a[0]) ?? null)
      eb.setDefaultBg(color(a[1]) ?? null)
      eb.setDefaultAttributes(a[2])
      return null
    case "eSyntaxStyle":
      return eb.setSyntaxStyle(a[0] ? s.style : null), null
    case "registerStyle": {
      const spec = a[1]
      return s.style.registerStyle(a[0], { ...spec, fg: color(spec.fg), bg: color(spec.bg) })
    }
    case "eEvents": {
      await Promise.resolve()
      const events = s.events
      s.events = []
      return events
    }
    case "vViewportSize":
      return view.setViewportSize(a[0], a[1]), null
    case "vViewport":
      return view.setViewport(a[0], a[1], a[2], a[3], a[4]), null
    case "vGetViewport": {
      const vp = view.getViewport()
      return [vp.offsetX, vp.offsetY, vp.width, vp.height]
    }
    case "vScrollMargin":
      return view.setScrollMargin(a[0]), null
    case "vWrapMode":
      return view.setWrapMode(a[0]), null
    case "vVcount":
      return view.getVirtualLineCount()
    case "vTotal":
      return view.getTotalVirtualLineCount()
    case "vSel":
      return view.setSelection(a[0], a[1]), null
    case "vUSel":
      return view.updateSelection(a[0]), null
    case "vRSel":
      return view.resetSelection(), null
    case "vLSel":
      return view.setLocalSelection(a[0], a[1], a[2], a[3], undefined, undefined, a[4], a[5], a[6])
    case "vULSel":
      return view.updateLocalSelection(a[0], a[1], a[2], a[3], undefined, undefined, a[4], a[5], a[6])
    case "vRLSel":
      return view.resetLocalSelection(), null
    case "vCell":
      return view.convertSelectionToCell()
    case "vOcc":
      return view.setSelectionOccupancy(a[0]), null
    case "vSelInclusive":
      return view.setSelectionInclusive(a[0], a[1]), null
    case "vSelinfo": {
      const sel = view.getSelection()
      return [sel ? [sel.start, sel.end] : null, view.getSelectedText(), view.getSelectionOccupancy()]
    }
    case "vCursor": {
      const c = view.getCursor()
      return [c.row, c.col]
    }
    case "vVisual":
      return visual(view.getVisualCursor())
    case "vUp":
      return view.moveUpVisual(), null
    case "vDown":
      return view.moveDownVisual(), null
    case "vDelSel":
      return view.deleteSelectedText(), null
    case "vReplaceSel":
      return view._replaceSelectedText(a[0])
    case "vCursorOffset":
      return view.setCursorByOffset(a[0]), null
    case "vNextWord":
      return visual(view.getNextWordBoundary())
    case "vPrevWord":
      return visual(view.getPrevWordBoundary())
    case "vEol":
      return visual(view.getEOL())
    case "vSol":
      return visual(view.getVisualSOL())
    case "vVEol":
      return visual(view.getVisualEOL())
    case "vGotoEnd":
      return view.gotoVisualLineEnd(), null
    case "vLineInfo":
      return lineInfo(view.getLineInfo())
    case "vLogical":
      return lineInfo(view.getLogicalLineInfo())
    case "vPlaceholder":
      return view.setPlaceholderStyledText(a[0].map((c: any) => ({ text: c.text, fg: color(c.fg), bg: color(c.bg), attributes: c.attributes }))), null
    case "vTabIndicator":
      return view.setTabIndicator(a[0]), null
    case "vMeasure": {
      const m = view.measureForDimensions(a[0], a[1])
      return m ? [m.lineCount, m.widthColsMax] : null
    }
    case "vDraw":
      return draw(s.method, (buffer) => buffer.drawEditorView(view, a[2], a[3]), a[0], a[1])
    case "xCreate": {
      const o = a[0]
      return view.extmarks.create({
        start: o.start,
        end: o.end,
        virtual: o.virtual,
        styleId: o.styleId,
        priority: o.priority,
        typeId: o.typeId,
        data: o.data,
        metadata: o.meta,
      })
    }
    case "xMeta":
      return view.extmarks.getMetadataFor(a[0]) ?? null
    case "xData":
      return view.extmarks.get(a[0])?.data ?? null
    case "xAdjustDel":
      return view.extmarks.adjustExtmarksAfterDeletion(a[0], a[1], a[2]), null
    case "xDelete":
      return view.extmarks.delete(a[0])
    case "xGet": {
      const e = view.extmarks.get(a[0])
      return e ? mark(e) : null
    }
    case "xAll":
      return view.extmarks.getAll().map(mark)
    case "xVirtual":
      return view.extmarks.getVirtual().map(mark)
    case "xAtOffset":
      return view.extmarks.getAtOffset(a[0]).map(mark)
    case "xForType":
      return view.extmarks.getAllForTypeId(a[0]).map(mark)
    case "xClear":
      return view.extmarks.clear(), null
    case "xRegisterType":
      return view.extmarks.registerType(a[0])
    case "xTypeId":
      return view.extmarks.getTypeId(a[0])
    case "xTypeName":
      return view.extmarks.getTypeName(a[0])
    case "xDestroy":
      return view.extmarks.destroy(), null
    case "vNewView":
      s.view.destroy()
      s.view = EditorView.create(eb, a[0], a[1])
      return null
  }
  throw new Error(`unknown op ${name}`)
}

export async function runEditCase(c: Case): Promise<unknown[]> {
  context ??= new ResourceContext({ objectCapacity: 1 << 16, renderCellsMax: 1 << 22 })
  const eb = EditBuffer.create(c.m, context)
  const state: EditState = { eb, view: EditorView.create(eb, c.w, c.h), style: SyntaxStyle.create(context), method: c.m, events: [] }
  for (const name of ["cursor-changed", "content-changed", "cursorChanged"]) eb.on(name, () => state.events.push(name))
  const results: unknown[] = []
  try {
    for (const op of c.ops) {
      try {
        results.push(await editOp(state, op))
      } catch (error) {
        results.push(fail(error))
      }
    }
  } finally {
    await Promise.resolve()
    state.view.destroy()
    eb.destroy()
    state.style.destroy()
  }
  return results
}

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
