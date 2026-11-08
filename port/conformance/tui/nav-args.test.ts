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

const models = {
  providers: {
    openrouter: {
      models: {
        "acme/plain-1": {
          name: "Plain One",
          family: "plain",
          package: "@opencode/ai/providers/openrouter",
          capabilities: { tools: true, input: ["text"], output: ["text"], temperature: true, reasoning: { supported: false } },
          limit: { context: 200000, output: 32000 },
        },
      },
    },
  },
}

const MISSING = "ses_0000000000conformanceMissing"

// The TUI takes model, agent and fork arguments and a session ID that may not exist, but the CLI's root command has
// no --model, --agent or --fork flag, and passes a missing --session as the ID of the first new session (as in "the
// first new session takes the given ID" below): those scenarios document TUI code no command reaches.
const UNREACHABLE = "no CLI flag reaches this TUI argument (the root command has no --model, --agent or --fork, and a missing --session becomes a new session's ID)"

// Arguments are applied at startup, and their warning or error toasts (3-5s) can be over before a slow startup
// (plugins loading) has finished: these scenarios record the screen once any such toast is gone. --agent and --model
// are applied once the agents and models have loaded, which the prompt footer shows (agent · model).
// (Home is shown only after a missing session's toast has been raised.)
async function afterStartupToasts(t: TuiCtx) {
  await waitLong(t, "Ask anything")
  await waitLong(t, /(Build|Plan) · (DeepSeek|Plain One)/)
  for (const text of ["Invalid model format", "Agent not found", "Session not found"]) await waitLong(t, absent(text))
}
const NEW = "ses_0000000000conformanceNewOne1"

scenario("args: --model picks the starting model", { width: 100, height: 30, config: models, args: { model: "openrouter/acme/plain-1" }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await waitLong(t, "Build · Plain One")
  await t.frame("home")
  await t.press("x", { ctrl: true })
  await t.press("m")
  await waitLong(t, "Select model")
  await t.frame("it is a recent model")
})

scenario("args: --model with an invalid format", { width: 100, height: 30, args: { model: "not-a-model" }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("default model")
})

scenario("args: --model that doesn't exist", { width: 100, height: 30, args: { model: "openrouter/acme/unknown" }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("default model")
})

scenario("args: --agent picks the starting agent", { width: 100, height: 30, args: { agent: "plan" }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await waitLong(t, "Plan · DeepSeek")
  await t.frame("home")
})

scenario("args: --agent that doesn't exist", { width: 100, height: 30, args: { agent: "ghost" }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("default agent")
})

// Whether the TUI fetches the missing session's terminals before giving up races in the TUI itself.
scenario("args: --session that doesn't exist", { width: 100, height: 30, args: { sessionID: MISSING }, requests: false, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("home")
})

scenario("args: --session --fork of a session that doesn't exist", { width: 100, height: 30, args: { sessionID: MISSING, fork: true }, inProcessOnly: UNREACHABLE }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("home")
})

scenario("args: --continue without any session", { width: 100, height: 30, args: { continue: true } }, async (t) => {
  await afterStartupToasts(t)
  await t.frame("home")
})

scenario(
  "args: --continue opens the latest session",
  {
    width: 100,
    height: 30,
    args: { continue: true },
    turns: [say("Earlier reply.")],
    setup: async (ctx) => {
      const id = await ctx.session()
      await ctx.prompt(id, "Earlier question")
    },
  },
  async (t) => {
    await waitLong(t, "Earlier reply.")
    await t.frame("session")
  },
)

scenario(
  "args: the first new session takes the given ID",
  { width: 100, height: 30, args: { newSessionID: NEW }, turns: [say("Created.")] },
  async (t) => {
    await waitLong(t, "ctrl+p commands")
    await t.type("Hello")
    await t.enter()
    await waitLong(t, "Created.")
    await t.frame("session")
    await t.http("GET", `/api/session/${NEW}`)
  },
)
