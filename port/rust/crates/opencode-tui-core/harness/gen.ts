// Buffer op scripts (see ops.ts): hand-picked cases, then random ones from a fixed seed.
// usage: bun gen.ts <count> > cases.txt
import { core } from "./core"
import { encodeColor, hex } from "./ops"

const { RGBA } = core
let seed = 0x9e3779b9
function random() {
  // xorshift32
  seed ^= seed << 13
  seed >>>= 0
  seed ^= seed >>> 17
  seed ^= seed << 5
  seed >>>= 0
  return seed / 0x100000000
}
const int = (lo: number, hi: number) => lo + Math.floor(random() * (hi - lo + 1))
const pick = <T>(items: readonly T[]) => items[int(0, items.length - 1)]!

const SOLID = [
  RGBA.fromInts(255, 255, 255),
  RGBA.fromInts(0, 0, 0),
  RGBA.fromInts(10, 10, 10),
  RGBA.fromInts(200, 30, 90),
  RGBA.fromHex("#3a6ff0"),
  RGBA.fromIndex(15),
  RGBA.fromIndex(232),
  RGBA.fromIndex(4),
  RGBA.defaultForeground(),
  RGBA.defaultBackground(),
]
const TRANSLUCENT = [
  RGBA.fromInts(0, 0, 0, 0),
  RGBA.fromInts(255, 0, 0, 128),
  RGBA.fromInts(20, 200, 40, 64),
  RGBA.fromInts(255, 255, 255, 1),
  RGBA.fromInts(0, 0, 255, 254),
  RGBA.fromValues(0.5, 0.5, 0.5, 0.5),
]
const anyColor = () => encodeColor(random() < 0.6 ? pick(SOLID) : pick(TRANSLUCENT))
const maybeColor = () => (random() < 0.3 ? "-" : anyColor())

const PIECES = [
  "a", "b", "Hello", " ", "  ", "x y", "é", "ñ", "─", "│", "┃", "█", "▀", "▄", "╹", "中", "文字", "😀", "🔥",
  "é", "\t", "\x01", "~", "…", "Ω", "→", "✓", "🇫🇷", "ｱ", "Ａ",
]
const randomText = () => {
  const count = int(0, 6)
  let out = ""
  for (let i = 0; i < count; i++) out += pick(PIECES)
  return out
}
const CODE_POINTS = [0x41, 0x20, 0x2588, 0x2500, 0x4e2d, 0x1f600, 0xe9, 0x7f, 0x1, 0x2502, 0x2026, 0x30]
const STYLES = ["single", "double", "rounded", "heavy", "9484,9488,9492,9496,9472,9474,9516,9524,9500,9508,9532", "43,43,43,43,45,124,43,43,43,43,43"]

function randomOp(width: number, height: number, buffers: number): string {
  const x = () => int(-3, width + 2)
  const y = () => int(-2, height + 1)
  const r = random()
  if (r < 0.06) return `clear ${encodeColor(pick(SOLID))}`
  if (r < 0.14) return `${pick(["cell", "cellblend", "char"])} ${int(0, width)} ${int(0, height)} ${pick(CODE_POINTS)} ${anyColor()} ${anyColor()} ${int(0, 255)}`
  if (r < 0.36) return `text ${hex(randomText()) || "61"} ${x()} ${y()} ${anyColor()} ${maybeColor()} ${int(0, 255)}`
  if (r < 0.4) {
    const t = randomText() || "abc"
    return `seltext ${hex(t)} ${x()} ${y()} ${anyColor()} ${maybeColor()} ${int(0, 3)} ${int(0, 4)} ${int(0, 6)} ${maybeColor()} ${maybeColor()}`
  }
  if (r < 0.46) return `fit ${hex(randomText()) || "61"} ${x()} ${y()} ${int(0, 8)} ${anyColor()} ${maybeColor()} ${int(0, 1)}`
  if (r < 0.58) return `fill ${x()} ${y()} ${int(-1, width)} ${int(-1, height)} ${anyColor()}`
  if (r < 0.7) {
    const title = random() < 0.5 ? "-" : hex(pick(["T", "Title", "中文", "a b", "😀x", ""]) ) || "-"
    const bottom = random() < 0.7 ? "-" : hex(pick(["B", "bottom"]))
    return `box ${x()} ${y()} ${int(-1, width)} ${int(-1, height)} ${pick(STYLES)} ${int(0, 15)} ${anyColor()} ${anyColor()} ${int(0, 1)} ${title} ${maybeColor()} ${int(0, 2)} ${bottom} ${int(0, 2)}`
  }
  if (r < 0.76) return `pscissor ${x()} ${y()} ${int(-1, width)} ${int(-1, height)}`
  if (r < 0.79) return "popscissor"
  if (r < 0.8) return "clrscissor"
  if (r < 0.85) return `popacity ${pick([0, 0.25, 0.5, 0.7, 1, 0.999])}`
  if (r < 0.88) return "popopacity"
  if (r < 0.89) return "clropacity"
  if (r < 0.91) return `respect ${int(0, 1)}`
  if (r < 0.93 && buffers > 1) {
    const opt = (hi: number) => (random() < 0.5 ? "-" : String(int(0, hi)))
    return `fb ${int(0, buffers - 2)} ${x()} ${y()} ${opt(4)} ${opt(3)} ${opt(width)} ${opt(height)}`
  }
  if (r < 0.94) return `resize ${int(1, 30)} ${int(1, 8)}`
  return `text ${hex(randomText()) || "62"} ${x()} ${y()} ${anyColor()} - 0`
}

