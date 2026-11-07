// Session view: subagents (child sessions) as shown in the parent transcript, the subagent picker, and the
// child session view itself.
import { call, say, type Responder, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed.
function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  const titled = Promise.withResolvers<void>()
  const [first, ...rest] = options.turns ?? []
  // Without a recorded event stream there is no order to fix (and no rename event to wait for).
  const turns: Turn[] | undefined =
    first && "events" in first && options.events !== false
      ? [{ ...first, events: [{ until: titled.promise }, ...first.events] }, ...rest]
      : options.turns
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

const files = { "docs/guide.md": "# Guide\n\nHow to use it.\n", "src/index.ts": "export const main = () => 1\n" }

// A foreground subagent runs to completion inside the parent's step, so the scripted turns are consumed in
// a fixed order: parent, child steps, parent.
const foreground: Turn[] = [
  call("subagent", { agent: "explore", description: "Survey the docs", prompt: "List and read the docs" }),
  call("glob", { pattern: "docs/**" }),
  call("read", { path: "docs/guide.md" }),
  say("The docs contain a single guide."),
  say("The subagent found one guide."),
]

scenario("foreground subagent in the parent transcript", { files, turns: foreground }, async (t) => {
  await ask(t, "Survey the docs with a subagent", "The subagent found one guide.")
  await t.frame("parent")
})

scenario("subagent picker and the child session view", { files, height: 30, turns: foreground }, async (t) => {
  await ask(t, "Survey the docs with a subagent", "The subagent found one guide.")
  await t.arrow("down")
  await t.waitFor("No active subagents")
  await t.frame("picker")
  await t.press("a", { ctrl: true })
  await t.waitFor(/Survey the docs\s*\n(.|\n)*Survey the docs/)
  await t.frame("picker-inactive")
  await t.enter()
  await t.waitFor("List and read the docs")
  await t.frame("child")
  await t.arrow("up")
  await t.waitFor("The subagent found one guide.")
  await t.frame("back-in-parent")
})

scenario(
  "subagent with an explicit model and a continuation",
  {
    files,
    turns: [
      call("subagent", {
        agent: "general",
        description: "Count files",
        prompt: "Count the files",
        model: "openrouter/deepseek/deepseek-v4.1-flash-1017#low",
      }),
      say("There are two files."),
      say("Counted."),
    ],
  },
  async (t) => {
    await ask(t, "Count files with a subagent", "Counted.")
    const child = t.events.find((event: any) => event?.type === "session.created" && event.data.parentID) as any
    t.provider.push(
      call("subagent", {
        agent: "general",
        description: "Recount",
        prompt: "Count again",
        sessionID: child?.data.sessionID ?? "ses_missing",
      }),
      say("Still two files."),
      say("Recounted."),
    )
    await ask(t, "Ask the same subagent again", "Recounted.")
    await t.frame("continued")
  },
)

scenario(
  "subagent with an unknown agent fails",
  {
    files,
    turns: [
      call("subagent", { agent: "nonexistent", description: "Do magic", prompt: "Magic" }),
      say("That agent does not exist."),
    ],
  },
  async (t) => {
    await ask(t, "Use a missing agent", "That agent does not exist.")
    await t.frame("failed")
  },
)

// A background subagent runs concurrently with its parent; answer its requests by content, not by order.
// Its requests share the parent's recording group, so their arrival order must be fixed too: a slow shell
// call in the same step keeps the parent's next request well behind the child's first one. The two calls of
// that step then run in parallel, so their events interleave at random: this scenario records no event stream
// and waits on the screen instead.
const childPrompt = "Index the documentation"
const isChild: Responder = (request) => {
  const messages = (request.body as { messages?: { role: string; content: unknown }[] } | undefined)?.messages ?? []
  const first = messages.find((message) => message.role === "user")
  return request.kind === "chat" && typeof first?.content === "string" && first.content.endsWith(`\n${childPrompt}`)
    ? childTurns.shift()
    : undefined
}
const childGate = Promise.withResolvers<void>()
const childTurns: Turn[] = [{ events: [{ until: childGate.promise }, { text: "Indexed one page." }] }]

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
  "background subagent reports back when it finishes",
  {
    files,
    height: 30,
    events: false,
    responders: [isChild],
    turns: [
      {
        events: [
          { tool: "subagent", input: { agent: "general", description: "Index docs", prompt: childPrompt, background: true } },
          { tool: "shell", input: { command: "sleep 2" } },
        ],
      },
      say("Started indexing in the background."),
      say("Indexing finished."),
    ],
  },
  async (t) => {
    await t.type("Index the docs in the background")
    await t.enter()
    await waitLong(t, /Started indexing in the background\.\s+Build · DeepSeek/)
    await t.frame("running")
    childGate.resolve()
    await waitLong(t, /Indexing finished\.\s+Build · DeepSeek/)
    await t.frame("finished")
  },
)
