// Frames: a test renderer created with the TUI's renderer options (app.tsx), set up against Ghostty's terminal
// core (port/conformance/lib/terminal.ts, as the conformance driver does), draws op lists (ops.ts) through a
// renderable's renderSelf; the bytes each frame writes are recorded. The script it writes for the Rust side
// replays the terminal's replies to the setup, then the same frames.
// usage: bun frames.ts <script out> <records out>
import { writeFileSync } from "node:fs"
import path from "node:path"
import { Writable } from "node:stream"
import { CONFORMANCE, core, testing } from "./core"
import { color, draw, encodeColor, hex } from "./ops"

const { VirtualTerminal } = await import(path.join(CONFORMANCE, "lib/terminal.ts"))
const { Renderable, RGBA, CliRenderEvents } = core

let seed = 0x1234567
function random() {
  seed ^= seed << 13
  seed >>>= 0
  seed ^= seed >>> 17
  seed ^= seed << 5
  seed >>>= 0
  return seed / 0x100000000
}
const int = (lo: number, hi: number) => lo + Math.floor(random() * (hi - lo + 1))
const pick = <T>(items: readonly T[]) => items[int(0, items.length - 1)]!

const COLORS = [
  RGBA.fromInts(255, 255, 255),
  RGBA.fromInts(10, 10, 10),
  RGBA.fromInts(200, 30, 90),
  RGBA.fromHex("#3a6ff0"),
  RGBA.fromHex("#808080"),
  RGBA.fromIndex(2),
  RGBA.fromIndex(244),
  RGBA.defaultForeground(),
  RGBA.fromInts(0, 0, 0, 0),
  RGBA.fromInts(255, 0, 0, 128),
].map(encodeColor)
const TEXTS = ["Hello", "a b c", "─│┃", "█▀▄", "中文", "😀", "éx", "x\ty", "…→", "Ω", "🇫🇷"]

function randomOp(width: number, height: number) {
  const x = () => int(-2, width)
  const y = () => int(-1, height)
  const r = random()
  if (r < 0.4) return `text ${hex(pick(TEXTS))} ${x()} ${y()} ${pick(COLORS)} ${random() < 0.3 ? "-" : pick(COLORS)} ${pick([0, 1, 2, 4, 8, 32, 128, 0, 0])}`
  if (r < 0.6) return `fill ${x()} ${y()} ${int(1, width)} ${int(1, height)} ${pick(COLORS)}`
  if (r < 0.7) return `cell ${int(0, width - 1)} ${int(0, height - 1)} ${pick([0x41, 0x2588, 0x4e2d, 0x20])} ${pick(COLORS)} ${pick(COLORS)} ${pick([0, 1, 9])}`
  if (r < 0.85) {
    return `box ${x()} ${y()} ${int(2, width)} ${int(2, height)} ${pick(["single", "rounded", "heavy", "double"])} ${int(0, 15)} ${pick(COLORS)} ${pick(COLORS)} ${int(0, 1)} ${random() < 0.5 ? hex("Title") : "-"} - ${int(0, 2)} - 0`
  }
  return `fit ${hex(pick(TEXTS))} ${x()} ${y()} ${int(1, 6)} ${pick(COLORS)} - ${int(0, 1)}`
}

class Ops extends Renderable {
  ops: string[][] = []
  protected override renderSelf(buffer: unknown) {
    for (const op of this.ops) draw(buffer, op)
  }
}

const cases: { name: string; width: number; height: number; frames: string[][] }[] = []
// Hand-picked: an unchanged frame, attribute runs, wide glyphs over earlier ones, a cleared screen.
cases.push({
  name: "basic",
  width: 30,
  height: 6,
  frames: [
    [`text ${hex("Hello world")} 1 1 ${COLORS[0]} - 0`],
    [`text ${hex("Hello world")} 1 1 ${COLORS[0]} - 0`],
    [`text ${hex("Hello")} 1 1 ${COLORS[0]} - 1`, `text ${hex("wörld")} 7 1 ${COLORS[2]} ${COLORS[1]} 4`],
    [`text ${hex("中文😀")} 0 2 ${COLORS[3]} - 0`, `fill 10 3 5 2 ${COLORS[5]}`],
    [`text ${hex("x")} 1 2 ${COLORS[3]} - 0`],
    [],
  ],
})
// The cursor and the pointer: shown, moved, restyled, recolored, hidden; a frame with only a cursor change.
cases.push({
  name: "cursor",
  width: 20,
  height: 4,
  frames: [
    [`text ${hex("abc")} 0 0 ${COLORS[0]} - 0`, "cursor 3 1 1 line 0 -"],
    ["cursor 5 2 1 - - -"],
    [`cursor 5 2 1 block 1 ${COLORS[2]}`],
    [`cursor 5 2 1 block 1 ${COLORS[2]}`, "pointer pointer"],
    [`text ${hex("x")} 4 1 ${COLORS[2]} - 0`, "cursor 1 1 0 - - -"],
    ["pointer default"],
    ["cursor 2 2 1 underline 0 -"],
  ],
})
// Resizes between frames repaint everything (`resize W H` as a frame of its own: no frame is recorded for it).
cases.push({
  name: "resize",
  width: 20,
  height: 5,
  frames: [
    [`text ${hex("abc中")} 0 0 ${COLORS[0]} - 0`, `fill 0 2 20 2 ${COLORS[5]}`],
    ["resize 12 3"],
    [`text ${hex("abc中")} 0 0 ${COLORS[0]} - 0`, `fill 0 2 20 2 ${COLORS[5]}`],
    ["resize 30 6"],
    [`text ${hex("xyz")} 5 5 ${COLORS[2]} - 1`],
  ],
})
for (let c = 0; c < 40; c++) {
  const width = int(8, 40)
  const height = int(2, 8)
  const frames: string[][] = []
  for (let f = int(1, 5); f > 0; f--) {
    const ops: string[] = []
    for (let i = int(0, 8); i > 0; i--) ops.push(randomOp(width, height))
    if (random() < 0.3) {
      const style = pick(["-", "block", "line", "underline", "default"])
      const cursorColor = random() < 0.5 ? "-" : pick(COLORS)
      ops.push(`cursor ${int(0, width + 1)} ${int(0, height + 1)} ${int(0, 1)} ${style} ${pick(["-", "0", "1"])} ${cursorColor}`)
    }
    if (random() < 0.15) ops.push(`pointer ${pick(["pointer", "text", "default", "crosshair"])}`)
    frames.push(ops)
  }
  cases.push({ name: `r${c}`, width, height, frames })
}

