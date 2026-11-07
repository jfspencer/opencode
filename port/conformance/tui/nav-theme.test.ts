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

// Frames are characters only, so the active theme is read back from the settings dialog.
async function command(t: TuiCtx, text: string, ready: string) {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type(text)
  await t.enter()
  await waitLong(t, ready)
}

async function themeSetting(t: TuiCtx, label: string) {
  await command(t, "open settings", "Settings")
  await t.frame(label)
  await t.escape()
}

scenario("themes: browse, filter and pick a theme", { width: 100, height: 40 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await command(t, "switch theme", "Themes")
  await t.frame("list")
  await t.arrow("down")
  await t.arrow("down")
  await t.frame("moved two down")
  await t.press("HOME")
  await t.frame("first theme")
  await t.type("nord")
  await t.frame("filtered")
  await t.enter()
  await t.frame("closed after picking")
  await themeSetting(t, "settings show the picked theme")
})

scenario("themes: escape restores the previous theme", { width: 100, height: 40 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await command(t, "switch theme", "Themes")
  await t.arrow("down")
  await t.arrow("down")
  await t.arrow("down")
  await t.escape()
  await themeSetting(t, "still opencode")
  await command(t, "switch theme", "Themes")
  await t.type("gruv")
  await t.frame("filter previews the first match")
  for (let i = 0; i < 4; i++) await t.press("BACKSPACE")
  await t.frame("empty filter")
  await t.escape()
  await themeSetting(t, "opencode again")
})

scenario("themes: /themes slash command", { width: 100, height: 40 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/themes")
  await t.frame("typed")
  await t.enter()
  await waitLong(t, "Themes")
  await t.type("tokyo")
  await t.enter()
  await themeSetting(t, "tokyonight")
})

scenario("themes: configured theme and light mode", { width: 100, height: 40, tui: { theme: { name: "catppuccin", mode: "light" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.frame("home")
  await themeSetting(t, "configured values")
  await command(t, "switch theme", "Themes")
  await t.frame("current theme marked")
})

scenario("themes: an unknown configured theme falls back to opencode", { width: 100, height: 40, tui: { theme: { name: "does-not-exist" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.frame("home")
  await themeSetting(t, "configured name")
  await command(t, "switch theme", "Themes")
  await t.frame("theme list")
})

scenario("themes: system theme", { width: 100, height: 40, tui: { theme: { name: "system" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.frame("home")
  await themeSetting(t, "system")
})

scenario("themes: theme mode lock and dark/light switch commands", { width: 100, height: 40, tui: { keybinds: { "theme.switch_mode": "f5", "theme.mode.lock": "f6" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("F5")
  await themeSetting(t, "switched to light")
  await t.press("F5")
  await themeSetting(t, "switched back to dark")
  await t.press("F6")
  await themeSetting(t, "unlocked to system")
  await t.press("F6")
  await themeSetting(t, "locked again")
})

scenario(
  "themes: custom themes from the project",
  {
    width: 100,
    height: 40,
    files: {
      ".opencode/themes/broken.json": JSON.stringify({ base: { text: { base: 42 } } }),
      ".opencode/themes/notatheme.json": JSON.stringify({ hello: "world" }),
    },
  },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await command(t, "switch theme", "Themes")
    await t.type("bro")
    await t.frame("custom theme listed")
    await t.enter()
    await waitLong(t, "Failed to load theme")
    await t.frame("error toast")
    await themeSetting(t, "theme after the failure")
  },
)
