// Session view: session tabs (several sessions open at once) and the session sidebar.
import { say, type Responder, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Every session gets a generated title, concurrently with its first step. Name each session after its first
// prompt, and hold the first step of every session until that session is renamed, so the recorded event
// order is fixed. Turns listed in `firsts` are the first step of a session (by index in `turns`).
function scenario(
  name: string,
  options: TuiOptions & { firsts?: number[] },
  body: (t: TuiCtx) => Promise<void>,
) {
  const gates = (options.firsts ?? [0]).map(() => Promise.withResolvers<void>())
  const turns = (options.turns ?? []).map((turn, index): Turn => {
    const at = (options.firsts ?? [0]).indexOf(index)
    if (at === -1 || !("events" in turn)) return turn
    return { ...turn, events: [{ until: gates[at]!.promise }, ...turn.events] }
  })
  tuiScenario(name, { ...options, turns, responders: [title, ...(options.responders ?? [])] }, async (t) => {
    const started = Date.now()
    gates.forEach((gate, index) =>
      t
        .until((event) => event?.type === "session.renamed" && renamed(t) > index, 0, 120_000)
        .then(gate.resolve, gate.resolve),
    )
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

const renamed = (t: TuiCtx) => t.events.filter((event: any) => event?.type === "session.renamed").length

/** Titles name the session after its first prompt, so tabs are told apart. */
const title: Responder = (request) => {
  if (request.kind !== "title") return
  const messages = (request.body as { messages?: { role: string; content: unknown }[] }).messages ?? []
  const text = messages.findLast((message) => message.role === "user")?.content
  const first = typeof text === "string" ? /<message>\n([^\n]*)\n<\/message>/.exec(text)?.[1] : undefined
  return say(first ? `${first} session` : "Untitled session")
}

/** Submit a prompt and wait until the turn is over and the TUI has acknowledged it, then for the text. */
async function ask(t: TuiCtx, prompt: string, text: string | RegExp) {
  const from = t.events.length
  await t.type(prompt)
  await t.enter()
  await t.until((event) => event?.type === "session.viewed", from, 60_000)
  await t.waitFor(text)
}

async function leader(t: TuiCtx, key: string) {
  await t.press("x", { ctrl: true })
  await t.press(key)
}

scenario(
  "two sessions in tabs, switched, closed and reopened",
  { firsts: [0, 1], turns: [say("Answer about alpha."), say("Answer about beta.")], tui: { tabs: { mode: "on" } } },
  async (t) => {
    await ask(t, "Alpha", "Answer about alpha.")
    await t.waitFor("Alpha session")
    await leader(t, "n")
    await t.frame("new-tab")
    await ask(t, "Beta", "Answer about beta.")
    await t.waitFor("Beta session")
    await t.frame("two-tabs")
    await t.press("ARROW_UP", { meta: true })
    await t.waitFor("Answer about alpha.")
    await t.frame("previous-tab")
    await t.press("ARROW_DOWN", { meta: true })
    await t.waitFor("Answer about beta.")
    await t.frame("next-tab")
    await t.press("1", { ctrl: true })
    await t.waitFor("Answer about alpha.")
    await t.frame("first-tab")
    await leader(t, "w")
    await t.frame("closed")
    await t.press("t", { ctrl: true, shift: true })
    await t.frame("reopened")
  },
)

{
  const gate = Promise.withResolvers<void>()
  scenario(
    "a background tab finishing shows as unread",
    {
      firsts: [0, 2],
      turns: [
        say("Quick answer."),
        { events: [{ text: "Slow answer" }, { until: gate.promise }, { text: " finished." }] },
        say("Second session answer."),
      ],
      tui: { tabs: { mode: "on" } },
    },
    async (t) => {
      await ask(t, "First", "Quick answer.")
      await t.type("Something slow")
      await t.enter()
      await t.waitFor("Slow answer")
      await leader(t, "n")
      await ask(t, "Second", "Second session answer.")
      await t.waitFor("Second session")
      await t.frame("other-tab-running")
      const from = t.events.length
      gate.resolve()
      await t.until((event) => event?.type === "session.execution.succeeded", from, 60_000)
      await t.frame("other-tab-unread")
      await t.press("ARROW_DOWN", { meta: true, shift: true })
      await t.waitFor("Slow answer finished.")
      await t.until((event) => event?.type === "session.viewed", from, 60_000)
      await t.frame("unread-visited")
    },
  )
}

scenario(
  "vertical tabs with numbers",
  {
    firsts: [0, 1],
    width: 140,
    height: 30,
    turns: [say("Answer about alpha."), say("Answer about beta.")],
    tui: { tabs: { mode: "on", layout: "vertical", indicators: "numbers" }, session: { sidebar: "hide" } },
  },
  async (t) => {
    await ask(t, "Alpha", "Answer about alpha.")
    await leader(t, "n")
    await ask(t, "Beta", "Answer about beta.")
    await t.waitFor("Beta session")
    await t.frame("vertical")
  },
)

scenario(
  "tabs turned off",
  { turns: [say("No tabs here.")], tui: { tabs: { mode: "off" } } },
  async (t) => {
    await ask(t, "Hello", "No tabs here.")
    await t.frame("no-tabs")
  },
)

scenario(
  "sidebar on a wide terminal, hidden and shown again",
  {
    git: true,
    width: 160,
    height: 36,
    turns: [say("Counted the tokens.", { prompt: 52_000, completion: 1_200, reasoning: 300, cached: 2_000 })],
  },
  async (t) => {
    await ask(t, "Count tokens", "Counted the tokens.")
    await t.frame("sidebar")
    await leader(t, "b")
    await t.frame("hidden")
    await leader(t, "b")
    await t.frame("shown")
  },
)

scenario(
  "sidebar as an overlay on a medium terminal",
  { git: true, width: 110, height: 30, turns: [say("Small terminal.")] },
  async (t) => {
    await ask(t, "Hello", "Small terminal.")
    await leader(t, "b")
    await t.frame("overlay")
    await t.escape()
    await t.frame("closed")
  },
)
