// Cases of solid/pty.ts: terminal socket messages decoded by the schema (valid ones and every way to break them),
// input frames, and the connect URL after a ticket.
import { Random } from "./random"

const MESSAGES = [
  { type: "attached", info: { size: { cols: 80, rows: 24 } } },
  { type: "attached", inputProtocol: 1, info: { size: { cols: 120, rows: 40 }, extra: 1 }, role: "controller", other: true },
  { type: "attached", info: { size: { cols: 80, rows: 24 } }, role: "observer" },
  { type: "replay_complete" },
  { type: "resized", cols: 100, rows: 30, checkpoint: "cp_1" },
  { type: "controller_changed" },
  { type: "controller_changed", attachmentID: null },
  { type: "controller_changed", attachmentID: "att_1" },
  { type: "exited" },
  { type: "exited", exitCode: null },
  { type: "exited", exitCode: 130 },
  { type: "title_changed", title: "vim" },
  { type: "foreground_process_changed", process: null },
  { type: "foreground_process_changed", process: "bash" },
]

const BREAKS: ((m: Record<string, unknown>) => unknown)[] = [
  (m) => ({ ...m, type: "unknown" }),
  (m) => ({ ...m, cols: "80" }),
  (m) => ({ ...m, info: { size: { cols: 0, rows: 24 } } }),
  (m) => ({ ...m, info: { size: { cols: 1.5, rows: 24 } } }),
  (m) => ({ ...m, info: { size: { cols: -3, rows: 24 } } }),
  (m) => ({ ...m, info: {} }),
  (m) => ({ ...m, role: "admin" }),
  (m) => ({ ...m, inputProtocol: "1" }),
  (m) => ({ ...m, attachmentID: 5 }),
  (m) => ({ ...m, exitCode: "0" }),
  (m) => ({ ...m, title: null }),
  (m) => ({ ...m, process: 3 }),
  (m) => ({ ...m, checkpoint: undefined }),
  () => [1, 2],
  () => "text",
  () => null,
]

export function ptyCases(random: Random, count: number) {
  const cases: unknown[] = []
  for (let i = 0; i < count; i++) {
    const texts = Array.from({ length: 6 }, () => {
      const message = random.pick(MESSAGES)
      if (random.bool(0.4)) return JSON.stringify(message)
      if (random.bool(0.1)) return random.pick(["{bad", "", "1e400", '{"type":"resized","cols":1e400,"rows":1,"checkpoint":"c"}'])
      return JSON.stringify(random.pick(BREAKS)(message as Record<string, unknown>))
    })
    const frames = Array.from({ length: 3 }, () => ({
      cols: random.pick([80, 0, 65535, 65536, 70000, 1.9, -1]),
      rows: random.pick([24, 1, 300]),
      ...(random.bool() ? { data: Array.from({ length: random.int(5) }, () => random.int(256)) } : {}),
    }))
    cases.push({
      kind: "pty",
      name: `pty #${i}`,
      texts,
      frames,
      connect: {
        ptyID: random.pick(["pty_1", "a b/c"]),
        cursor: random.pick([0, 12, 1.5]),
        attachmentID: random.pick(["att_1", "x&y"]),
        takeover: random.bool(),
        url: random.pick(["http://127.0.0.1:4096", "https://host/prefix", "http://localhost:1/a/"]),
        ticket: random.pick(["tk_1", "t k", "missing"]),
      },
    })
  }
  return cases
}
