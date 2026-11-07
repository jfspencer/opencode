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

scenario("shell: ! enters shell mode, backspace leaves it", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("!")
  await t.frame("shell mode")
  await t.press("BACKSPACE")
  await t.frame("normal mode")
  await t.type("!")
  await t.escape()
  await t.frame("escape leaves shell mode")
})

scenario("shell: run a command", { width: 100, height: 30, files: { "hello.txt": "hello from a file\n" } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("!")
  // Slow enough that the TUI always sees it running (and reads its output while it runs).
  await t.type("sleep 0.5; cat hello.txt")
  await t.frame("typed")
  await t.enter()
  await waitLong(t, "hello from a file")
  await t.frame("ran")
})

scenario("shell: a failing command", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("!")
  await t.type("sleep 0.5; echo oops >&2; exit 3")
  await t.enter()
  await waitLong(t, "exited with code 3")
  await t.frame("failed")
})

scenario("shell: ! in the middle of a prompt is text", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("wow")
  await t.type("!")
  await t.frame("text")
  await clear(t)
})
