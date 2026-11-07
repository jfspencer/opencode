import { say } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Shared by the nav-* scenario files (each file is self-contained).
//
// - Startup: the TUI shows a "Loading plugins…"/"Finishing startup…" indicator when plugins take over 500ms to
//   load (e.g. under coverage), and keeps it up for 3s. Every scenario first waits until the prompt is drawn
//   (plugins are ready by then) and the indicator, if it was shown, is gone.
// - The TUI polls the data-migration status once, 1s after startup: every scenario runs past that point so the
//   recorded endpoint list doesn't depend on how fast it ran.
// - waitFor gives up after a fixed number of frames, which slow steps (model replies, server round trips, a
//   loaded machine) can exceed: waits retry until a deadline instead.
const scenario = (name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) =>
  tuiScenario(name, options, async (t) => {
    await waitLong(t, /╹▀/)
    await waitLong(t, absent("Finishing startup"))
    await waitLong(t, absent("Loading plugins"))
    // A non-empty prompt is kept as a draft when a TUI closes and restored by the next one started in this
    // process (another scenario, or another file in the same run): start from an empty prompt.
    await t.press("a", { super: true })
    await t.press("BACKSPACE")
    await body(t)
    await new Promise((resolve) => setTimeout(resolve, 1100))
  })

async function waitLong(t: TuiCtx, match: string | RegExp, timeout = 20_000) {
  const deadline = Date.now() + timeout
  for (;;) {
    try {
      return await t.waitFor(match)
    } catch (error) {
      if (Date.now() > deadline) throw error
    }
  }
}

// A matcher for frames that no longer contain the text (toasts and indicators disappear on timers).
const absent = (text: string) => ({ test: (frame: string) => !frame.includes(text) }) as unknown as RegExp

// Leave the prompt empty: a non-empty prompt is kept as a draft for the next TUI started in this process.
// (Select all and delete: ctrl+c would exit on an empty prompt, and only hide an open autocomplete.)
async function clear(t: TuiCtx) {
  await t.press("a", { super: true })
  await t.press("BACKSPACE")
}

const files = {
  "README.md": "# Demo\n",
  "src/main.ts": "export const main = () => 1\n",
  "src/util/strings.ts": "export const upper = (s: string) => s.toUpperCase()\n",
  "docs/guide.md": "Guide\nline 2\nline 3\nline 4\n",
}

const options = { width: 100, height: 30, files }

scenario("mentions: @ lists skills and subagents, then files once there is a query", options, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("@")
  await waitLong(t, "@explore")
  await t.frame("skills and agents")
  await t.type("str")
  await waitLong(t, "strings.ts")
  await t.frame("filtered")
  await t.type("zzzz")
  await t.frame("no match")
  await t.escape()
  await clear(t)
})

scenario("mentions: pick a file with enter and submit", { ...options, turns: [say("Read it.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("Look at @mai")
  await waitLong(t, "src/main.ts")
  await t.enter()
  await t.frame("file inserted")
  await t.type("please")
  await t.enter()
  await waitLong(t, "Read it.")
  await t.frame("sent with the file")
})

scenario("mentions: tab completes a directory", options, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("@src/ut")
  await waitLong(t, "src/util")
  await t.frame("directory match")
  await t.tab()
  await t.frame("tab")
  await clear(t)
})

scenario("mentions: a line range", options, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("@guide.md#2-3")
  await waitLong(t, "guide.md#2-3")
  await t.frame("range")
  await t.enter()
  await t.frame("inserted with the range")
  await clear(t)
})

scenario("mentions: subagents", { ...options, config: { agents: { helper: { description: "Helps", mode: "subagent" } } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("@hel")
  await waitLong(t, "@helper")
  await t.frame("subagent listed")
  await t.enter()
  await t.frame("agent inserted")
  await clear(t)
})

scenario("mentions: deleting a mention removes it whole", options, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("@READ")
  await waitLong(t, "README.md")
  await t.enter()
  await t.frame("inserted")
  await t.press("BACKSPACE")
  await t.press("BACKSPACE")
  await t.frame("backspaced into the mention")
  await clear(t)
})

scenario("mentions: an @ in the middle of a word is not a mention", options, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("mail me@exam")
  await t.frame("no list")
  await clear(t)
})
