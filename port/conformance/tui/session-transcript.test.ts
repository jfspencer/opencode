// Session view: reasoning, streaming and markdown display options, terminal sizes, and long transcripts.
import { call, say, type StreamEvent, type Turn } from "../lib/openrouter"
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

/** A turn that pauses at each gate: the scenario opens them once it has looked at the screen. */
function gated(...parts: (StreamEvent | "gate")[]) {
  const gates: PromiseWithResolvers<void>[] = []
  const events = parts.map((part) => {
    if (part !== "gate") return part
    const gate = Promise.withResolvers<void>()
    gates.push(gate)
    return { until: gate.promise }
  })
  return { turn: { events } as Turn, gates }
}

{
  const { turn, gates } = gated(
    { reasoning: "**Planning the answer**\n\nThe user wants a greeting. I will keep it short." },
    "gate",
    { text: "Hello there." },
  )
  scenario("reasoning is collapsed to its title by default", { turns: [turn] }, async (t) => {
    await t.type("Greet me")
    await t.enter()
    await t.waitFor("Thinking: Planning the answer")
    await t.frame("thinking")
    gates[0].resolve()
    await t.waitFor("Hello there.")
    await t.until((event) => event?.type === "session.viewed")
    await t.frame("thought")
  })
}

{
  const { turn, gates } = gated(
    { reasoning: "**Weighing options**\n\nFirst option: *fast*. Second option: `safe`." },
    "gate",
    { text: "Going with the safe option." },
  )
  scenario(
    "reasoning shown in full, then collapsed with a key",
    { turns: [turn], tui: { session: { thinking: "show" }, keybinds: { "session.toggle.thinking": "f5" } } },
    async (t) => {
      await t.type("Choose")
      await t.enter()
      await t.waitFor("First option")
      await t.frame("thinking-shown")
      gates[0].resolve()
      await t.waitFor("Going with the safe option.")
      await t.until((event) => event?.type === "session.viewed")
      // Shown reasoning headers carry a raw duration; collapse before recording the finished turn.
      await t.press("F5")
      await t.waitFor("+ Thought")
      await t.frame("collapsed")
    },
  )
}

{
  const first = gated({ reasoning: "**Reading the file**\n\nLet me look." }, "gate", { tool: "read", input: { path: "a.txt" } })
  const second = gated({ reasoning: "**Summarizing**\n\nIt says hello." }, "gate", { text: "The file says hello." })
  scenario(
    "reasoning across several steps is grouped",
    { files: { "a.txt": "hello\n" }, turns: [first.turn, second.turn] },
    async (t) => {
      await t.type("What does a.txt say?")
      await t.enter()
      await t.waitFor("Thinking: Reading the file")
      first.gates[0].resolve()
      await t.waitFor("Thinking: Summarizing")
      await t.frame("second-step")
      second.gates[0].resolve()
      await t.waitFor("The file says hello.")
      await t.until((event) => event?.type === "session.viewed")
      await t.frame("done")
    },
  )
}

{
  const { turn, gates } = gated(
    { text: "# Plan\n\nSteps:\n\n1. Read\n2. Write\n\n```ts\nconst answer = 4" },
    "gate",
    { text: "2\n```\n\nDone streaming." },
  )
  scenario("partial markdown while streaming", { turns: [turn] }, async (t) => {
    await t.type("Plan it")
    await t.enter()
    await t.waitFor("const answer = 4")
    await t.frame("streaming")
    gates[0].resolve()
    await t.waitFor("Done streaming.")
    await t.until((event) => event?.type === "session.viewed")
    await t.frame("streamed")
  })
}

const RELEASE_NOTES = [
  "# Release notes",
  "",
  "Some **bold**, some *italic*, some `inline code` and a [link](https://example.com/docs).",
  "",
  "## Changes",
  "",
  "- First item",
  "- Second item with `code`",
  "  - Nested item",
  "1. Numbered one",
  "2. Numbered two",
  "",
  "> A quoted remark.",
  "",
  "| Name | Value |",
  "| ---- | ----- |",
  "| alpha | 1 |",
  "| beta | 22 |",
  "",
  "```ts",
  "export function add(a: number, b: number) {",
  "  return a + b // sum",
  "}",
  "```",
  "",
  "---",
  "",
  "Done.",
].join("\n")

scenario("headings, lists, quotes, tables and code", { height: 48, turns: [say(RELEASE_NOTES)] }, async (t) => {
  await ask(t, "Write release notes", "Done.")
  await t.frame("markdown")
})

const MARKDOWN = [
  "## Summary",
  "",
  "Use **bold**, _emphasis_, `code`, ~~strike~~ and [a link](https://example.com).",
  "",
  "- [x] done task",
  "- [ ] open task",
  "",
  "```",
  "plain fenced block",
  "```",
].join("\n")

scenario("markdown shown as source", { turns: [say(MARKDOWN)], tui: { session: { markdown: "source" } } }, async (t) => {
  await ask(t, "Summarize", "plain fenced block")
  await t.frame("source")
})

scenario("markdown rendered", { turns: [say(MARKDOWN)] }, async (t) => {
  await ask(t, "Summarize", "plain fenced block")
  await t.frame("rendered")
})

