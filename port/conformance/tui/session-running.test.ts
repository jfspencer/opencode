// Session view: work in flight. Running shells (live output, moving them to the background, the composer's shell
// list and output viewer, killing them), running subagents (picker, live child view, interrupting), tool calls
// still streaming, and an aside question answered outside the transcript.
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

/** t.waitFor gives up after a fixed number of rendered frames; keep waiting through quiet periods. */
async function waitLong(t: TuiCtx, match: string | RegExp, ms = 30_000) {
  const deadline = Date.now() + ms
  while (true) {
    try {
      return await t.waitFor(match)
    } catch (error) {
      if (Date.now() > deadline) throw error
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
  }
}

scenario(
  "a long shell command moved to the background, viewed and killed",
  {
    height: 30,
    timeout: 120_000,
    turns: [
      call("shell", { command: "echo started && sleep 60" }),
      say("The server keeps running in the background."),
      say("Waiting for it."),
      say("Noted that it was stopped."),
    ],
  },
  async (t) => {
    await t.type("Start the server")
    await t.enter()
    await waitLong(t, "echo started && sleep 60")
    // The hint to background the command appears after it has run for a few seconds.
    await waitLong(t, "to move running work to the background")
    await t.frame("running")
    let from = t.events.length
    await t.press("b", { ctrl: true })
    // Moving the command to the background ends the step, then its notice is delivered as one more step.
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("Waiting for it.")
    await t.frame("backgrounded")
    await t.arrow("down")
    await t.arrow("right")
    await waitLong(t, "sleep 60")
    await t.frame("shell-tab")
    await t.enter()
    await waitLong(t, "started")
    await t.frame("shell-output")
    await t.escape()
    from = t.events.length
    await t.press("d", { ctrl: true })
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await waitLong(t, "Noted that it was stopped.")
    await t.escape()
    await t.frame("killed")
  },
)

{
  const gate = Promise.withResolvers<void>()
  scenario(
    "a running subagent seen from the picker and interrupted",
    {
      height: 30,
      turns: [
        call("subagent", { agent: "general", description: "Long research", prompt: "Research for a long time" }),
        { events: [{ text: "Researching" }, { until: gate.promise }, { text: " forever." }] },
        say("The subagent was interrupted."),
      ],
    },
    async (t) => {
      await t.type("Research with a subagent")
      await t.enter()
      // The child's text streams in the child session; the parent shows the running subagent.
      await t.until((event) => event?.type === "session.text.delta" && event.data.delta === "Researching", 0, 60_000)
      await t.waitFor("General Subagent — Long research")
      await t.frame("running")
      // The first press moves through the (empty) prompt history; the next one opens the picker.
      await t.arrow("down")
      await t.arrow("down")
      await t.waitFor(/Subagents\s+Shell/)
      await t.frame("picker")
      await t.enter()
      await t.waitFor("Research for a long time")
      await t.frame("child-running")
      await t.arrow("up")
      await t.waitFor("Research with a subagent")
      await t.arrow("down")
      await t.arrow("down")
      await t.waitFor(/Subagents\s+Shell/)
      const from = t.events.length
      await t.press("d", { ctrl: true })
      gate.resolve()
      await t.until((event) => event?.type === "session.viewed", from, 60_000)
      await t.waitFor("The subagent was interrupted.")
      await t.escape()
      await t.frame("interrupted")
    },
  )
}

{
  const gate = Promise.withResolvers<void>()
  scenario(
    "tool calls shown while the step is still streaming",
    {
      files: { "a.txt": "a\n" },
      height: 40,
      tui: { session: { grouping: "none" } },
      turns: [
        {
          events: [
            { text: "Let me do several things." },
            { tool: "write", input: { path: "b.txt", content: "b\n" } },
            { tool: "edit", input: { path: "a.txt", oldString: "a", newString: "A" } },
            { tool: "shell", input: { command: "ls" } },
            { tool: "read", input: { path: "a.txt" } },
            { tool: "question", input: { questions: [{ question: "Sure?", header: "Sure", options: [{ label: "Yes", description: "y" }] }] } },
            { tool: "subagent", input: { agent: "general", description: "Helper", prompt: "Help" } },
            { tool: "execute", input: { code: "1" } },
            { until: gate.promise },
          ],
          finish: "stop",
        },
      ],
    },
    async (t) => {
      await t.type("Do things")
      await t.enter()
      await t.waitFor("Let me do several things.")
      await t.frame("streaming")
      // Interrupt before the step ends, so none of the calls run.
      const from = t.events.length
      await t.escape()
      await t.escape()
      await t.until((event) => event?.type === "session.execution.interrupted", from, 60_000)
      gate.resolve()
      await t.waitFor("interrupted")
      await t.frame("interrupted")
    },
  )
}

scenario(
  "an aside question answered in a dialog",
  { height: 30, turns: [say("We are building a parser."), say("**Four.** Two plus two is `4`.")] },
  async (t) => {
    await ask(t, "What are we doing?", "We are building a parser.")
    await t.type("/btw What is 2+2?")
    await t.enter()
    await t.waitFor("Two plus two")
    await t.frame("answer")
    await t.escape()
    await t.frame("closed")
  },
)
