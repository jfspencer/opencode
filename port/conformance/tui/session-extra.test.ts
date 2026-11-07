// Session view: rarer renderings. Diffs with several hunks, very large added files in the diff viewer,
// nested instruction files loaded mid-session, pasting into a question form, and a very small terminal.
import { baseConfig } from "../lib/model"
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

const lines = (count: number, at: (i: number) => string) => Array.from({ length: count }, (_, i) => at(i)).join("\n") + "\n"

scenario(
  "an edit with several hunks",
  {
    files: { "src/config.ts": lines(40, (i) => (i === 2 || i === 36 ? `export const flag${i} = "old"` : `export const v${i} = ${i}`)) },
    height: 40,
    turns: [
      call("edit", { path: "src/config.ts", oldString: '"old"', newString: '"new"', replaceAll: true }),
      say("Updated both flags."),
    ],
  },
  async (t) => {
    await ask(t, "Update the flags", "Updated both flags.")
    await t.frame("hunks")
  },
)

scenario(
  "a very large added file in the diff viewer",
  {
    git: true,
    files: { "README.md": "# Demo\n" },
    width: 120,
    height: 30,
    turns: [call("shell", { command: "sleep 1; seq 1 3200 > numbers.txt" }), say("Wrote 3200 numbers.")],
    tui: { keybinds: { "diff.open": "f8" }, session: { sidebar: "hide" } },
  },
  async (t) => {
    await ask(t, "Write numbers", "Wrote 3200 numbers.")
    await t.press("F8")
    // The transcript already names numbers.txt: wait for the diff viewer's own header and rows.
    await t.waitFor("+3200 -0")
    await t.press("f")
    await t.waitFor("1 + 1")
    await t.frame("top")
    await t.press("\x1b[6~")
    await t.press("\x1b[6~")
    await t.frame("paged")
    await t.press("G", { shift: true })
    await t.frame("end")
    await t.escape()
  },
)

scenario(
  "nested instructions loaded by a read",
  {
    files: {
      "AGENTS.md": "# Root rules\n",
      "pkg/AGENTS.md": "# Package rules\n\nUse tabs.\n",
      "pkg/lib/AGENTS.md": "# Library rules\n",
      "pkg/lib/index.ts": "export const lib = 1\n",
      "pkg/main.ts": "export const main = 1\n",
    },
    height: 30,
    turns: [
      call("read", { path: "pkg/main.ts" }),
      call("read", { path: "pkg/lib/index.ts" }),
      say("Read the package."),
    ],
  },
  async (t) => {
    await ask(t, "Read the package", "Read the package.")
    await t.frame("medium")
  },
)

scenario(
  "nested instructions at high verbosity",
  {
    files: {
      "pkg/AGENTS.md": "# Package rules\n",
      "pkg/lib/AGENTS.md": "# Library rules\n",
      "pkg/lib/index.ts": "export const lib = 1\n",
      "pkg/main.ts": "export const main = 1\n",
    },
    height: 30,
    tui: { session: { verbosity: "high" } },
    turns: [
      call("read", { path: "pkg/main.ts" }),
      call("read", { path: "pkg/lib/index.ts" }),
      say("Read the package."),
    ],
  },
  async (t) => {
    await ask(t, "Read the package", "Read the package.")
    await t.frame("high")
  },
)

const color = {
  question: "Which color should the button be?",
  header: "Button color",
  options: [
    { label: "Blue", description: "Matches the brand palette" },
    { label: "Green", description: "Signals success" },
  ],
}

scenario(
  "pasting a custom answer into a question",
  { height: 30, turns: [call("question", { questions: [color, { ...color, header: "Border color", question: "And the border?" }] }), say("Got it.")] },
  async (t) => {
    const from = t.events.length
    await t.type("Style it")
    await t.enter()
    await t.until((event) => event?.type === "form.created", from, 60_000)
    await t.waitFor("Which color should the button be?")
    await t.paste("Teal, like the sea")
    await t.frame("pasted")
    await t.enter()
    await t.frame("second")
    await t.arrow("up")
    await t.enter()
    await t.paste("Gray\nwith lines")
    await t.frame("pasted-multiline")
    await t.press("TAB", { shift: true })
    await t.frame("back")
    const done = t.events.length
    await t.tab()
    await t.tab()
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", done, 60_000)
    await t.waitFor("Got it.")
    await t.frame("answered")
  },
)

