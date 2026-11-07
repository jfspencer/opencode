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

async function settings(t: TuiCtx) {
  await waitLong(t, "ctrl+p commands")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("open settings")
  await t.enter()
  await waitLong(t, "Settings")
}

const down = async (t: TuiCtx, n: number) => {
  for (let i = 0; i < n; i++) await t.arrow("down")
}

scenario("settings: browse every setting", { width: 100, height: 60 }, async (t) => {
  await settings(t)
  await t.frame("top")
  await down(t, 12)
  await t.frame("session settings")
  await down(t, 12)
  await t.frame("tabs and diffs")
  await down(t, 12)
  await t.frame("input and alerts")
  await down(t, 3)
  await t.frame("down from the last setting wraps to the top")
  await t.press("END")
  await t.frame("end")
})

scenario("settings: cycle values with left, right and enter", { width: 100, height: 40 }, async (t) => {
  await settings(t)
  // Color mode: system → dark → light, then back.
  await t.arrow("down")
  await t.arrow("right")
  await t.frame("color mode dark")
  await t.arrow("right")
  await t.frame("color mode light")
  await t.arrow("left")
  await t.arrow("left")
  await t.frame("color mode back to system")
  await t.arrow("left")
  await t.frame("left from the first value wraps")
  await t.arrow("right")
  await t.frame("system again")
  // Sidebar (a two-value setting) cycles on enter.
  await down(t, 2)
  await t.enter()
  await t.frame("sidebar hidden with enter")
  await t.enter()
  await t.frame("sidebar auto again")
})

scenario("settings: numeric settings step and clamp", { width: 100, height: 40 }, async (t) => {
  await settings(t)
  await t.type("scroll speed")
  await t.frame("scroll speed")
  await t.arrow("right")
  await t.arrow("right")
  await t.frame("scroll speed raised twice")
  for (let i = 0; i < 14; i++) await t.arrow("left")
  await t.frame("scroll speed clamped at the minimum")
  for (let i = 0; i < 14; i++) await t.press("BACKSPACE")
  await t.type("volume")
  await t.arrow("right")
  await t.frame("volume")
  for (let i = 0; i < 8; i++) await t.arrow("right")
  await t.frame("volume clamped at 100%")
  for (let i = 0; i < 6; i++) await t.press("BACKSPACE")
  await t.type("leader timeout")
  await t.arrow("left")
  await t.frame("leader timeout")
})

scenario("settings: search by keyword", { width: 100, height: 40 }, async (t) => {
  await settings(t)
  await t.type("clipboard")
  await t.frame("clipboard")
  for (let i = 0; i < 9; i++) await t.press("BACKSPACE")
  await t.type("dark mode")
  await t.frame("dark mode")
  for (let i = 0; i < 9; i++) await t.press("BACKSPACE")
  await t.type("nothing like this")
  await t.frame("no match")
})

scenario("settings: theme cycles through the theme list", { width: 100, height: 40 }, async (t) => {
  await settings(t)
  await t.arrow("right")
  await t.frame("next theme")
  await t.arrow("left")
  await t.arrow("left")
  await t.frame("previous theme")
  await t.escape()
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("switch theme")
  await t.enter()
  await waitLong(t, "Themes")
  await t.frame("theme list marks the chosen theme")
})

scenario("settings: values from the TUI config", {
  width: 100,
  height: 60,
  tui: {
    theme: { name: "nord", mode: "light" },
    session: { sidebar: "hide", scrollbar: true, submit_scroll: "keep", thinking: "show", markdown: "source", grouping: "none", verbosity: "high", tps: false, new_location: "inherit", permissions: "autoaccept" },
    tabs: { mode: "off", scope: "global", layout: "vertical", indicators: "numbers" },
    diffs: { view: "split", wrap: "none", tree: false, single: true },
    scroll: { speed: 1.5, acceleration: true },
    mouse: false,
    prompt: { editor: false, paste: "full", image_preview: true },
    leader: { timeout: 500 },
    attention: { notifications: true, sound: true, volume: 0.7 },
    highlights: false,
    terminal: { title: false, copy: "manual" },
  },
}, async (t) => {
  await settings(t)
  await t.frame("top")
  await down(t, 20)
  await t.frame("middle")
  await t.press("END")
  await t.frame("end")
})

scenario("settings: changing tabs mode shows the tab strip in a session", { width: 100, height: 30 }, async (t) => {
  await t.session({ title: "Tabbed session" })
  await waitLong(t, "ctrl+p commands")
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Tabbed session")
  await t.enter()
  await waitLong(t, "Tabbed session")
  await t.frame("session with tabs on auto")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("open settings")
  await t.enter()
  await waitLong(t, "Settings")
  await down(t, 14)
  await t.frame("tabs mode")
  await t.arrow("left")
  await t.frame("tabs mode on")
  await t.arrow("left")
  await t.frame("tabs mode off")
  await t.escape()
  await t.frame("session without tabs")
})

// Toggle commands without a default key, bound here to function keys; the settings show their effect.
scenario(
  "settings: toggle commands",
  {
    width: 100,
    height: 60,
    tui: {
      keybinds: {
        "app.toggle.file_context": "f5",
        "app.toggle.diffwrap": "f6",
        "terminal.title.toggle": "f7",
        "app.toggle.paste_summary": "f8",
      },
    },
  },
  async (t) => {
    await t.press("F5")
    await t.press("F6")
    await t.press("F7")
    await t.press("F8")
    await settings(t)
    await t.press("END")
    await t.frame("toggled once")
    await t.escape()
    await t.press("F5")
    await t.press("F6")
    await t.press("F7")
    await t.press("F8")
    await settings(t)
    await t.press("END")
    await t.frame("toggled back")
  },
)
