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

const PAGE_UP = "\x1b[5~"
const PAGE_DOWN = "\x1b[6~"

async function palette(t: TuiCtx) {
  await waitLong(t, "ctrl+p commands")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
}

scenario("command palette: open, scroll and close", { width: 100, height: 36 }, async (t) => {
  await palette(t)
  await t.frame("open")
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.press("n", { ctrl: true })
  await t.frame("ten items down scrolls the list")
  await t.press("p", { ctrl: true })
  await t.frame("ctrl+p moves up inside the palette")
  await t.press(PAGE_DOWN)
  await t.frame("page down")
  await t.press("END")
  await t.frame("end")
  await t.press(PAGE_UP)
  await t.frame("page up")
  await t.press("HOME")
  await t.frame("home")
  await t.arrow("up")
  await t.frame("up from the first item wraps to the last")
  await t.arrow("down")
  await t.frame("down from the last item wraps to the first")
  await t.escape()
  await t.frame("closed")
})

scenario("command palette: the selected command runs on enter", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  // Suggested: Switch model, Open settings, ... — the second one opens the settings.
  await t.arrow("down")
  await t.enter()
  await waitLong(t, "Settings")
  await t.frame("second suggestion ran")
})

scenario("command palette: filter commands and settings", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("sess")
  await t.frame("sess")
  await t.type("ion")
  await t.frame("session")
  for (let i = 0; i < 7; i++) await t.press("BACKSPACE")
  await t.frame("filter erased")
  await t.type("mouse")
  await t.frame("a setting")
  await t.type("zzzz")
  await t.frame("no match")
  await t.enter()
  await t.frame("enter with no match does nothing")
  await t.escape()
  await t.frame("closed")
})

scenario("command palette: run the first match", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("variant")
  await t.frame("variant")
  // "Variant cycle" is first: max wraps around to the model's first variant.
  await t.enter()
  await t.frame("variant cycled")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("switch model variant")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.frame("variant dialog")
})

scenario("command palette: run switch theme", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("switch theme")
  await t.frame("filtered")
  await t.enter()
  await waitLong(t, "Themes")
  await t.frame("themes dialog")
  await t.escape()
  await t.frame("closed")
})

scenario("command palette: run help", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("help")
  await t.enter()
  await waitLong(t, "Help")
  await t.frame("help")
  await t.enter()
  await t.frame("closed with enter")
})

scenario("command palette: open a setting", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("color mode")
  await t.frame("filtered")
  await t.enter()
  await waitLong(t, "Settings")
  await t.frame("settings at color mode")
  await t.escape()
  await t.frame("closed")
})

scenario("command palette: ctrl+p again while open", { width: 100, height: 30 }, async (t) => {
  await palette(t)
  await t.type("stat")
  await t.frame("filtered")
  await t.escape()
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("reopened with an empty filter")
})

scenario("command palette: in a session", { width: 100, height: 30 }, async (t) => {
  const id = await t.session({ title: "Palette session" })
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Palette session")
  await t.enter()
  await waitLong(t, "Palette session")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("session palette")
  await t.type("rename")
  await t.frame("rename filtered")
  await t.enter()
  await waitLong(t, "Rename session")
  await t.frame("rename dialog")
  await t.escape()
  void id
})
