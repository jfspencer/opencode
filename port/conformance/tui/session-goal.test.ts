// Session view: a session goal (`/goal`), shown as a notice in the transcript and a label beside the prompt,
// through its reviewer's verdicts.
import { say, type Turn } from "../lib/openrouter"
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

scenario(
  "a goal met after one round",
  {
    height: 24,
    turns: [
      say("Getting ready."),
      say("Wrote the docs."),
      say('{"verdict":"done","reason":"The docs are written."}'),
    ],
  },
  async (t) => {
    await ask(t, "Hello", "Getting ready.")
    await ask(t, "/goal Write the docs", "Wrote the docs.")
    await t.waitFor("goal met")
    await t.frame("met")
  },
)

scenario(
  "a goal paused when its reviewer gives no verdict",
  {
    height: 24,
    turns: [say("Getting ready."), say("Tried once."), say("I am not sure."), say("Still not sure.")],
  },
  async (t) => {
    await ask(t, "Hello", "Getting ready.")
    await ask(t, "/goal Ship the release", "Tried once.")
    await t.waitFor("goal paused")
    await t.frame("paused")
    // Clearing is a command, not a turn: wait for the label to go away.
    await t.type("/goal clear")
    await t.enter()
    await t.waitFor(/^(?![\s\S]*goal paused)/)
    await t.frame("cleared")
  },
)