const lines: string[] = []
const white = encodeColor(RGBA.fromInts(255, 255, 255))
const black = encodeColor(RGBA.fromInts(0, 0, 0))
const half = encodeColor(RGBA.fromInts(255, 0, 0, 128))
const none = encodeColor(RGBA.fromInts(0, 0, 0, 0))
// Hand-picked.
lines.push(
  "case wide", "buffer 12 3 0", `clear ${black}`,
  `text ${hex("a中b😀c")} 0 0 ${white} - 0`, `text ${hex("x")} 2 0 ${white} - 0`, `text ${hex("中")} 11 1 ${white} - 0`,
  `text ${hex("文字")} -1 2 ${white} ${black} 1`, "dump",
  "case blend", "buffer 8 2 0", `clear ${black}`, `text ${hex("Hello")} 0 0 ${white} - 0`, `fill 0 0 8 2 ${half}`,
  `cellblend 6 1 65 ${half} ${half} 0`, "popacity 0.5", `fill 2 1 3 1 ${white}`, "dump",
  "case boxes", "buffer 20 6 0", `clear ${black}`,
  `box 0 0 20 3 rounded 15 ${white} ${black} 1 ${hex("Title")} - 1 ${hex("end")} 2`,
  `box 2 3 6 3 heavy 9 ${white} ${none} 0 - - 0 - 0`, `box 10 3 4 3 single 1 ${white} ${none} 0 - - 0 - 0`, "dump",
  "case scissor", "buffer 10 4 0", `clear ${black}`, "pscissor 2 1 5 2", `fill 0 0 10 4 ${white}`,
  `text ${hex("中中中中中")} 0 1 ${black} - 0`, "pscissor 3 0 10 10", `text ${hex("zzzzzzzzzz")} 0 2 ${black} - 0`, "dump",
  "case fit", "buffer 12 3 0", `clear ${black}`, `fit ${hex("Hello world")} 0 0 5 ${white} - 1`,
  `fit ${hex("中文字体")} 0 1 5 ${white} - 1`, `fit ${hex("ab")} 0 2 5 ${white} - 1`, "dump",
  "case frames", "buffer 6 3 1", `clear ${half}`, `text ${hex("ab中")} 0 0 ${white} - 0`, "buffer 10 4 0", `clear ${black}`,
  `text ${hex("0123456789")} 0 1 ${white} - 0`, "fb 0 2 0 - - - -", "fb 0 -1 2 1 0 4 2", "dump",
)
const count = Number(process.argv[2] ?? 300)
for (let c = 0; c < count; c++) {
  lines.push(`case r${c}`)
  const buffers = int(1, 2)
  let width = 0
  let height = 0
  for (let b = 0; b < buffers; b++) {
    width = int(1, 24)
    height = int(1, 6)
    lines.push(`buffer ${width} ${height} ${int(0, 1)}`)
    if (random() < 0.8) lines.push(`clear ${encodeColor(pick(SOLID))}`)
    for (let i = int(0, 6); i > 0; i--) lines.push(randomOp(width, height, b))
  }
  for (let i = int(1, 30); i > 0; i--) {
    const op = randomOp(width, height, buffers)
    lines.push(op)
    if (op.startsWith("resize")) {
      const [, w, h] = op.split(" ")
      width = Number(w)
      height = Number(h)
    }
  }
  lines.push("dump")
}
process.stdout.write(lines.join("\n") + "\n")
