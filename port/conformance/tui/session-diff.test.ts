// Session view: the diff viewer panel for a session's changes (git working tree and the last turn).
import { call, say, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed.
function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  const titled = Promise.withResolvers<void>()
  const [first, ...rest] = options.turns ?? []
  const turns: Turn[] | undefined =
    first && "events" in first ? [{ ...first, events: [{ until: titled.promise }, ...first.events] }, ...rest] : options.turns
  tuiScenario(name, { ...options, turns }, async (t) => {
    const started = Date.now()
    t.until((event) => event?.type === "session.renamed", 0, 60_000).then(titled.resolve, titled.resolve)
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

/** Submit a prompt and wait until the turn is over and the TUI has acknowledged it, then for the text. */
async function ask(t: TuiCtx, prompt: string, text: string | RegExp) {
  const from = t.events.length
  await t.type(prompt)
  await t.enter()
  await t.until((event) => event?.type === "session.viewed", from, 60_000)
  await t.waitFor(text)
}

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": Array.from({ length: 30 }, (_, i) => `export const n${i} = ${i}`).join("\n") + "\n",
  "src/old.ts": "export const old = true\n",
}

const changes: Turn[] = [
  call("edit", { path: "src/math.ts", oldString: "export const n3 = 3", newString: "export const n3 = 300" }),
  call("edit", { path: "src/math.ts", oldString: "export const n25 = 25", newString: "export const n25 = 2500" }),
  call("write", { path: "src/new.ts", content: "export const created = 1\n" }),
  call("shell", { command: "sleep 1; rm src/old.ts" }),
  say("Changed three files."),
]

// The diff panel shares rows with the turn footer: keep the footer a fixed width (no throughput; the shell call
// makes the duration a single "N.Ns").
const tui = { keybinds: { "diff.open": "f8" }, session: { tps: false } }

scenario(
  "diff viewer opened beside the session",
  { git: true, files, width: 160, height: 40, turns: changes, tui },
  async (t) => {
    await ask(t, "Make some changes", "Changed three files.")
    await t.press("F8")
    await t.waitFor("src/math.ts")
    await t.frame("opened")
    await t.press("n")
    await t.frame("next-file")
    await t.press("]")
    await t.frame("next-hunk")
    await t.press("[")
    await t.press("p")
    await t.frame("previous-file")
    await t.press("j")
    await t.press("j")
    await t.frame("down")
    await t.press("G", { shift: true })
    await t.frame("last")
    await t.press("g")
    await t.press("g")
    await t.frame("first")
    await t.press("q")
    await t.frame("closed")
  },
)

scenario(
  "diff viewer layouts and sources",
  { git: true, files, width: 160, height: 40, turns: changes, tui },
  async (t) => {
    await ask(t, "Make some changes", "Changed three files.")
    await t.press("F8")
    await t.waitFor("src/math.ts")
    await t.press("v")
    await t.frame("toggled-view")
    await t.press("b")
    await t.frame("toggled-tree")
    await t.press("s")
    await t.frame("single-patch")
    await t.press("m")
    await t.frame("reviewed")
    await t.press("f")
    await t.frame("fullscreen")
    await t.press("d")
    await t.frame("source-picker")
    await t.arrow("down")
    await t.arrow("down")
    await t.arrow("down")
    await t.enter()
    await t.frame("last-turn")
    await t.press("?")
    await t.frame("help")
    await t.escape()
    await t.escape()
    await t.frame("closed")
  },
)

scenario(
  "diff viewer with nothing changed",
  { git: true, files, width: 120, height: 30, turns: [say("Nothing to change.")], tui },
  async (t) => {
    await ask(t, "Change nothing", "Nothing to change.")
    await t.type("/diff")
    await t.enter()
    await t.frame("empty")
    await t.escape()
  },
)

scenario(
  "diff viewer on a narrow terminal",
  { git: true, files, width: 80, height: 30, turns: changes, tui },
  async (t) => {
    await ask(t, "Make some changes", "Changed three files.")
    await t.press("F8")
    await t.waitFor("src/math.ts")
    await t.frame("opened")
    await t.press("\x1b[6~")
    await t.frame("page-down")
    await t.press("u", { ctrl: true })
    await t.frame("half-page-up")
    await t.escape()
    await t.frame("closed")
  },
)
