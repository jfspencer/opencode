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

// The "new-designs" TUI experiment (tui.json experimental["new-designs"]) restyles dialogs, the model picker
// and the settings.
const tui = { experimental: { "new-designs": true } }

scenario("new designs: home and palette", { width: 100, height: 30, tui }, async (t) => {
  await t.frame("home")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Search commands")
  await t.frame("palette")
  await t.type("theme")
  await t.frame("filtered")
})

scenario("new designs: model picker and variants", { width: 100, height: 30, tui }, async (t) => {
  await t.press("x", { ctrl: true })
  await t.press("m")
  await waitLong(t, "Search models")
  await t.frame("model picker")
  await t.press("f", { ctrl: true })
  await t.frame("favorited")
  await t.enter()
  await t.frame("picked")
  await t.type("/variants")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.frame("variant picker")
})

scenario("new designs: settings", { width: 100, height: 40, tui }, async (t) => {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Search commands")
  await t.type("open settings")
  await t.enter()
  await waitLong(t, "Search settings")
  await t.frame("settings")
  await t.arrow("down")
  await t.arrow("right")
  await t.frame("color mode changed")
  await t.press("HOME")
  await t.enter()
  await waitLong(t, "Search themes")
  await t.frame("theme picker from settings")
  await t.type("nord")
  await t.enter()
  await waitLong(t, "Search settings")
  await t.frame("back in settings")
})

scenario("new designs: agents, sessions and help", { width: 100, height: 30, tui }, async (t) => {
  await t.session({ title: "A session" })
  await t.press("x", { ctrl: true })
  await t.press("a")
  await waitLong(t, "Search agents")
  await t.frame("agents")
  await t.escape()
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "A session")
  await t.frame("sessions")
  await t.escape()
  await t.type("/help")
  await t.enter()
  await waitLong(t, "Help")
  await t.frame("help")
})
