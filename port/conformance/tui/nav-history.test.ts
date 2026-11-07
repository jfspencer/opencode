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

scenario(
  "history: up and down recall earlier prompts",
  { width: 100, height: 30, turns: [say("One."), say("Two.")] },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await t.type("first prompt")
    await t.enter()
    await waitLong(t, "One.")
    await t.type("second prompt")
    await t.enter()
    await waitLong(t, "Two.")
    await t.arrow("up")
    await t.frame("up: the last prompt")
    await t.arrow("up")
    await t.frame("up again: the one before")
    await t.arrow("up")
    await t.frame("up at the oldest entry")
    await t.arrow("down")
    await t.frame("down")
    await t.arrow("down")
    await t.frame("down again")
    await t.arrow("down")
    await t.frame("down a third time")
    await t.press("x", { ctrl: true })
    await t.press("n")
    await t.arrow("up")
    await t.frame("history in a new session")
    await clear(t)
  },
)

scenario("history: a draft is kept while browsing", { width: 100, height: 30, turns: [say("Done.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("sent before")
  await t.enter()
  await waitLong(t, "Done.")
  await t.type("my draft")
  await t.arrow("up")
  await t.frame("up on a typed prompt")
  await t.press("a", { ctrl: true })
  await t.arrow("up")
  await t.frame("up at the start of the prompt")
  await t.arrow("down")
  await t.frame("down restores the draft")
  await clear(t)
})

scenario("history: multi-line entries", { width: 100, height: 30, turns: [say("Ok.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("line one")
  await t.press("RETURN", { shift: true })
  await t.type("line two")
  await t.enter()
  await waitLong(t, "Ok.")
  await t.arrow("up")
  await t.frame("recalled")
  await t.arrow("up")
  await t.type("<")
  await t.frame("up moves inside the recalled entry")
  await clear(t)
})
