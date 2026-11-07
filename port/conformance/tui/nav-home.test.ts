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

// The home screen's layout adapts to the terminal size: the logo switches between wide, stacked and compact
// variants (and disappears on short terminals), the padding narrows below 44 columns, and the footer drops
// hints that no longer fit. (Sizes are chosen so the truncated path and placeholder don't cut through the
// sandbox's random directory names or the randomly chosen example prompt.)
for (const [width, height] of [
  [160, 50],
  [80, 24],
  [70, 11],
] as const) {
  scenario(`home screen at ${width}x${height}`, { width, height }, async (t) => {
    await waitLong(t, "max")
    await t.frame("home")
  })
}

// Below ~64 columns the example prompt would be cut mid-way (it is chosen at random), so these sizes are
// recorded with some typed text instead.
for (const [width, height] of [
  [60, 20],
  [43, 20],
  [30, 16],
  [21, 14],
] as const) {
  scenario(`narrow home screen at ${width}x${height}`, { width, height }, async (t) => {
    await waitLong(t, "max")
    await t.type("hello")
    await t.frame("home")
    // A non-empty prompt is kept as a draft for the next TUI in this process; leave it empty.
    await t.press("c", { ctrl: true })
  })
}

scenario("home screen follows terminal resizes", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p")
  await t.frame("100x30")
  await t.resize(20, 10)
  await t.frame("20x10")
  await t.resize(120, 36)
  await t.frame("120x36")
})

scenario("typed prompt text survives a resize", { width: 100, height: 30 }, async (t) => {
  await t.type("A prompt long enough that it has to wrap once the terminal becomes narrow enough")
  await t.frame("wide")
  await t.resize(44, 20)
  await t.frame("narrow")
  await t.press("c", { ctrl: true })
})
