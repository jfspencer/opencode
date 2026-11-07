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

// Error and info toasts from failed or finished actions.

scenario("toasts: renaming a session that was deleted meanwhile", { width: 100, height: 30 }, async (t) => {
  const id = await t.session({ title: "Vanishing" })
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Vanishing")
  await t.press("r", { ctrl: true })
  await waitLong(t, "Rename session")
  await t.http("DELETE", `/api/session/${id}`)
  await t.type(" again")
  await t.enter()
  await waitLong(t, "Failed to rename session")
  await t.frame("error toast")
})

scenario("toasts: deleting a session that was deleted meanwhile", { width: 100, height: 30 }, async (t) => {
  const id = await t.session({ title: "Already gone" })
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Already gone")
  await t.http("DELETE", `/api/session/${id}`)
  await t.press("d", { ctrl: true })
  await t.press("d", { ctrl: true })
  await new Promise((resolve) => setTimeout(resolve, 300))
  await t.frame("after the second ctrl+d")
})

scenario("toasts: info toast comes and goes", { width: 100, height: 30, tui: { keybinds: { "model.cycle_favorite": "f3" } } }, async (t) => {
  await t.press("F3")
  await waitLong(t, "Add a favorite model")
  await t.frame("shown")
  await waitLong(t, absent("Add a favorite model"))
  await t.frame("gone")
})

// Leave the prompt empty: a non-empty prompt is kept as a draft for the next TUI started in this process.
async function clear(t: TuiCtx) {
  await t.press("a", { super: true })
  await t.press("BACKSPACE")
}

scenario("toasts: some prompts cannot be queued", { width: 100, height: 30 }, async (t) => {
  await t.type("exit")
  await t.press("x", { ctrl: true })
  await t.enter()
  await waitLong(t, "cannot be queued")
  await t.frame("exit")
  await clear(t)
  await waitLong(t, absent("cannot be queued"))
  await t.type("!")
  await t.type("ls")
  await t.press("x", { ctrl: true })
  await t.enter()
  await waitLong(t, "cannot be queued")
  await t.frame("shell command")
  await clear(t)
  await t.escape()
})

// With every provider denied there is no model: submitting warns and offers to connect an integration.
scenario(
  "toasts: no model to send the prompt to",
  { width: 100, height: 30, config: { policies: [{ action: "provider.use", resource: "*", effect: "deny" }] } },
  async (t) => {
    await t.frame("home without a model")
    await t.type("Hello")
    await t.enter()
    await waitLong(t, "Connect an integration to send prompts")
    await t.frame("warning toast")
    await t.escape()
    await clear(t)
  },
)
