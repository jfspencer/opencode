// Session view: the session's own dialogs. The timeline (jump to a user message), message actions, and forking.
// The sessions are imported with fixed timestamps, because both dialogs list each prompt's time of day.
import { MODEL_ID } from "../lib/model"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

const T0 = Date.UTC(2026, 0, 5, 10, 0, 0)
const MODEL = { providerID: "openrouter", id: MODEL_ID, variant: "max" }
const tokens = (input: number, output: number) => ({ input, output, reasoning: 0, cache: { read: 0, write: 0 } })
const id = (prefix: string, n: number) => `${prefix}_00000000000000conformance${String(n).padStart(3, "0")}`

/** A conversation of `count` prompts, a minute apart, each answered with a long reply. */
function conversation(count: number) {
  return Array.from({ length: count }, (_, i) => [
    { id: id("msg", i * 2 + 1), type: "user", text: `Question number ${i + 1}`, time: { created: T0 + i * 60_000 } },
    {
      id: id("msg", i * 2 + 2),
      type: "assistant",
      agent: "build",
      model: MODEL,
      content: [{ type: "text", text: Array.from({ length: 8 }, (_, line) => `Answer ${i + 1}, line ${line + 1}.`).join("\n\n") }],
      finish: "stop",
      tokens: tokens(500, 40),
      time: { created: T0 + i * 60_000 + 1000, completed: T0 + i * 60_000 + 3000 },
    },
  ]).flat()
}

/** Import a session with the given history, then open it from the session list. */
async function open(t: TuiCtx, title: string, messages: unknown[]) {
  await t.http("POST", "/api/experimental/session/import", {
    info: {
      id: id("ses", 1),
      projectID: "0000000000000000000000000000000000000000",
      title,
      cost: 0,
      tokens: tokens(0, 0),
      time: { created: T0, updated: T0 },
      location: { directory: t.project },
    },
    location: { directory: t.project },
    messages,
  })
  await t.press("x", { ctrl: true })
  await t.press("l")
  await t.waitFor(title)
  await t.type(title)
  await t.enter()
  await t.waitFor(new RegExp(`^\\s*${title}`))
}

function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  tuiScenario(name, options, async (t) => {
    const started = Date.now()
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

scenario("the timeline jumps to a prompt", { height: 30 }, async (t) => {
  await open(t, "Long talk", conversation(5))
  await t.waitFor("Answer 5, line 8.")
  await t.type("/timeline")
  await t.enter()
  await t.waitFor("Timeline")
  await t.frame("timeline")
  // Moving through the list scrolls the transcript to the highlighted prompt.
  await t.arrow("down")
  await t.arrow("down")
  await t.frame("moved")
  await t.enter()
  await t.waitFor("Message Actions")
  await t.frame("actions")
  await t.enter()
  await t.frame("jumped")
})

scenario("the timeline filtered by search", { height: 30 }, async (t) => {
  await open(t, "Long talk", conversation(5))
  await t.type("/timeline")
  await t.enter()
  await t.waitFor("Timeline")
  await t.type("number 2")
  await t.frame("filtered")
  await t.escape()
  await t.frame("closed")
})

scenario("fork the whole session", { height: 30 }, async (t) => {
  await open(t, "Long talk", conversation(3))
  await t.type("/fork")
  await t.enter()
  await t.waitFor("Fork session")
  await t.frame("choices")
  await t.enter()
  await t.waitFor("Forked session")
  await t.frame("forked")
})

scenario("fork from a prompt through its message actions", { height: 30 }, async (t) => {
  await open(t, "Long talk", conversation(3))
  await t.type("/timeline")
  await t.enter()
  await t.waitFor("Timeline")
  await t.arrow("down")
  await t.enter()
  await t.waitFor("Message Actions")
  await t.arrow("down")
  await t.arrow("down")
  await t.arrow("down")
  await t.frame("fork-selected")
  await t.enter()
  await t.waitFor("Forked session")
  await t.frame("forked")
})
