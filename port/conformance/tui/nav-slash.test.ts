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

const commands = {
  commands: {
    greet: { template: "Say hello to $ARGUMENTS.", description: "Greets someone by name" },
    standup: { template: "Summarize what changed today.", description: "Daily summary", agent: "plan" },
  },
}

scenario("slash: the command list", { width: 100, height: 40, config: commands }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/")
  await t.frame("all commands")
  await t.escape()
  await t.frame("hidden with escape")
  await clear(t)
  // The selection is shown by color only: run the sixth entry (/continue) to show where it is.
  await t.type("/")
  for (let i = 0; i < 6; i++) await t.arrow("down")
  await t.arrow("up")
  await t.enter()
  await waitLong(t, "Sessions")
  await t.frame("ran the sixth command")
  await t.escape()
  await t.type("/")
  await t.press("p", { ctrl: true })
  await t.enter()
  await t.frame("up from the first wraps to the last command")
  await t.escape()
  await clear(t)
})

scenario("slash: filtering", { width: 100, height: 30, config: commands }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/se")
  await t.frame("se")
  await t.type("ss")
  await t.frame("sess")
  await t.type("zz")
  await t.frame("no match hides the list")
  await t.press("BACKSPACE")
  await t.press("BACKSPACE")
  await t.frame("matches again")
  await clear(t)
})

scenario("slash: tab completes the selected command", { width: 100, height: 30, config: commands }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/gre")
  await t.tab()
  await t.frame("completed")
  await clear(t)
})

scenario("slash: built-in commands run on enter", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  for (const [command, ready] of [
    ["/status", "Status"],
    ["/models", "Select model"],
    ["/agents", "Select agent"],
    ["/help", "Help"],
    ["/settings", "Settings"],
    ["/effort", "Select variant"],
  ] as const) {
    await t.type(command)
    await t.enter()
    await waitLong(t, ready)
    await t.frame(command)
    await t.escape()
  }
})

scenario("slash: a custom command with arguments", { width: 100, height: 30, config: commands, turns: [say("Hello, Bob!")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/greet")
  await t.enter()
  await t.frame("selected: the name is completed for arguments")
  await t.type("Bob")
  await t.enter()
  await waitLong(t, "Hello, Bob!")
  await t.frame("ran")
})

// Run in an existing session: on a brand-new session, whether the switch notice names the previous agent races
// session creation in TS itself ("Switched agent to Plan" vs "Switched agent from Build to Plan").
scenario("slash: a custom command with its own agent", { width: 100, height: 30, config: commands, turns: [say("Ready."), say("Nothing changed.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("Start")
  await t.enter()
  await waitLong(t, "Ready.")
  await t.type("/standup")
  await t.enter()
  await t.enter()
  await waitLong(t, "Nothing changed.")
  await t.frame("ran with the plan agent")
})

scenario("slash: an unknown command is sent as a prompt", { width: 100, height: 30, turns: [say("Not a command.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("/notacommand some text")
  await t.frame("typed")
  await t.enter()
  await waitLong(t, "Not a command.")
  await t.frame("sent")
})

scenario("slash: commands in a session", { width: 100, height: 30, turns: [say("Hi.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("Hi")
  await t.enter()
  await waitLong(t, "tok/s")
  await t.type("/")
  await t.frame("session commands")
  await t.type("ren")
  await t.enter()
  await t.frame("/rename takes an argument")
  await clear(t)
})

scenario("slash: /cd argument completion and errors", { width: 100, height: 30, files: { "sub/dir/file.txt": "x\n" } }, async (t) => {
  await t.type("/cd")
  await t.enter()
  await t.frame("cd completed for an argument: directories are offered")
  await t.escape()
  await t.enter()
  await waitLong(t, "Directory is required")
  await t.frame("no directory")
  await waitLong(t, absent("Directory is required"))
  await t.type("/cd su")
  await t.frame("directory completion")
  await t.tab()
  await t.frame("completed sub/")
  await clear(t)
  await t.type("/cd /does/not/exist")
  await t.escape()
  await t.enter()
  await waitLong(t, "Failed to change directory")
  await t.frame("failure toast")
  await clear(t)
})
