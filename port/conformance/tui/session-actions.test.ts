// Session view: actions on a session from the TUI (undo/redo, compaction, shell mode, queued and steered
// prompts, agent switches) and how their results render in the transcript.
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

/** Press the leader key (ctrl+x), then a key. */
async function leader(t: TuiCtx, key: string) {
  await t.press("x", { ctrl: true })
  await t.press(key)
}

const files = { "README.md": "# Demo\n" }

scenario(
  "undo the last message reverts its file changes, then redo",
  {
    git: true,
    files,
    height: 30,
    turns: [
      call("write", { path: "notes.md", content: "# Notes\n\nFirst draft.\n" }),
      say("Wrote the notes."),
      call("edit", { path: "README.md", oldString: "# Demo", newString: "# Demo project" }),
      say("Renamed the heading."),
    ],
  },
  async (t) => {
    await ask(t, "Write some notes", "Wrote the notes.")
    await ask(t, "Rename the heading", "Renamed the heading.")
    let from = t.events.length
    await leader(t, "u")
    await t.until((event) => event?.type === "session.revert.staged" || event?.type?.startsWith("session.revert"), from)
    await t.waitFor("reverted")
    await t.frame("undone-once")
    from = t.events.length
    await leader(t, "u")
    await t.until((event) => event?.type?.startsWith("session.revert"), from)
    await t.waitFor("2 messages reverted")
    await t.frame("undone-twice")
    t.note("files-after-undo", { readme: t.read("README.md") })
    from = t.events.length
    await leader(t, "r")
    await t.until((event) => event?.type?.startsWith("session.revert"), from)
    await t.waitFor("Renamed the heading.")
    await t.frame("redone")
  },
)

scenario(
  "undo with the slash command, then a new prompt discards the reverted messages",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "draft.txt", content: "draft\n" }),
      say("Drafted."),
      say("Starting over."),
    ],
  },
  async (t) => {
    await ask(t, "Draft something", "Drafted.")
    await t.type("/undo")
    await t.frame("slash-typed")
    const from = t.events.length
    await t.enter()
    await t.until((event) => event?.type?.startsWith("session.revert"), from)
    await t.waitFor("1 message reverted")
    await t.frame("undone")
    await t.press("u", { ctrl: true })
    await ask(t, "Start over instead", "Starting over.")
    await t.frame("continued")
  },
)

scenario(
  "compact the session",
  {
    turns: [
      say("The answer is 42."),
      say("## Objective\n- Answer the user's question.\n\n## Decisions\n- The answer was **42**.", { prompt: 1200, completion: 80 }),
      say("Still 42."),
    ],
  },
  async (t) => {
    await ask(t, "What is the answer?", "The answer is 42.")
    let from = t.events.length
    await leader(t, "c")
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("The answer was 42.")
    await t.frame("compacted")
    await ask(t, "And now?", "Still 42.")
    await t.frame("after-compaction")
  },
)

scenario(
  "compaction summary that ignores the template",
  {
    turns: [
      say("The answer is 42."),
      say("Here is a summary without headings.", { prompt: 1200, completion: 40 }),
      say("Still no headings.", { prompt: 1300, completion: 50 }),
    ],
  },
  async (t) => {
    await ask(t, "What is the answer?", "The answer is 42.")
    const from = t.events.length
    await t.type("/compact")
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("did not match the required template")
    await t.frame("failed")
  },
)

scenario(
  "compaction that fails",
  {
    turns: [
      say("The answer is 42."),
      { status: 400, body: { error: { message: "Context too large to summarize", code: 400 } } },
    ],
  },
  async (t) => {
    await ask(t, "What is the answer?", "The answer is 42.")
    const from = t.events.length
    await t.type("/compact")
    await t.enter()
    await t.until((event) => event?.type === "session.viewed", from, 60_000)
    await t.waitFor("Context too large to summarize")
    await t.frame("failed")
  },
)

scenario("shell mode runs a command in the session", { files, turns: [say("Ready.")] }, async (t) => {
  await ask(t, "Get ready", "Ready.")
  await t.type("!")
  await t.frame("shell-mode")
  await t.type("sleep 1; ls && echo shell-ok")
  let from = t.events.length
  await t.enter()
  // A shell command is not a model turn: its result is queued into the session for the next prompt.
  await t.until((event) => event?.type === "session.inbox.enqueued", from, 60_000)
  await t.waitFor("shell-ok")
  await t.frame("ran")
  await t.type("!")
  await t.type("sleep 1; echo failing; exit 2")
  from = t.events.length
  await t.enter()
  await t.until((event) => event?.type === "session.inbox.enqueued", from, 60_000)
  await t.waitFor("Command exited with code 2")
  await t.frame("failed")
})

{
  const gate = Promise.withResolvers<void>()
  scenario(
    "prompts queued and steered while a step runs",
    {
      turns: [
        { events: [{ text: "Working on the first request" }, { until: gate.promise }, { text: "." }] },
        say("Handled the steering message."),
        say("Handled the queued prompt."),
      ],
      height: 30,
    },
    async (t) => {
      await t.type("First request")
      await t.enter()
      await t.waitFor("Working on the first request")
      await t.type("Queued follow-up")
      await t.press("x", { ctrl: true })
      await t.press("RETURN")
      await t.waitFor("1 queued")
      await t.frame("queued")
      await t.press("x", { ctrl: true })
      await t.press("q")
      await t.waitFor("Queued prompts")
      await t.frame("queued-dialog")
      await t.escape()
      await t.type("Steer now")
      await t.enter()
      await t.frame("steered")
      const from = t.events.length
      gate.resolve()
      await t.until((event) => event?.type === "session.viewed", from, 60_000)
      await t.waitFor("Handled the queued prompt.")
      await t.frame("done")
    },
  )
}

scenario(
  "switching agent shows in the transcript",
  { turns: [say("Built."), say("Planned.")] },
  async (t) => {
    await ask(t, "Build it", "Built.")
    await t.press("TAB", { shift: true })
    await t.frame("plan-selected")
    await ask(t, "Plan it", "Planned.")
    await t.frame("switched")
  },
)
