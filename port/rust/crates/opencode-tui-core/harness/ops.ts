// The buffer op script both sides run (the Rust side is examples/harness.rs), one op per line:
//   case NAME | buffer W H RESPECT | target I | clear C | cell|cellblend|char X Y CP FG BG ATTR
//   text HEX X Y FG BG ATTR | seltext HEX X Y FG BG ATTR START END SELBG SELFG | fit HEX X Y MAXW FG BG ELLIPSIS
//   fill X Y W H C | box X Y W H STYLE SIDES BORDER BG FILL TITLE TITLECOLOR TALIGN BTITLE BALIGN
//   pscissor X Y W H | popscissor | clrscissor | popacity F | popopacity | clropacity | resize W H | respect 0|1
//   fb SRC DX DY SX SY SW SH | dump
// A color is `#` and the four u16 of the packed RGBA in hex; `-` is "not given" (bg, title, selection colors,
// frame buffer source rect). Text is UTF-8 in hex. STYLE is a style name or 11 comma-separated code points.
// SIDES is the native bit set (left 1, bottom 2, right 4, top 8).
import { core } from "./core"

const { OptimizedBuffer, RGBA, ResourceContext } = core

export function color(token: string) {
  if (token === "-") return undefined
  const hex = token.slice(1)
  const parts = new Uint16Array(4)
  for (let i = 0; i < 4; i++) parts[i] = parseInt(hex.slice(i * 4, i * 4 + 4), 16)
  return RGBA.fromArray(parts)
}

export function encodeColor(rgba: { buffer: Uint16Array }) {
  return "#" + [...rgba.buffer].map((v) => v.toString(16).padStart(4, "0")).join("")
}

export function text(token: string) {
  return Buffer.from(token, "hex").toString("utf8")
}

export function hex(value: string) {
  return Buffer.from(value, "utf8").toString("hex")
}

const STYLES = ["single", "double", "rounded", "heavy"]

function sides(bits: number) {
  const out: string[] = []
  if (bits & 8) out.push("top")
  if (bits & 4) out.push("right")
  if (bits & 2) out.push("bottom")
  if (bits & 1) out.push("left")
  return out
}

const ALIGN = ["left", "center", "right"]

/** Draws one op on a TS buffer-like target (an OptimizedBuffer or a recording frame buffer). */
export function draw(target: any, op: string[], buffers: any[] = []) {
  const n = (i: number) => Number(op[i])
  switch (op[0]) {
    case "clear":
      return target.clear(color(op[1]))
    case "cell":
      return target.setCell(n(1), n(2), String.fromCodePoint(n(3)), color(op[4]), color(op[5]), n(6))
    case "cellblend":
      return target.setCellWithAlphaBlending(n(1), n(2), String.fromCodePoint(n(3)), color(op[4]), color(op[5]), n(6))
    case "char":
      return target.drawChar(n(3), n(1), n(2), color(op[4]), color(op[5]), n(6))
    case "text":
      return target.drawText(text(op[1]), n(2), n(3), color(op[4]), color(op[5]), n(6))
    case "seltext":
      return target.drawText(text(op[1]), n(2), n(3), color(op[4]), color(op[5]), n(6), {
        start: n(7),
        end: n(8),
        bgColor: color(op[9]),
        fgColor: color(op[10]),
      })
    case "fit":
      return target.drawTextFit(text(op[1]), n(2), n(3), n(4), color(op[5]), color(op[6]), op[7] === "1")
    case "fill":
      return target.fillRect(n(1), n(2), n(3), n(4), color(op[5]))
    case "box": {
      const style = op[5]
      const custom = STYLES.includes(style) ? undefined : new Uint32Array(style.split(",").map(Number))
      return target.drawBox({
        x: n(1),
        y: n(2),
        width: n(3),
        height: n(4),
        borderStyle: custom ? undefined : style,
        customBorderChars: custom,
        border: sides(n(6)),
        borderColor: color(op[7]),
        backgroundColor: color(op[8]),
        shouldFill: op[9] === "1",
        title: op[10] === "-" ? undefined : text(op[10]),
        titleColor: color(op[11]),
        titleAlignment: ALIGN[n(12)],
        bottomTitle: op[13] === "-" ? undefined : text(op[13]),
        bottomTitleAlignment: ALIGN[n(14)],
      })
    }
    case "pscissor":
      return target.pushScissorRect(n(1), n(2), n(3), n(4))
    case "popscissor":
      return target.popScissorRect()
    case "clrscissor":
      return target.clearScissorRects()
    case "popacity":
      return target.pushOpacity(n(1))
    case "popopacity":
      return target.popOpacity()
    case "clropacity":
      return target.clearOpacity()
    case "resize":
      return target.resize(n(1), n(2))
    case "respect":
      return target.setRespectAlpha(op[1] === "1")
    case "fb": {
      const opt = (i: number) => (op[i] === "-" ? undefined : n(i))
      return target.drawFrameBuffer(n(2), n(3), buffers[n(1)], opt(4), opt(5), opt(6), opt(7))
    }
    default:
      throw new Error(`unknown op ${op[0]}`)
  }
}

