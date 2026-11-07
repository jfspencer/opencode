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

// TUI keybinding configuration (the tui.json "keybinds" and "leader" settings).

scenario("keybinds: a remapped command palette key", { width: 100, height: 30, tui: { keybinds: { "command.palette.show": "ctrl+k" } } }, async (t) => {
  await waitLong(t, "ctrl+k commands")
  await t.frame("footer shows the new key")
  await t.press("p", { ctrl: true })
  await t.frame("ctrl+p does nothing")
  await t.press("k", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("ctrl+k opens the palette")
  await t.type("help")
  await t.enter()
  await waitLong(t, "Help")
  await t.frame("help names the new key")
})

scenario("keybinds: a custom leader key", { width: 100, height: 30, tui: { keybinds: { leader: "ctrl+g" } } }, async (t) => {
  await t.press("x", { ctrl: true })
  await t.press("m")
  await t.frame("old leader does nothing")
  await t.press("g", { ctrl: true })
  await t.press("m")
  await waitLong(t, "Select model")
  await t.frame("new leader opens the model picker")
  await t.escape()
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("switch model")
  await t.frame("palette shows the leader binding")
  // Leave the prompt empty (the "m" typed after the old leader): it would be kept as a draft.
  await t.escape()
  await t.press("a", { super: true })
  await t.press("BACKSPACE")
})

scenario("keybinds: the leader times out", { width: 100, height: 30, tui: { leader: { timeout: 250 } } }, async (t) => {
  await t.press("x", { ctrl: true })
  await new Promise((resolve) => setTimeout(resolve, 600))
  await t.press("m")
  await t.frame("the key after the timeout is typed")
  await t.press("a", { super: true })
  await t.press("BACKSPACE")
})

scenario("keybinds: disabled and multiple bindings", {
  width: 100,
  height: 30,
  tui: { keybinds: { "agent.cycle": "none", "agent.list": ["f9", { key: "f10" }], "variant.cycle": false } },
}, async (t) => {
  await t.press("TAB", { shift: true })
  await t.frame("shift+tab does nothing")
  await t.press("t", { ctrl: true })
  await t.frame("ctrl+t does nothing")
  await t.press("F9")
  await waitLong(t, "Select agent")
  await t.frame("f9 opens the agent picker")
  await t.escape()
  await t.press("F10")
  await waitLong(t, "Select agent")
  await t.frame("so does f10")
})

scenario("keybinds: bindings for commands without a default key", {
  width: 100,
  height: 30,
  tui: { keybinds: { "help.show": "f1", "opencode.settings": "f3", "theme.switch": "f4", "session.list": "f5", "variant.list": "f6" } },
}, async (t) => {
  for (const [key, ready] of [
    ["F1", "Help"],
    ["F3", "Settings"],
    ["F4", "Themes"],
    ["F5", "Sessions"],
    ["F6", "Select variant"],
  ] as const) {
    await t.press(key)
    await waitLong(t, ready)
    await t.frame(key)
    await t.escape()
  }
})

scenario("keybinds: shortcuts shown in the palette follow the configuration", {
  width: 100,
  height: 40,
  tui: { keybinds: { "model.list": "ctrl+shift+m", "session.new": "f6,ctrl+x n", "opencode.status": "none" } },
}, async (t) => {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("palette")
  await t.type("stat")
  await t.frame("status without a key")
})
