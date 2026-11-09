// The TS side of the scene Text check: a `TextRenderable` (its text lives in a native scene Text node) in a test
// renderer, driven through its setters and the scene's text calls, answered as examples/text-driver/scene.rs does.
import { join } from "node:path"
import { runCase, runEditCase, type Case, type Op } from "./ts-exec.ts"

const reference = process.env.OPENCODE_REFERENCE ?? join(import.meta.dir, "../../../../.reference")
const pkg = join(reference, "packages/tui/node_modules/@opentui/core")
const core = await import(join(pkg, "index.bun.js"))
const testing = await import(join(pkg, "testing.bun.js"))
const { TextRenderable, RGBA, StyledText, OptimizedBuffer, NativeError, NativeStatus } = core

const color = (c: number[] | null | undefined) => (c ? RGBA.fromInts(c[0], c[1], c[2], c[3]) : undefined)
const lineInfo = (i: any) => [i.lineStartCols, i.lineWidthCols, i.lineSources, i.lineWraps, i.lineWidthColsMax]

function fail(error: unknown) {
  if (error instanceof NativeError) return { err: NativeStatus[(error as any).status] }
  return { err: `js:${(error as Error).message}` }
}

function content(c: any) {
  if (typeof c === "string") return c
  return new StyledText(
    c.map((chunk: any) => ({
      __isChunk: true,
      text: chunk.text,
      fg: color(chunk.fg),
      bg: color(chunk.bg),
      attributes: chunk.attributes,
      link: chunk.link === undefined ? undefined : { url: chunk.link },
    })),
  )
}

function props(p: any) {
  const out: any = { ...p, position: "absolute" }
  for (const key of ["fg", "bg", "selectionBg", "selectionFg", "tabIndicatorColor"]) if (key in p) out[key] = color(p[key])
  if ("content" in p) out.content = content(p.content)
  return out
}

let setup: any

/** The test renderer's width method (scene cases run under it on both sides). */
export async function sceneWidthMethod(): Promise<string> {
  setup ??= await testing.createTestRenderer({ width: 40, height: 12 })
  return setup.renderer.widthMethod
}

/** Disposes of the test renderer. */
export async function disposeScene() {
  if (setup) await setup.dispose()
  setup = undefined
}

/** Runs a scene case (`sw` by `sh` is the size of the buffers `sDraw` paints into). */
export async function runSceneCase(c: Case): Promise<unknown[]> {
  setup ??= await testing.createTestRenderer({ width: 40, height: 12 })
  const { renderer } = setup
  const method = renderer.widthMethod
  const results: unknown[] = []
  let text: any
  try {
    text = new TextRenderable(renderer, props(c.props))
    renderer.root.add(text)
    for (const op of c.ops) {
      try {
        results.push(await sceneOp(renderer, method, text, op, c))
      } catch (error) {
        results.push(fail(error))
      }
    }
  } finally {
    if (text) {
      renderer.root.remove(text)
      text.destroy()
    }
    await setup.renderOnce()
  }
  return results
}

async function sceneOp(renderer: any, method: string, text: any, op: Op, c: Case): Promise<unknown> {
  const [name, ...a] = op
  switch (name) {
    case "sRender":
      return await setup.renderOnce(), null
    case "sDraw": {
      const buffer = OptimizedBuffer.create(c.sw, c.sh, method, { owner: renderer.nativeScene })
      try {
        buffer.clear(RGBA.fromInts(0, 0, 0, 255))
        text.drawToBuffer(buffer, a[0], a[1])
        return buffer
          .getSpanLines()
          .map((line: any) => line.spans.map((s: any) => [s.text, [...s.fg.buffer], [...s.bg.buffer], s.attributes, s.width]))
      } finally {
        buffer.destroy()
      }
    }
    case "sSet": {
      const value = a[0] === "content" ? content(a[1]) : ["fg", "bg", "selectionBg", "selectionFg", "tabIndicatorColor"].includes(a[0]) ? color(a[1]) : a[1]
      text[a[0]] = value
      return null
    }
    case "sScroll":
      text[a[0]] = a[1]
      return [text.scrollX, text.scrollY]
    case "sSel":
      return renderer.nativeScene.setTextSelection(
        text,
        a[0],
        { anchorX: a[1], anchorY: a[2], focusX: a[3], focusY: a[4], behavior: a[5] ?? "cell" },
        color(a[6]),
        color(a[7]),
      )
    case "sSelection": {
      const s = text.getSelection()
      return s ? [s.start, s.end] : null
    }
    case "sSelected":
      return text.getSelectedText()
    case "sInfo":
      return [text.lineCount, text.virtualLineCount, text.scrollWidth, text.scrollHeight, text.textLength, text.plainText]
    case "sLayout":
      return [text.x, text.y, text.width, text.height]
    case "sLineInfo":
      return lineInfo(text.lineInfo)
    case "sLineSources":
      return text.getLineSources(a[0], a[1])
  }
  throw new Error(`unknown op ${name}`)
}


/** Runs any case on the TS side. */
export async function runAnyCase(c: Case): Promise<unknown[]> {
  return c.k === "edit" ? runEditCase(c) : c.k === "scene" ? runSceneCase(c) : runCase(c)
}
