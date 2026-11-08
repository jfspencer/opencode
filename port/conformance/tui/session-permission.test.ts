// Session view: permission requests shown in the TUI and answered with keys (allow once, always, reject).
import { baseConfig } from "../lib/model"
import { call, say, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed.
// The TS TUI shows a subagent's shell command ("$ rm -rf build") only when its client happens to hold the call it
// names; otherwise it lists the permission pattern ("- rm -rf build"). In-process that is always the case; against
// a slower server it never is. Open item (FINDINGS.md).
const SUBAGENT_PROMPT = "the TS TUI's prompt for a subagent's request depends on client timing (FINDINGS.md)"

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

type Effect = "allow" | "deny" | "ask"
const ask = (...rules: [string, string, Effect][]) => ({
  permissions: [...baseConfig.permissions, ...rules.map(([action, resource, effect]) => ({ action, resource, effect }))],
})

/** Submit a prompt and wait until the TUI shows the permission request it blocks on. */
async function prompt(t: TuiCtx, text: string) {
  const from = t.events.length
  await t.type(text)
  await t.enter()
  await t.until((event) => event?.type === "permission.asked", from, 60_000)
  await t.waitFor("Permission required")
}

/** Answer with a key press and wait until the turn is over and acknowledged, then for the text. */
async function answer(t: TuiCtx, keys: () => Promise<void>, text: string) {
  const from = t.events.length
  await keys()
  await t.until((event) => event?.type === "session.viewed", from, 60_000)
  await t.waitFor(text)
}

/** Reject with a key press: the execution is interrupted and the model is not asked again. */
async function reject(t: TuiCtx, keys: () => Promise<void>) {
  const from = t.events.length
  await keys()
  await t.until((event) => event?.type === "session.execution.interrupted", from, 60_000)
  await t.waitFor("interrupted")
}

const files = {
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n",
}

scenario(
  "shell permission allowed once",
  { files, config: ask(["shell", "*", "ask"]), turns: [call("shell", { command: "sleep 1; echo allowed" }), say("Ran it.")] },
  async (t) => {
    await prompt(t, "Echo something")
    await t.frame("asked")
    await answer(t, () => t.enter(), "Ran it.")
    await t.frame("allowed")
  },
)

scenario(
  "shell permission allowed always skips the next request",
  {
    files,
    config: ask(["shell", "*", "ask"]),
    turns: [call("shell", { command: "sleep 1; echo first" }), call("shell", { command: "sleep 1; echo second" }), say("Ran both.")],
  },
  async (t) => {
    await prompt(t, "Echo twice")
    await t.arrow("right")
    await t.frame("always-selected")
    await answer(t, () => t.enter(), "Ran both.")
    await t.frame("allowed")
  },
)

scenario(
  "shell permission rejected with escape",
  { files, config: ask(["shell", "*", "ask"]), turns: [call("shell", { command: "rm -rf build" })] },
  async (t) => {
    await prompt(t, "Clean the build")
    await reject(t, () => t.escape())
    await t.frame("rejected")
  },
)

scenario(
  "shell permission rejected by selecting reject",
  { files, config: ask(["shell", "*", "ask"]), turns: [call("shell", { command: "rm -rf build" })] },
  async (t) => {
    await prompt(t, "Clean the build")
    await t.arrow("left")
    await t.frame("reject-selected")
    await reject(t, () => t.enter())
    await t.frame("rejected")
  },
)

scenario(
  "edit permission shows the diff",
  {
    files,
    config: ask(["edit", "*", "ask"]),
    turns: [call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }), say("Edited.")],
  },
  async (t) => {
    await prompt(t, "Swap the operands")
    await t.frame("asked")
    await answer(t, () => t.enter(), "Edited.")
    await t.frame("edited")
  },
)

