import { say } from "../lib/openrouter"
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

const lines = (count: number) => Array.from({ length: count }, (_, i) => `pasted line ${i + 1}`).join("\n")

scenario("paste: short text is inserted as typed", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("before ")
  await t.paste("a short paste")
  await t.frame("inserted")
  await t.paste("\nsecond line")
  await t.frame("two lines are still inserted")
  await clear(t)
})

scenario("paste: large text becomes a placeholder", { width: 100, height: 30, turns: [say("Got it.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("Summarize: ")
  await t.paste(lines(12))
  await t.frame("placeholder")
  await t.paste("x".repeat(200))
  await t.frame("long single line")
  await t.enter()
  await waitLong(t, "Got it.")
  await t.frame("sent: the model gets the full text")
})

scenario("paste: pasting the same text again expands the placeholder", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.paste(lines(4))
  await t.frame("placeholder")
  await t.paste(lines(4))
  await t.frame("expanded")
  await clear(t)
})

scenario("paste: full mode inserts large text", { width: 100, height: 30, tui: { prompt: { paste: "full" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.paste(lines(5))
  await t.frame("full text")
  await clear(t)
})

scenario("paste: control characters and escape sequences are dropped", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.paste("red \x1b[31mtext\x1b[0m and\ttab\r\nwindows line")
  await t.frame("cleaned")
  await clear(t)
})

scenario("paste: toggling the paste summary from the palette", { width: 100, height: 30, tui: { keybinds: { "app.toggle.paste_summary": "f8" } } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("F8")
  await t.paste(lines(3))
  await t.frame("full after toggling")
  await clear(t)
  await t.press("F8")
  await t.paste(lines(3))
  await t.frame("summarized again")
  await clear(t)
})

scenario(
  "paste: a file path attaches the file",
  {
    width: 100,
    height: 30,
    files: { "pic.png": "not really a png", "doc.pdf": "%PDF-1.4 tiny", "icon.svg": "<svg xmlns='http://www.w3.org/2000/svg'/>" },
    turns: [say("Seen.")],
  },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await t.type("Look ")
    await t.paste(`${t.project}/pic.png`)
    await t.frame("image attached")
    await t.paste(`${t.project}/doc.pdf`)
    await t.frame("pdf attached")
    await t.paste(`${t.project}/icon.svg`)
    await t.frame("svg inlined")
    await t.enter()
    await waitLong(t, "Seen.")
    await t.frame("sent")
  },
)

scenario("paste: several file paths at once", { width: 100, height: 30, files: { "a.png": "a", "b.png": "b" } }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.paste(`${t.project}/a.png ${t.project}/b.png`)
  await t.frame("two images")
  await clear(t)
})

scenario("paste: a path that doesn't exist is text", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.paste(`${t.project}/missing.png`)
  await t.frame("text")
  await clear(t)
})

scenario("paste: viewing image attachments", { width: 100, height: 30, files: { "a.png": "not a png", "b.png": "nor this" } }, async (t) => {
  await t.paste(`${t.project}/a.png ${t.project}/b.png`)
  await t.press("x", { ctrl: true })
  await t.press("i")
  await waitLong(t, "Image 1 of 2")
  await t.frame("first image")
  await t.arrow("right")
  await t.frame("second image")
  await t.arrow("right")
  await t.frame("right wraps")
  await t.escape()
  await clear(t)
})