/** The cells of a buffer, as the Rust side dumps them. */
export function dump(buffer: any) {
  return buffer["withResolvedChars"](
    { addLineBreaks: false, cellLengths: true },
    (bytes: Uint8Array, cells: any, lengths: Uint8Array) => {
      const rows: string[] = []
      let offset = 0
      for (let y = 0; y < cells.height; y++) {
        const row: string[] = []
        for (let x = 0; x < cells.width; x++) {
          const i = y * cells.width + x
          const char = cells.char[i] >>> 0
          const end = offset + lengths[i]
          const cellBytes = Buffer.from(bytes.subarray(offset, end))
          offset = end
          const type = (char & 0xc0000000) >>> 0
          const token =
            type === 0x80000000
              ? `g${((char >>> 26) & 0xf) + 1}_${cellBytes.toString("hex")}`
              : type === 0xc0000000
                ? `c${(char >>> 26) & 0xf}`
                : `u${char.toString(16)}`
          const fg = [...cells.fg.subarray(i * 4, i * 4 + 4)].map((v: number) => v.toString(16).padStart(4, "0")).join("")
          const bg = [...cells.bg.subarray(i * 4, i * 4 + 4)].map((v: number) => v.toString(16).padStart(4, "0")).join("")
          row.push(`${token}:${fg}:${bg}:${(cells.attributes[i] >>> 0).toString(16)}`)
        }
        rows.push(row.join(" "))
      }
      return rows
    },
  )
}

/** Runs a script; returns the dumps. */
export function run(script: string) {
  const out: string[] = []
  const context = new ResourceContext({ objectCapacity: 64, renderCellsMax: 1 << 20 })
  let buffers: any[] = []
  let target: any
  let name = ""
  const finish = () => {
    for (const buffer of buffers) buffer.destroy()
    buffers = []
  }
  for (const line of script.split("\n")) {
    if (!line.trim()) continue
    const op = line.split(" ")
    try {
      if (op[0] === "case") {
        finish()
        name = op[1]
        continue
      }
      if (op[0] === "buffer") {
        target = OptimizedBuffer.create(Number(op[1]), Number(op[2]), "unicode", {
          owner: context,
          respectAlpha: op[3] === "1",
        })
        buffers.push(target)
        continue
      }
      if (op[0] === "target") {
        target = buffers[Number(op[1])]
        continue
      }
      if (op[0] === "dump") {
        out.push(`dump ${name} ${target.width} ${target.height}`, ...dump(target))
        continue
      }
      draw(target, op, buffers)
    } catch (error) {
      // A draw the native API rejects changes nothing; both sides note it.
      out.push(`error ${name} ${op[0]}`)
    }
  }
  finish()
  context.destroy()
  return out
}

if (import.meta.main) {
  const script = await Bun.file(process.argv[2]).text()
  process.stdout.write(run(script).join("\n") + "\n")
}
