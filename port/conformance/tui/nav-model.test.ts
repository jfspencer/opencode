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

const model = (name: string, reasoning: boolean) => ({
  name,
  family: name.toLowerCase().replaceAll(" ", "-"),
  package: "@opencode/ai/providers/openrouter",
  capabilities: {
    tools: true,
    input: ["text"],
    output: ["text"],
    temperature: true,
    reasoning: reasoning ? { supported: true, effort: ["low", "high"] } : { supported: false },
  },
  limit: { context: 200000, output: 32000 },
})

// Two more OpenRouter models: one without reasoning variants, one with its own variants.
const models = {
  providers: {
    openrouter: {
      models: {
        "acme/plain-1": model("Plain One", false),
        "acme/thinker-2": model("Thinker Two", true),
      },
    },
  },
}

async function modelPicker(t: TuiCtx) {
  await t.press("x", { ctrl: true })
  await t.press("m")
  await waitLong(t, "Select model")
}

scenario("model picker: list, filter and pick a model without variants", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.frame("picker")
  await t.type("plain")
  await t.frame("filtered")
  await t.enter()
  await t.frame("picked")
  await modelPicker(t)
  await t.frame("picker with a recent model")
})

scenario("model picker: a model with variants opens the variant picker", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.type("thinker")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.frame("variant picker")
  await t.arrow("down")
  await t.arrow("down")
  await t.enter()
  await t.frame("thinker on high")
  // The picked variant is remembered: picking the model again closes straight away.
  await modelPicker(t)
  await t.type("thinker")
  await t.enter()
  await t.frame("picked again")
})

scenario("model picker: no match and escape", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.type("nonexistent model")
  await t.frame("no match")
  await t.escape()
  await t.frame("closed")
})

scenario("model picker: favorites", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.type("plain")
  await t.press("f", { ctrl: true })
  await t.frame("plain marked favorite")
  await t.escape()
  await modelPicker(t)
  await t.frame("favorites section")
  await t.type("plain")
  await t.press("f", { ctrl: true })
  await t.frame("unfavorited")
})

scenario("model picker: cycle recent models with f2", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("F2")
  await t.frame("no recent models yet")
  await modelPicker(t)
  await t.type("plain")
  await t.enter()
  await modelPicker(t)
  await t.type("thinker")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.enter()
  await t.frame("thinker on default")
  await t.press("F2")
  await t.frame("f2 goes to the next recent model")
  await t.press("F2")
  await t.frame("f2 wraps")
  await t.press("F2", { shift: true })
  await t.frame("shift+f2 goes back")
})

scenario(
  "model picker: cycle favorite models",
  { width: 100, height: 30, config: models, tui: { keybinds: { "model.cycle_favorite": "f3", "model.cycle_favorite_reverse": "f4" } } },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await t.press("F3")
    await waitLong(t, "Add a favorite model")
    await t.frame("no favorites toast")
    await waitLong(t, absent("Add a favorite model"))
    await modelPicker(t)
    await t.type("plain")
    await t.press("f", { ctrl: true })
    await t.press("BACKSPACE")
    for (let i = 0; i < 5; i++) await t.press("BACKSPACE")
    await t.type("thinker")
    await t.press("f", { ctrl: true })
    await t.escape()
    await t.press("F3")
    await t.frame("first favorite")
    await t.press("F3")
    await t.frame("second favorite")
    await t.press("F4")
    await t.frame("back to the first")
  },
)

scenario("variants: ctrl+t cycles through the variants", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.frame("max")
  for (const label of ["default", "none", "low", "high", "max again"]) {
    await t.press("t", { ctrl: true })
    await t.frame(label)
  }
})

scenario("variants: /variants picks a variant", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/variants")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.frame("current variant marked")
  await t.type("lo")
  await t.frame("filtered")
  await t.enter()
  await t.frame("low")
  await t.type("/thinking")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.press("HOME")
  await t.enter()
  await t.frame("default")
})

scenario("variants: a model without variants", { width: 100, height: 30, config: models }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.type("plain")
  await t.enter()
  await t.press("t", { ctrl: true })
  await t.frame("ctrl+t does nothing")
  await t.type("/variants")
  await t.enter()
  await waitLong(t, "No variants available")
  await t.frame("no variants toast")
})

scenario("model picker: the session keeps its own model", { width: 100, height: 30, config: models, turns: [say("Plain reply.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await modelPicker(t)
  await t.type("plain")
  await t.enter()
  await t.type("Use the plain model")
  await t.enter()
  await waitLong(t, "Plain reply.")
  await t.frame("replied")
  await modelPicker(t)
  await t.type("thinker")
  await t.enter()
  await waitLong(t, "Select variant")
  await t.type("high")
  await t.enter()
  await t.frame("session switched to thinker high")
  await t.press("x", { ctrl: true })
  await t.press("n")
  await t.frame("new session starts from the last pick")
})
