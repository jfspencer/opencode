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
async function clear(t: TuiCtx) {
  await t.press("a", { super: true })
  await t.press("BACKSPACE")
}

async function command(t: TuiCtx, text: string) {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type(text)
  await t.enter()
}

scenario("dialogs: status", { width: 100, height: 30 }, async (t) => {
  await t.press("x", { ctrl: true })
  await t.press("s")
  await waitLong(t, "Status")
  await t.frame("status")
  await t.escape()
  await t.frame("closed")
})

scenario("dialogs: open recent sessions and projects", { width: 100, height: 30 }, async (t) => {
  await t.session({ title: "Recent one" })
  await new Promise((resolve) => setTimeout(resolve, 20))
  await t.session({ title: "Recent two" })
  await t.press("o", { ctrl: true })
  await waitLong(t, "Recent two")
  await t.frame("open menu")
  await t.press("o", { ctrl: true })
  await t.frame("ctrl+o again keeps it open")
  await t.type("one")
  await t.frame("filtered")
  await t.enter()
  await waitLong(t, absent("Projects"))
  await t.frame("opened")
})

// Narrow, so the footer's path and branch are cut before the sandbox's random directory name; with some text
// in the prompt, so its randomly chosen example isn't shown cut either.
scenario("dialogs: open menu project worktrees", { width: 60, height: 30, git: true, files: { "a.txt": "a\n" } }, async (t) => {
  await t.type("x")
  await t.press("o", { ctrl: true })
  await waitLong(t, "Search")
  await t.frame("open menu")
  await t.press("END")
  await t.arrow("right")
  await t.frame("right on a project")
  await t.arrow("left")
  await t.frame("left returns")
  await t.escape()
  await clear(t)
})

scenario("dialogs: skills", { width: 100, height: 30 }, async (t) => {
  await t.type("/skills")
  await t.enter()
  await waitLong(t, "Skills")
  await t.frame("skills")
  await t.type("report")
  await t.frame("filtered")
  await t.enter()
  await t.frame("skill inserted in the prompt")
  await clear(t)
})

scenario("dialogs: stash a prompt and bring it back", { width: 100, height: 30 }, async (t) => {
  await t.type("first draft")
  await command(t, "stash prompt")
  await t.frame("stashed")
  await t.type("second draft")
  await command(t, "stash prompt")
  await command(t, "stash list")
  await waitLong(t, "Stash")
  await t.frame("stash list")
  await t.press("d", { ctrl: true })
  await t.frame("delete asks for confirmation")
  await t.press("d", { ctrl: true })
  await t.frame("deleted")
  await t.enter()
  await t.frame("restored")
  await clear(t)
  await command(t, "stash pop")
  await t.frame("no stash pop once the stash is empty")
  await t.escape()
  await clear(t)
})

scenario("dialogs: workspaces", { width: 60, height: 30, git: true, files: { "a.txt": "a\n" } }, async (t) => {
  await t.type("x")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("manage workspaces")
  await t.enter()
  await waitLong(t, "Worktrees")
  await t.frame("worktrees")
  await t.escape()
  await clear(t)
})

scenario("dialogs: workspaces outside git", { width: 100, height: 30 }, async (t) => {
  await t.type("/worktrees")
  await t.enter()
  await new Promise((resolve) => setTimeout(resolve, 300))
  await t.frame("worktrees")
  await t.escape()
})

scenario("dialogs: reload configuration", { width: 100, height: 30 }, async (t) => {
  await t.type("/reload")
  await t.enter()
  await waitLong(t, "Configuration reloaded")
  await t.frame("reloaded")
})

scenario("dialogs: escape closes a dialog opened from another", { width: 100, height: 30 }, async (t) => {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("open settings")
  await t.enter()
  await waitLong(t, "Settings")
  await t.escape()
  await t.frame("all closed")
})

scenario("dialogs: dialogs adapt to small terminals", { width: 50, height: 16 }, async (t) => {
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("palette")
  await t.escape()
  await t.press("x", { ctrl: true })
  await t.press("m")
  await waitLong(t, "Select model")
  await t.frame("model picker")
  await t.escape()
})

scenario("dialogs: session commands from the palette", { width: 100, height: 30, turns: [say("A reply to copy.")] }, async (t) => {
  await t.type("Say something")
  await t.enter()
  await waitLong(t, "A reply to copy.")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.frame("palette in a session with messages")
  await t.escape()
})

scenario("dialogs: connect an integration", { width: 100, height: 30 }, async (t) => {
  await t.type("/connect")
  await t.enter()
  await waitLong(t, "Connect an integration")
  await t.frame("integrations")
  await t.type("openrouter")
  await t.frame("filtered")
  await t.enter()
  await new Promise((resolve) => setTimeout(resolve, 300))
  await t.frame("openrouter")
  await t.escape()
})

scenario("dialogs: new worktree for the next session", { width: 60, height: 30, git: true, files: { "a.txt": "a\n" } }, async (t) => {
  await t.type("x")
  await t.press("p", { ctrl: true })
  await waitLong(t, "Commands")
  await t.type("manage workspaces")
  await t.enter()
  await waitLong(t, "Worktrees")
  await t.press("a", { ctrl: true })
  await waitLong(t, "Name worktree")
  await t.frame("name dialog")
  await t.type("feature-one")
  await t.enter()
  await waitLong(t, "(new worktree)")
  await t.frame("the next session starts in a new worktree")
  await clear(t)
})
