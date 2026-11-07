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

// Open sessions each in its own tab, oldest first: a new session tab (leader n), then the session list, which
// opens the picked session in the active tab.
async function openTabs(t: TuiCtx, titles = ["One", "Two", "Three"]) {
  for (const title of titles) {
    await t.session({ title })
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  await waitLong(t, "ctrl+p commands")
  for (const [index, title] of titles.entries()) {
    if (index > 0) {
      await t.press("x", { ctrl: true })
      await t.press("n")
      await waitLong(t, "Ask anything")
    }
    await t.press("x", { ctrl: true })
    await t.press("l")
    await waitLong(t, title)
    // Newest first.
    await t.press("HOME")
    for (let i = 0; i < titles.length - 1 - index; i++) await t.arrow("down")
    await t.enter()
    await waitLong(t, absent("Sessions"))
  }
}

// The active tab is told apart by color only: the rename dialog names the open session.
async function active(t: TuiCtx, label: string) {
  await t.press("r", { ctrl: true })
  await waitLong(t, "Rename session")
  await t.frame(label)
  await t.escape()
}

scenario("tabs: opening sessions adds tabs", { width: 100, height: 24 }, async (t) => {
  await openTabs(t)
  await t.frame("three tabs")
  await active(t, "the last one is active")
})

scenario("tabs: next and previous", { width: 100, height: 24 }, async (t) => {
  await openTabs(t)
  await t.press("TAB", { ctrl: true })
  await active(t, "ctrl+tab wraps to the first tab")
  await t.press("ARROW_DOWN", { meta: true })
  await active(t, "alt+down to the second")
  await t.press("ARROW_UP", { meta: true })
  await active(t, "alt+up back to the first")
  await t.press("TAB", { ctrl: true, shift: true })
  await active(t, "ctrl+shift+tab wraps to the last")
})

scenario("tabs: select by number", { width: 100, height: 24 }, async (t) => {
  await openTabs(t)
  await t.press("1", { ctrl: true })
  await active(t, "ctrl+1")
  await t.press("x", { ctrl: true })
  await t.press("2")
  await active(t, "leader 2")
  await t.press("9", { ctrl: true })
  await active(t, "ctrl+9 with three tabs")
})

scenario("tabs: close and reopen", { width: 100, height: 24 }, async (t) => {
  await openTabs(t)
  await t.press("2", { ctrl: true })
  await t.press("x", { ctrl: true })
  await t.press("w")
  await t.frame("closed the second tab")
  await t.press("t", { ctrl: true, shift: true })
  await t.frame("reopened")
  await t.press("x", { ctrl: true })
  await t.press("w")
  await t.press("x", { ctrl: true })
  await t.press("w")
  await t.press("x", { ctrl: true })
  await t.press("w")
  await t.frame("all tabs closed")
  await t.press("t", { ctrl: true, shift: true })
  await t.frame("reopen the last closed")
})

scenario("tabs: history back and forward", { width: 100, height: 24, tui: { keybinds: { "session.tab.history.back": "f7" } } }, async (t) => {
  await openTabs(t)
  await t.press("1", { ctrl: true })
  await t.press("F7")
  await active(t, "back to the previously active tab")
  await t.press("i", { ctrl: true })
  await active(t, "forward again")
})

scenario("tabs: a new session tab", { width: 100, height: 24 }, async (t) => {
  await openTabs(t, ["Existing"])
  await t.press("x", { ctrl: true })
  await t.press("n")
  await t.frame("new session tab")
  await t.press("1", { ctrl: true })
  await active(t, "back to the existing tab")
  await t.press("2", { ctrl: true })
  await t.frame("ctrl+2 with only one tab left")
})

scenario("tabs: many tabs overflow the strip", { width: 100, height: 30 }, async (t) => {
  await openTabs(t, ["First long session title", "Second long session title", "Third long session title", "Fourth"])
  await t.frame("four tabs")
  await t.resize(60, 20)
  await t.frame("overflow")
  await t.press("1", { ctrl: true })
  await active(t, "first tab")
})

scenario("tabs: numbered indicators", { width: 100, height: 24, tui: { tabs: { indicators: "numbers" } } }, async (t) => {
  await openTabs(t)
  await t.frame("numbers")
})

scenario("tabs: vertical layout", { width: 120, height: 24, tui: { tabs: { layout: "vertical" } } }, async (t) => {
  await openTabs(t)
  await t.frame("vertical")
  await t.press("1", { ctrl: true })
  await active(t, "first")
  await t.resize(70, 24)
  await t.frame("too narrow for the side bar")
})

scenario("tabs: global scope", { width: 100, height: 24, tui: { tabs: { scope: "global" } } }, async (t) => {
  await openTabs(t, ["Global one", "Global two"])
  await t.frame("global tabs")
})