const script: string[] = []
const out: string[] = []
// The TUI's renderer runs in the conformance terminal with TERM=xterm-256color and nothing else forwarded.
for (const key of Object.keys(process.env)) if (key !== "PATH" && key !== "HOME") delete process.env[key]
process.env.TERM = "xterm-256color"

for (const scenario of cases) {
  const replies: string[] = []
  let deliver = (_: string) => {}
  const terminal = await VirtualTerminal.create(scenario.width, scenario.height, (response: string) =>
    setImmediate(() => deliver(response)),
  )
  const written: Buffer[] = []
  const stdout = Object.assign(
    new Writable({
      write(chunk, _encoding, callback) {
        written.push(Buffer.from(chunk))
        terminal.write(chunk)
        callback()
      },
    }),
    { columns: scenario.width, rows: scenario.height, isTTY: true },
  )
  const setup = await testing.createTestRenderer({
    width: scenario.width,
    height: scenario.height,
    stdout,
    bufferedOutput: "stdout",
    screenMode: "alternate-screen",
    externalOutputMode: "passthrough",
    targetFps: 60,
    gatherStats: false,
    exitOnCtrlC: false,
    useKittyKeyboard: {},
    autoFocus: false,
    openConsoleOnError: false,
    remote: false,
  })
  const renderer = setup.renderer
  // As a real TUI's stdin, a terminal: palette detection only queries a TTY.
  Object.assign(renderer.stdin, { isTTY: true })
  deliver = (data: string) => {
    replies.push(data)
    renderer.stdin.emit("data", Buffer.from(data))
  }
  await renderer.setupTerminal()
  const ops = new Ops(renderer, { id: "ops", position: "absolute", left: 0, top: 0, width: "100%", height: "100%" })
  renderer.root.add(ops)
  // Every reply arrives and the palette probe times out (300 ms) before frames are recorded.
  await Bun.sleep(1200)
  await setup.renderOnce()
  await Bun.sleep(20)
  script.push(`case ${scenario.name} ${scenario.width} ${scenario.height}`)
  for (const reply of replies) script.push(`reply ${hex(reply)}`)
  script.push("settle")
  for (const frame of scenario.frames) {
    const lines = frame.map((line) => line.split(" "))
    if (lines[0]?.[0] === "resize") {
      // The renderer repaints on its own after a resize, with the ops it has.
      const [width, height] = [Number(lines[0][1]), Number(lines[0][2])]
      written.length = 0
      terminal.resize(width, height)
      Object.assign(stdout, { columns: width, rows: height })
      setup.resize(width, height)
      await Bun.sleep(50)
      out.push(`frame ${scenario.name} ${Buffer.concat(written).toString("hex")}`)
      script.push(frame[0], "frame", ...ops.ops.map((op: string[]) => op.join(" ")), "endframe")
      continue
    }
    ops.ops = lines.filter((op) => op[0] !== "cursor" && op[0] !== "pointer")
    for (const op of lines) {
      if (op[0] === "pointer") renderer.setMousePointer(op[1])
      if (op[0] !== "cursor") continue
      const style: Record<string, unknown> = {}
      if (op[4] !== "-") style.style = op[4]
      if (op[5] !== "-") style.blinking = op[5] === "1"
      if (op[6] !== "-") style.color = color(op[6])
      if (Object.keys(style).length) renderer.setCursorStyle(style)
      renderer.setCursorPosition(Number(op[1]), Number(op[2]), op[3] === "1")
    }
    ops.requestRender()
    written.length = 0
    const presented = new Promise((resolve) => renderer.once(CliRenderEvents.FRAME, resolve))
    await setup.renderOnce()
    await presented
    await Bun.sleep(5)
    out.push(`frame ${scenario.name} ${Buffer.concat(written).toString("hex")}`)
    script.push("frame", ...frame, "endframe")
  }
  await setup.dispose()
  terminal.free()
}
writeFileSync(process.argv[2], script.join("\n") + "\n")
writeFileSync(process.argv[3], out.join("\n") + "\n")
