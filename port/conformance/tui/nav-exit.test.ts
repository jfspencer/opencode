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

// Exiting. Once the TUI has exited the test renderer is gone and frames can no longer be captured, so each
// scenario records whether a frame could still be taken after the key.
async function exited(t: TuiCtx, key: () => Promise<void>) {
  // Let the startup migration poll happen first, as in every other scenario.
  await new Promise((resolve) => setTimeout(resolve, 1100))
  await key().catch(() => undefined)
  await new Promise((resolve) => setTimeout(resolve, 300))
  t.note("exited", await t.frame("after").then(() => false, () => true))
}

scenario("exit: ctrl+c on an empty prompt", { width: 80, height: 20 }, async (t) => {
  await t.frame("home")
  await exited(t, () => t.press("c", { ctrl: true }))
})

scenario("exit: ctrl+d on an empty prompt", { width: 80, height: 20 }, async (t) => {
  await exited(t, () => t.press("d", { ctrl: true }))
})

scenario("exit: leader q", { width: 80, height: 20 }, async (t) => {
  await t.press("x", { ctrl: true })
  await exited(t, () => t.press("q"))
})

scenario("exit: /exit", { width: 80, height: 20 }, async (t) => {
  await t.type("/exit")
  await t.frame("typed")
  await exited(t, () => t.enter())
})

scenario("exit: /q alias", { width: 80, height: 20 }, async (t) => {
  await t.type("/q")
  await t.frame("autocomplete")
  await exited(t, () => t.enter())
})

scenario("exit: ctrl+c with text clears instead, ctrl+d deletes", { width: 80, height: 20 }, async (t) => {
  await t.type("some text")
  await t.press("a", { ctrl: true })
  await t.press("d", { ctrl: true })
  await t.frame("ctrl+d deleted a character")
  await t.press("c", { ctrl: true })
  await t.frame("ctrl+c cleared")
  await exited(t, () => t.press("c", { ctrl: true }))
})

scenario("exit: ctrl+c in a dialog closes the dialog", { width: 80, height: 20 }, async (t) => {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await exited(t, () => t.press("c", { ctrl: true }))
})

scenario("exit: from a session", { width: 80, height: 20 }, async (t) => {
  await t.session({ title: "Left behind" })
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Left behind")
  await t.enter()
  await waitLong(t, absent("Sessions"))
  await t.frame("session")
  await exited(t, () => t.press("c", { ctrl: true }))
})

for (const word of ["exit", "quit", ":q"]) {
  scenario(`exit: typing "${word}" and enter`, { width: 80, height: 20 }, async (t) => {
    await t.type(word)
    await t.frame("typed")
    await exited(t, () => t.enter())
  })
}
