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

// Custom agents: two more primary agents (one with its own color), a subagent and a hidden agent, neither of which
// can be selected as the main agent.
const agents = {
  agents: {
    reviewer: { description: "Reviews changes without editing", mode: "primary", color: "#ff8800" },
    writer: { description: "Writes documentation", mode: "all" },
    helper: { description: "A subagent", mode: "subagent" },
    secret: { description: "Hidden from the picker", mode: "primary", hidden: true },
  },
}

scenario("agents: shift+tab cycles build and plan", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.frame("build")
  await t.press("TAB", { shift: true })
  await t.frame("plan")
  await t.press("TAB", { shift: true })
  await t.frame("back to build")
})

scenario("agents: custom agents join the cycle", { width: 100, height: 30, config: agents }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  for (const label of ["first", "second", "third", "fourth", "fifth"]) {
    await t.press("TAB", { shift: true })
    await t.frame(label)
  }
})

scenario(
  "agents: reverse cycle with a configured key",
  { width: 100, height: 30, config: agents, tui: { keybinds: { "agent.cycle.reverse": "f6" } } },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await t.press("F6")
    await t.frame("previous wraps to the last agent")
    await t.press("F6")
    await t.frame("one more back")
  },
)

scenario("agents: agent picker", { width: 100, height: 30, config: agents }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("x", { ctrl: true })
  await t.press("a")
  await waitLong(t, "Select agent")
  await t.frame("picker")
  await t.type("rev")
  await t.frame("filtered")
  await t.enter()
  await t.frame("reviewer selected")
  await t.type("/agents")
  await t.enter()
  await waitLong(t, "Select agent")
  await t.frame("picker marks reviewer")
  await t.escape()
  await t.frame("closed")
})

scenario("agents: the prompt is sent with the selected agent", { width: 100, height: 30, config: agents, turns: [say("Planned.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("TAB", { shift: true })
  await t.type("Make a plan")
  await t.enter()
  await waitLong(t, "Planned.")
  await t.frame("plan reply")
  await t.press("TAB", { shift: true })
  await t.frame("agent switched inside the session")
})

scenario("agents: model per agent", {
  width: 100,
  height: 30,
  config: {
    agents: { plan: { model: "openrouter/deepseek/deepseek-v4.1-flash-1017#low" } },
  },
}, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.press("TAB", { shift: true })
  await t.frame("plan uses its own variant")
  await t.press("t", { ctrl: true })
  await t.frame("plan variant cycled")
  await t.press("TAB", { shift: true })
  await t.frame("build shares the variant: it is kept per model")
  await t.press("TAB", { shift: true })
  await t.frame("plan remembers its variant")
})
