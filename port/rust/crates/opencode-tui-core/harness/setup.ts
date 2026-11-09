// Terminal setup and teardown: a test renderer created with the TUI's renderer options (app.tsx) sets up
// against Ghostty's terminal core; the terminal's replies reach it once setupTerminal has resolved (as through
// a pseudo-terminal, where they come after the setup's writes). Everything written except frames (the
// synchronized-output blocks) is recorded, up to the end of teardown; the log it writes for the Rust side has
// the replies and the waits between them.
// usage: bun setup.ts <log out> <records out>
import { writeFileSync } from "node:fs"
import path from "node:path"
import { Writable } from "node:stream"
import { CONFORMANCE, testing } from "./core"
import { hex } from "./ops"

const { VirtualTerminal } = await import(path.join(CONFORMANCE, "lib/terminal.ts"))

const cases = [
  { name: "default", width: 80, height: 24, env: { TERM: "xterm-256color" }, mouse: true },
  { name: "small", width: 20, height: 5, env: { TERM: "xterm-256color" }, mouse: true },
  { name: "truecolor", width: 80, height: 24, env: { TERM: "xterm-256color", COLORTERM: "truecolor" }, mouse: true },
  { name: "nomouse", width: 60, height: 20, env: { TERM: "xterm-256color" }, mouse: false },
  { name: "plain", width: 40, height: 10, env: { TERM: "xterm" }, mouse: true },
]

function withoutFrames(text: string) {
  let out = ""
  let at = 0
  for (;;) {
    const start = text.indexOf("\x1b[?2026h", at)
    if (start < 0) return out + text.slice(at)
    out += text.slice(at, start)
    const end = text.indexOf("\x1b[?2026l", start)
    if (end < 0) return out
    at = end + "\x1b[?2026l".length
  }
}

const log: string[] = []
const out: string[] = []
for (const scenario of cases) {
  for (const key of Object.keys(process.env)) if (key !== "PATH" && key !== "HOME") delete process.env[key]
  Object.assign(process.env, scenario.env)
  let ready = false
  const queued: string[] = []
  let deliver = (_: string) => {}
  const terminal = await VirtualTerminal.create(scenario.width, scenario.height, (response: string) =>
    setImmediate(() => (ready ? deliver(response) : queued.push(response))),
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
    useMouse: scenario.mouse,
    remote: false,
  })
  const renderer = setup.renderer
  // As a real TUI's stdin, a terminal: palette detection only queries a TTY.
  Object.assign(renderer.stdin, { isTTY: true })
  const env = Object.entries(scenario.env)
    .map(([key, value]) => `${key}=${value}`)
    .join(",")
  log.push(`case ${scenario.name} ${scenario.width} ${scenario.height} ${scenario.mouse ? 1 : 0} ${env}`)
  let last = performance.now()
  const wait = () => {
    const now = performance.now()
    log.push(`wait ${Math.round(now - last)}`)
    last = now
  }
  deliver = (data: string) => {
    wait()
    log.push(`reply ${hex(data)}`)
    renderer.stdin.emit("data", Buffer.from(data))
  }
  await renderer.setupTerminal()
  last = performance.now()
  ready = true
  for (const response of queued.splice(0)) deliver(response)
  await Bun.sleep(1500)
  wait()
  log.push("close")
  renderer.destroy()
  await renderer.closed.catch(() => {})
  log.push("end")
  out.push(`setup ${scenario.name} ${Buffer.from(withoutFrames(Buffer.concat(written).toString("latin1")), "latin1").toString("hex")}`)
  terminal.free()
}
writeFileSync(process.argv[2], log.join("\n") + "\n")
writeFileSync(process.argv[3], out.join("\n") + "\n")
