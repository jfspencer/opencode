// Hand-picked programs for the opencode-tui-input stdin check (see oracle.ts for the op language): OpenTUI's own
// conformance vectors (packages/core/src/lib/stdin-parser.conformance.json, each also one byte per read), and
// cases for the timeout, deferred replies and the renderer's context hooks.
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const vectors = JSON.parse(
  readFileSync(join(here, "../../../../.vendor/opentui/packages/core/src/lib/stdin-parser.conformance.json"), "utf8"),
) as { rows: [string, (string | number[] | null)[], string[], string | null | undefined, { kitty?: boolean; pending?: string[] }?][] }

const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString("hex") || "-"
const bytesOf = (step: string | number[]) => (typeof step === "string" ? new TextEncoder().encode(step) : Uint8Array.from(step))

/** `new` as the conformance suite makes its timed parser: kitty context, and "cpr"/"pixel" queries pending. */
function conformanceNew(context: { kitty?: boolean; pending?: string[] } = {}): string {
  const pending = context.pending ?? []
  const bits = [
    context.kitty ? "1" : "0",
    "-",
    pending.includes("pixel") ? "1" : "-",
    pending.includes("cpr") ? "1" : "-",
    pending.includes("cpr") ? "1" : "-",
    "-",
  ].join("")
  return `new 20 - 1 - ${bits}`
}

const conformance: string[][] = []
for (const [, steps, , , context] of vectors.rows) {
  for (const split of [false, true]) {
    const ops = [conformanceNew(context)]
    for (const step of steps) {
      if (step === null) ops.push("adv 20")
      else if (split) for (const byte of bytesOf(step)) ops.push(`push ${byte.toString(16).padStart(2, "0")}`)
      else ops.push(`push ${hex(bytesOf(step))}`)
    }
    ops.push("adv 20")
    conformance.push(ops)
  }
}

const ascii = (text: string) => hex(Uint8Array.from(text, (c) => c.charCodeAt(0)))

const timing: string[][] = [
  // A lone ESC: 19.5 ms is not enough, 20 is.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b")}`, "adv 19.5", "pix", "adv 0.5", `push ${ascii("[A")}`, "adv 30"],
  // Each read re-arms the timer from its own time.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[")}`, "adv 15", `push ${ascii("1")}`, "adv 15", `push ${ascii(";5")}`, "adv 19", `push ${ascii("A")}`, "adv 50"],
  // ESC timeout, then a delayed SGR mouse continuation.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b")}`, "adv 25", `push ${ascii("[<35;10;5M")}`, "adv 25"],
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b")}`, "adv 25", `push ${ascii("[M !!")}`, "adv 25", `push ${ascii("\x1b")}`, "adv 25", `push ${ascii("[x")}`, "adv 25"],
  // A split Kitty key waits past the timeout only while Kitty is negotiated.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[97;")}`, "adv 40", `push ${ascii("5u")}`, "adv 40"],
  ["new 20 - 1 1 0-----", `push ${ascii("\x1b[97;")}`, "adv 40", `push ${ascii("5u")}`, "adv 40"],
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[97;")}`, "adv 40", "ctx 0 0 1", `push ${ascii("5u")}`, "adv 40"],
  // Private capability replies and start-up CPRs while the renderer waits for them.
  ["new 20 - 1 1 01--1-", `push ${ascii("\x1b[?62;")}`, "adv 40", `push ${ascii("22c")}`, `push ${ascii("\x1b[12;")}`, "adv 40", `push ${ascii("40R")}`, "adv 40"],
  ["new 20 - 1 1 01--1-", `push ${ascii("\x1b[?2026;2")}`, "adv 40", "ctx 1 0 1", `push ${ascii("$y")}`, "adv 40"],
  ["new 20 - 1 1 0---1-", `push ${ascii("\x1b[12")}`, "abort", "adv 10", `push ${ascii(";40R")}`, "adv 40"],
  ["new 20 - 1 1 0---1-", `push ${ascii("\x1b[1")}`, "abort", "adv 30", `push ${ascii(";40R")}`, "adv 40"],
  ["new 20 - 1 1 0--11-", `push ${ascii("\x1b[1;")}`, "abort", `push ${ascii("2R")}`, "adv 40"],
  ["new 20 - 1 1 0---1-", `push ${ascii("\x1b[12;4")}`, "abort", `push ${ascii("0Rx")}`, "adv 40"],
  // A pixel-resolution reply held while the renderer pauses the timeout.
  ["new 20 - 1 1 0-1---", `push ${ascii("\x1b[4;6")}`, "pause", "pix", "adv 100", `push ${ascii("00;8")}`, "adv 100", "resume", `push ${ascii("00t")}`, "adv 40"],
  ["new 20 - 1 1 0-1---", `push ${ascii("\x1b[4;6")}`, "pause", "adv 100", `push ${ascii("x")}`, "adv 40"],
  ["new 20 - 1 1 0-1---", `push ${ascii("\x1b[4;6")}`, "pause", "adv 100", "ctx 2 0 1", "adv 40"],
  // A paste split anywhere, with markers in it, and one that never ends.
  ["new 20 - 1 1 1-----", `push ${ascii("ab\x1b[20")}`, `push ${ascii("0~hel")}`, "adv 100", `push ${ascii("lo\x1b[2")}`, `push ${ascii("01")}`, `push ${ascii("~\x1b")}`, "adv 30"],
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[200~x\x1b[201\x1b[201~y")}`, "adv 30"],
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[200~endless")}`, "adv 100", "pix", "reset", `push ${ascii("q")}`],
  // Overflow: a reply longer than the pending cap is one opaque response.
  ["new 20 8 1 1 1-----", `push ${ascii("\x1b]11;rgb:1e1e/1e1e/1e1e")}`, `push ${ascii("\x07z")}`, "adv 30"],
  // Timeouts off: only flushTimeout flushes.
  ["new 20 - 0 1 1-----", `push ${ascii("\x1b")}`, "adv 100", "flush -", "flush 25", "read", "read"],
  // Mouse drag state across reports, and its reset.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b[<0;5;5M\x1b[<32;6;5M")}`, "mreset", `push ${ascii("\x1b[<32;7;5M\x1b[<0;7;5m\x1b[<35;8;5M")}`],
  // A destroyed parser.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b")}`, "destroy", "adv 30", `push ${ascii("a")}`, "read", "pix", "cap", "reset"],
  // Capacity growth and compaction.
  ["new 20 - 1 1 1-----", `push ${ascii("\x1b]" + "x".repeat(600))}`, "cap", `push ${ascii("\x07")}`, "cap", "push -", "adv 30"],
]

export const handPicked: string[][] = [...conformance, ...timing]
