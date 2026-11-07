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

const options = { width: 80, height: 24 }

// Every scenario here ends with text in the prompt, which would be kept as a draft for the next TUI started in
// this process: select all and delete it (ctrl+c would exit on an empty prompt).
const editing = (name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) =>
  scenario(name, options, async (t) => {
    await body(t)
    await t.press("a", { super: true })
    await t.press("BACKSPACE")
  })

async function ready(t: TuiCtx) {
  await waitLong(t, "ctrl+p commands")
}

editing("prompt: multi-line input", options, async (t) => {
  await ready(t)
  await t.type("first line")
  await t.press("RETURN", { shift: true })
  await t.type("second line")
  await t.press("j", { ctrl: true })
  await t.type("third line")
  await t.press("RETURN", { meta: true })
  await t.type("fourth line")
  await t.press("RETURN", { ctrl: true })
  await t.type("fifth line")
  await t.frame("five lines")
  await t.press("c", { ctrl: true })
  await t.frame("ctrl+c clears the prompt")
})

editing("prompt: cursor movement", options, async (t) => {
  await ready(t)
  await t.type("alpha beta gamma")
  await t.press("a", { ctrl: true })
  await t.type("[")
  await t.press("e", { ctrl: true })
  await t.type("]")
  await t.frame("line home and end")
  await t.press("b", { meta: true })
  await t.press("b", { meta: true })
  await t.type("<")
  await t.frame("two words back")
  await t.press("f", { meta: true })
  await t.type(">")
  await t.frame("one word forward")
  await t.arrow("left")
  await t.arrow("left")
  await t.arrow("left")
  await t.type("^")
  await t.arrow("right")
  await t.type("$")
  await t.frame("left and right")
  await t.press("ARROW_LEFT", { ctrl: true })
  await t.type("{")
  await t.press("ARROW_RIGHT", { ctrl: true })
  await t.type("}")
  await t.frame("ctrl+left and ctrl+right")
  await t.press("b", { ctrl: true })
  await t.type("(")
  await t.press("f", { ctrl: true })
  await t.type(")")
  await t.frame("ctrl+b and ctrl+f")
})

editing("prompt: moving between lines", options, async (t) => {
  await ready(t)
  await t.type("top line here")
  await t.press("RETURN", { shift: true })
  await t.type("middle")
  await t.press("RETURN", { shift: true })
  await t.type("bottom line here")
  await t.arrow("up")
  await t.type("1")
  await t.arrow("up")
  await t.type("2")
  await t.frame("up twice")
  await t.arrow("down")
  await t.type("3")
  await t.frame("down")
  await t.press("HOME", { shift: true })
  await t.type("X")
  await t.frame("selection to the start replaced")
})

editing("prompt: deleting text", options, async (t) => {
  await ready(t)
  await t.type("one two three four five")
  await t.press("w", { ctrl: true })
  await t.frame("ctrl+w deletes a word")
  await t.press("BACKSPACE", { meta: true })
  await t.frame("alt+backspace deletes a word")
  await t.press("BACKSPACE")
  await t.frame("backspace")
  await t.press("a", { ctrl: true })
  await t.press("d", { meta: true })
  await t.frame("alt+d deletes the next word")
  await t.press("d", { ctrl: true })
  await t.frame("ctrl+d deletes a character")
  await t.press("DELETE")
  await t.frame("delete")
  await t.press("f", { meta: true })
  await t.press("k", { ctrl: true })
  await t.frame("ctrl+k deletes to the end")
  await t.type(" again and more")
  await t.press("b", { meta: true })
  await t.press("u", { ctrl: true })
  await t.frame("ctrl+u deletes to the start")
})

editing("prompt: delete line, undo and redo", options, async (t) => {
  await ready(t)
  await t.type("keep this")
  await t.press("RETURN", { shift: true })
  await t.type("drop this")
  await t.press("d", { ctrl: true, shift: true })
  await t.frame("line deleted")
  await t.press("-", { ctrl: true })
  await t.frame("undo")
  await t.press(".", { ctrl: true })
  await t.frame("redo")
})

editing("prompt: selection is replaced by typing", options, async (t) => {
  await ready(t)
  await t.type("hello brave new world")
  await t.press("ARROW_LEFT", { shift: true })
  await t.press("ARROW_LEFT", { shift: true })
  await t.press("ARROW_LEFT", { shift: true })
  await t.press("ARROW_LEFT", { shift: true })
  await t.press("ARROW_LEFT", { shift: true })
  await t.type("WORLD")
  await t.frame("characters selected")
  await t.press("b", { meta: true, shift: true })
  await t.press("b", { meta: true, shift: true })
  await t.type("X")
  await t.frame("words selected")
  await t.press("a", { ctrl: true, shift: true })
  await t.press("BACKSPACE")
  await t.frame("selected to line start and deleted")
})

editing("prompt: long input wraps and scrolls", { width: 60, height: 20 }, async (t) => {
  await ready(t)
  for (let i = 1; i <= 12; i++) {
    await t.type(`line ${i}`)
    await t.press("RETURN", { shift: true })
  }
  await t.type("last")
  await t.frame("the prompt grows up to a limit")
})

editing("prompt: unicode and wide characters", options, async (t) => {
  await ready(t)
  await t.type("café — naïve 日本語 ✓")
  await t.frame("typed")
  await t.press("w", { ctrl: true })
  await t.press("w", { ctrl: true })
  await t.frame("two words deleted")
})

editing("prompt: escape on a typed prompt keeps the text", options, async (t) => {
  await ready(t)
  await t.type("not lost")
  await t.escape()
  await t.frame("still there")
})