const LONG =
  "This reply is deliberately long so that it has to wrap across several lines of the transcript; " +
  "it keeps going with ordinary words, a very-long-hyphenated-compound-word-that-does-not-fit, and a URL " +
  "https://example.com/a/very/long/path/that/keeps/going/and/going/until/it/must/wrap/somewhere/index.html " +
  "before it finally ends."

scenario(
  "long lines wrap on a narrow terminal",
  { width: 60, height: 20, turns: [say(LONG)], tui: { session: { tps: false } } },
  async (t) => {
    await ask(t, "Say something long", "finally ends.")
    await t.frame("narrow")
  },
)

scenario(
  "wide terminal shows the sidebar",
  { git: true, width: 160, height: 48, turns: [call("write", { path: "notes.md", content: "# Notes\n" }), say(LONG)] },
  async (t) => {
    await ask(t, "Say something long", "finally ends.")
    await t.frame("wide")
  },
)

scenario(
  "resizing reflows the transcript",
  { git: true, width: 100, height: 30, turns: [say(LONG)], tui: { session: { tps: false } } },
  async (t) => {
    await ask(t, "Say something long", "finally ends.")
    await t.resize(60, 20)
    await t.frame("narrowed")
    await t.resize(160, 40)
    await t.frame("widened")
  },
)

const numbered = (from: number, to: number) =>
  Array.from({ length: to - from + 1 }, (_, i) => `Line ${from + i} of the reply.`).join("\n\n")

scenario(
  "long transcript scrolled with page keys",
  {
    height: 24,
    turns: [say(numbered(1, 12)), say(numbered(13, 24)), say(numbered(25, 36))],
    tui: { keybinds: { "session.message.previous": "f6", "session.message.next": "f7" } },
  },
  async (t) => {
    await ask(t, "First", "Line 12 of the reply.")
    await ask(t, "Second", "Line 24 of the reply.")
    await ask(t, "Third", "Line 36 of the reply.")
    await t.frame("bottom")
    await t.press("\x1b[5~")
    await t.frame("page-up")
    await t.press("\x1b[5~")
    await t.frame("page-up-twice")
    await t.press("\x1b[6~")
    await t.frame("page-down")
    await t.press("g", { ctrl: true })
    await t.waitFor("First")
    await t.frame("first")
    await t.press("F7")
    await t.frame("next-message")
    await t.press("F6")
    await t.frame("previous-message")
    await t.press("g", { ctrl: true, meta: true })
    await t.waitFor("Line 36 of the reply.")
    await t.frame("last")
    await t.press("END", { meta: true })
    await t.frame("last-user-message")
  },
)

// The scrollbar is drawn on the same rows as the turn footer, so the footer must have a fixed width: no
// throughput, and a duration held between one and ten seconds ("1.2s").
{
  const gate = Promise.withResolvers<void>()
  scenario(
    "scrolling by lines and half pages with the scrollbar",
    {
      height: 24,
      turns: [say(numbered(1, 20)), { events: [{ text: numbered(21, 30) + "\n\n" }, { until: gate.promise }, { text: numbered(31, 40) }] }],
      tui: { session: { scrollbar: true, tps: false } },
    },
    async (t) => {
      await ask(t, "First", "Line 20 of the reply.")
      const from = t.events.length
      await t.type("Second")
      await t.enter()
      await t.waitFor("Line 30 of the reply.")
      setTimeout(gate.resolve, 1_200)
      await t.until((event) => event?.type === "session.viewed", from, 60_000)
      await t.waitFor("Line 40 of the reply.")
      await t.frame("bottom")
      await t.press("y", { ctrl: true, meta: true })
      await t.press("y", { ctrl: true, meta: true })
      await t.frame("two-lines-up")
      await t.press("u", { ctrl: true, meta: true })
      await t.frame("half-page-up")
      await t.press("d", { ctrl: true, meta: true })
      await t.frame("half-page-down")
      await t.press("e", { ctrl: true, meta: true })
      await t.frame("line-down")
    },
  )
}

scenario(
  "display settings toggled with keys",
  {
    files: { "a.txt": "a\n", "b.txt": "b\n" },
    height: 30,
    turns: [call("read", { path: "a.txt" }), call("glob", { pattern: "a.*" }), call("shell", { command: "sleep 1; echo hi" }), say("Looked around.")],
    // The scrollbar shares rows with the turn footer: keep it a fixed width (no throughput; the shell makes the
    // duration a single "N.Ns").
    tui: {
      session: { tps: false },
      keybinds: {
        "session.verbosity.cycle": "f9",
        "session.toggle.exploration_grouping": "f10",
        "session.toggle.scrollbar": "f11",
      },
    },
  },
  async (t) => {
    await ask(t, "Look around", "Looked around.")
    await t.frame("medium")
    await t.press("F9")
    await t.waitFor("Explored: 1 read, 1 search")
    await t.frame("high")
    await t.press("F9")
    await t.frame("low")
    await t.press("F9")
    await t.press("F10")
    await t.frame("ungrouped")
    await t.press("F10")
    await t.press("F11")
    await t.frame("scrollbar")
  },
)