scenario(
  "a very small terminal",
  {
    // Narrow enough that the turn footer drops its duration (which would otherwise wrap unnormalized).
    width: 34,
    height: 16,
    files: { "a.txt": "a\n" },
    config: { permissions: [...baseConfig.permissions, { action: "shell", resource: "*", effect: "ask" }] },
    tui: { session: { tps: false } },
    turns: [call("shell", { command: "sleep 1; cat a.txt" }), say("Done in a small space.")],
  },
  async (t) => {
    const from = t.events.length
    await t.type("Cat a")
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor("Permission required")
    await t.frame("asked")
    const done = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", done, 60_000)
    await t.waitFor("Done in a small space.")
    await t.frame("done")
  },
)

const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="

scenario(
  "an added image in the diff viewer",
  {
    git: true,
    files: { "README.md": "# Demo\n" },
    width: 120,
    height: 30,
    turns: [call("shell", { command: `sleep 1; echo ${PNG} | base64 -d > pixel.png` }), say("Added an image.")],
    tui: { keybinds: { "diff.open": "f8" }, session: { sidebar: "hide" } },
  },
  async (t) => {
    await ask(t, "Add an image", "Added an image.")
    await t.press("F8")
    await t.waitFor("pixel.png")
    await t.frame("image")
    await t.escape()
  },
)

scenario(
  "an edit permission on a wide terminal shows a split diff",
  {
    files: { "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n" },
    width: 160,
    height: 30,
    config: { permissions: [...baseConfig.permissions, { action: "edit", resource: "*", effect: "ask" }] },
    tui: { session: { sidebar: "hide" } },
    turns: [call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }), say("Edited.")],
  },
  async (t) => {
    const from = t.events.length
    await t.type("Swap")
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor("Permission required")
    await t.frame("split")
    const done = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", done, 60_000)
    await t.waitFor("Edited.")
  },
)

scenario(
  "a long form review scrolled with keys",
  { height: 22, turns: [say("Ready.")] },
  async (t) => {
    await ask(t, "Get ready", "Ready.")
    const created = t.events.find((event: any) => event?.type === "session.created") as any
    const fields = Array.from({ length: 12 }, (_, i) => ({ key: `f${i}`, type: "boolean", title: `Option ${i + 1}`, default: i % 2 === 0 }))
    const from = t.events.length
    await t.http("POST", `/api/session/${created.data.sessionID}/form`, { id: "frm_conformance0000000000000009", title: "Many options", fields })
    await t.until((event) => event?.type === "form.created", from)
    await t.waitFor("Many options")
    await t.press("TAB", { shift: true })
    await t.frame("review")
    await t.press("j")
    await t.press("j")
    await t.arrow("down")
    await t.frame("scrolled")
    await t.press("k")
    await t.arrow("up")
    await t.frame("scrolled-back")
    const replied = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "form.replied", replied)
    await t.frame("submitted")
  },
)

scenario(
  "clearing and cancelling a rejection reason",
  {
    height: 30,
    config: { permissions: [...baseConfig.permissions, { action: "shell", resource: "*", effect: "ask" }] },
    turns: [
      call("subagent", { agent: "general", description: "Clean up", prompt: "Delete the build directory" }),
      call("shell", { command: "rm -rf build" }),
      // Rejected without a reason, the subagent stops and the parent answers.
      say("The subagent was stopped."),
    ],
  },
  async (t) => {
    const from = t.events.length
    await t.type("Delegate the cleanup")
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor("Permission required")
    await t.arrow("left")
    await t.enter()
    await t.waitFor("Reject permission")
    await t.type("Not now")
    await t.press("c", { ctrl: true })
    await t.frame("cleared")
    await t.press("c", { ctrl: true })
    await t.waitFor("Permission required")
    await t.frame("cancelled")
    await t.escape()
    await t.waitFor("Reject permission")
    const done = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", done, 60_000)
    await t.waitFor("The subagent was stopped.")
    await t.frame("done")
  },
)