scenario(
  "edit permission in fullscreen",
  {
    files,
    height: 24,
    config: ask(["edit", "*", "ask"]),
    turns: [call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }), say("Edited.")],
  },
  async (t) => {
    await prompt(t, "Swap the operands")
    await t.press("f", { ctrl: true })
    await t.frame("fullscreen")
    // Escape first leaves fullscreen, then rejects.
    await t.escape()
    await t.frame("minimized")
    await t.press("f", { ctrl: true })
    await t.press("f", { ctrl: true })
    await answer(t, () => t.enter(), "Edited.")
  },
)

scenario(
  "write permission for a new file",
  {
    files,
    config: ask(["edit", "*", "ask"]),
    turns: [call("write", { path: "src/new.ts", content: "export const created = true\n" }), say("Wrote it.")],
  },
  async (t) => {
    await prompt(t, "Create a file")
    await t.frame("asked")
    await answer(t, () => t.enter(), "Wrote it.")
  },
)

scenario(
  "read permission and external directory",
  {
    files,
    config: ask(["read", "*", "ask"], ["external_directory", "*", "ask"]),
    turns: [call("read", { path: "src/math.ts" }), call("read", { path: "../elsewhere/notes.txt" })],
  },
  async (t) => {
    await prompt(t, "Read files")
    await t.frame("read-asked")
    const from = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor("external directory")
    await t.frame("external-asked")
    await reject(t, () => t.escape())
    await t.frame("done")
  },
)

scenario(
  "glob, grep and webfetch permissions",
  {
    files,
    config: ask(["glob", "*", "ask"], ["grep", "*", "ask"], ["webfetch", "*", "ask"]),
    turns: [
      call("glob", { pattern: "**/*.ts" }),
      call("grep", { pattern: "add" }),
      call("webfetch", { url: "https://example.com/" }),
    ],
  },
  async (t) => {
    await prompt(t, "Look around")
    await t.frame("glob-asked")
    let from = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor('Grep "add"')
    await t.frame("grep-asked")
    from = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "permission.asked", from, 60_000)
    await t.waitFor("WebFetch")
    await t.frame("webfetch-asked")
    await reject(t, () => t.escape())
    await t.frame("done")
  },
)

scenario(
  "permission prompt on a narrow terminal",
  {
    files,
    width: 60,
    height: 24,
    config: ask(["shell", "*", "ask"]),
    tui: { session: { tps: false } },
    turns: [call("shell", { command: "sleep 1; echo narrow" }), say("Ran.")],
  },
  async (t) => {
    await prompt(t, "Echo")
    await t.frame("asked")
    await answer(t, () => t.enter(), "Ran.")
  },
)

scenario(
  "permissions accepted automatically",
  {
    files,
    config: ask(["shell", "*", "ask"]),
    tui: { session: { permissions: "autoaccept" } },
    turns: [call("shell", { command: "sleep 1; echo auto" }), say("Ran without asking.")],
  },
  async (t) => {
    const from = t.events.length
    await t.type("Echo")
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("Ran without asking.")
    await t.frame("ran")
  },
)

scenario(
  "permission requested by a subagent is rejected with a message",
  {
    inProcessOnly: SUBAGENT_PROMPT,
    files,
    height: 40,
    config: ask(["shell", "*", "ask"]),
    turns: [
      call("subagent", { agent: "general", description: "Clean up", prompt: "Delete the build directory" }),
      call("shell", { command: "rm -rf build" }),
      say("The user declined."),
      say("The subagent was stopped."),
    ],
  },
  async (t) => {
    await prompt(t, "Delegate the cleanup")
    // The request shows the command once the subagent's tool call has loaded ("- " before, "$ " after).
    await t.waitFor("$ rm -rf build")
    await t.frame("asked")
    await t.escape()
    await t.waitFor("Reject permission")
    await t.frame("reject-reason")
    // Escape goes back to the request; escape again returns to the reason.
    await t.escape()
    await t.waitFor("Permission required")
    await t.escape()
    await t.waitFor("Reject permission")
    await t.type("Keep the build")
    await t.frame("reason-typed")
    const from = t.events.length
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("The subagent was stopped.")
    await t.frame("done")
  },
)
