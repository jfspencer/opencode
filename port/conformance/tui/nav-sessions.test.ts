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

// Sessions listed newest first: create them a few milliseconds apart so the order is fixed.
async function sessions(t: TuiCtx, ...titles: string[]) {
  const ids: string[] = []
  for (const title of titles) {
    ids.push(await t.session({ title }))
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  return ids
}

async function sessionList(t: TuiCtx) {
  await t.press("x", { ctrl: true })
  await t.press("l")
  await waitLong(t, "Sessions")
}

scenario("sessions: empty session list", { width: 100, height: 30 }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "No sessions")
  await t.frame("empty")
})

scenario("sessions: list, filter and open a session", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Alpha work", "Beta work", "Gamma notes")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Alpha work")
  await t.frame("list")
  await t.type("work")
  await waitLong(t, absent("Gamma notes"))
  await t.frame("filtered")
  await t.arrow("down")
  await t.enter()
  await waitLong(t, "Alpha work")
  await t.frame("opened the second match")
})

scenario("sessions: /sessions and its aliases", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Only session")
  await waitLong(t, "ctrl+p commands")
  await t.type("/resume")
  await t.frame("alias in the autocomplete")
  await t.enter()
  await waitLong(t, "Only session")
  await t.frame("list")
  await t.enter()
  await waitLong(t, "Only session")
  await t.frame("opened")
})

scenario("sessions: switch between sessions", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "First session", "Second session")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "First session")
  await t.enter()
  await t.frame("second session open")
  await sessionList(t)
  await t.frame("list marks the open session")
  await t.arrow("down")
  await t.enter()
  await t.frame("first session open")
})

scenario("sessions: rename from the list", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Old title")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Old title")
  await t.press("r", { ctrl: true })
  await waitLong(t, "Rename session")
  await t.frame("rename dialog")
  for (let i = 0; i < 9; i++) await t.press("BACKSPACE")
  await t.type("New title")
  await t.frame("edited")
  await t.enter()
  await waitLong(t, absent("Rename session"))
  await t.frame("renamed")
  await sessionList(t)
  await waitLong(t, "New title")
  await t.frame("list shows the new title")
})

scenario("sessions: rename with an empty title is ignored, escape cancels", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Keep me")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Keep me")
  await t.press("r", { ctrl: true })
  await waitLong(t, "Rename session")
  for (let i = 0; i < 7; i++) await t.press("BACKSPACE")
  await t.type("   ")
  await t.enter()
  await t.frame("still open")
  await t.escape()
  await t.frame("cancelled")
})

scenario("sessions: rename the open session with ctrl+r", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Current one")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Current one")
  await t.enter()
  await waitLong(t, "Current one")
  await t.press("r", { ctrl: true })
  await waitLong(t, "Rename session")
  await t.frame("rename dialog")
  await t.type(" renamed")
  await t.enter()
  await waitLong(t, "Current one renamed")
  await t.frame("renamed in place")
})

scenario("sessions: delete from the list needs a confirmation", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Doomed", "Survivor")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Doomed")
  await t.arrow("down")
  await t.press("d", { ctrl: true })
  await t.frame("confirm")
  await t.arrow("up")
  await t.frame("moving away cancels")
  await t.arrow("down")
  await t.press("d", { ctrl: true })
  await t.press("d", { ctrl: true })
  await waitLong(t, absent("Doomed"))
  await t.frame("deleted")
  await t.escape()
  await sessionList(t)
  await t.frame("reopened list")
})

for (const mode of ["auto", "off"] as const) {
  scenario(`sessions: deleting the open session returns home (tabs ${mode})`, { width: 100, height: 30, tui: { tabs: { mode } } }, async (t) => {
    const [id] = await sessions(t, "Open and deleted")
    await waitLong(t, "ctrl+p commands")
    await sessionList(t)
    await waitLong(t, "Open and deleted")
    await t.enter()
    await waitLong(t, absent("Sessions"))
    await t.frame("open")
    await t.http("DELETE", `/api/session/${id}`)
    await waitLong(t, "Ask anything")
    await t.frame("home")
  })
}

scenario("sessions: new session from a session", { width: 100, height: 30, turns: [say("First answer.")] }, async (t) => {
  await waitLong(t, "ctrl+p commands")
  await t.type("Hello")
  await t.enter()
  await waitLong(t, "First answer.")
  await t.press("x", { ctrl: true })
  await t.press("n")
  await t.frame("new session via leader n")
  await t.type("/new")
  await t.frame("slash new")
  await t.enter()
  await t.frame("still home")
})

scenario("sessions: /clear closes the tab and starts over", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "To clear")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "To clear")
  await t.enter()
  await waitLong(t, "To clear")
  await t.type("/clear")
  await t.enter()
  await t.frame("cleared")
})

scenario("sessions: all projects toggle", { width: 100, height: 30, files: { "sub/readme.md": "sub\n" } }, async (t) => {
  await sessions(t, "Root session")
  await t.session({ title: "Sub session", location: { directory: `${t.project}/sub` } })
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Root session")
  await t.frame("current directory")
  await t.press("a", { ctrl: true })
  await waitLong(t, "Sub session")
  await t.frame("all projects")
  await t.press("a", { ctrl: true })
  await waitLong(t, absent("Sub session"))
  await t.frame("current directory again")
})

scenario("sessions: search that matches nothing", { width: 100, height: 30 }, async (t) => {
  await sessions(t, "Something")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Something")
  await t.type("zzz")
  await waitLong(t, "No sessions found")
  await t.frame("no match")
})

scenario("sessions: pinning and quick switch without tabs", { width: 100, height: 30, tui: { tabs: { mode: "off" } } }, async (t) => {
  await sessions(t, "Pin one", "Pin two", "Unpinned")
  await waitLong(t, "ctrl+p commands")
  await sessionList(t)
  await waitLong(t, "Pin one")
  await t.frame("list without tabs")
  for (const title of ["Pin two", "Pin one"]) {
    await t.type(title)
    await waitLong(t, absent("Unpinned"))
    await t.press("f", { ctrl: true })
    for (let i = 0; i < title.length; i++) await t.press("BACKSPACE")
    await waitLong(t, "Unpinned")
    await t.frame(`pinned ${title}`)
  }
  await t.escape()
  // Without tabs the session view shows no title: the rename dialog shows which session is open.
  const open = async (label: string) => {
    await t.press("r", { ctrl: true })
    await waitLong(t, "Rename session")
    await t.frame(label)
    await t.escape()
  }
  await t.press("x", { ctrl: true })
  await t.press("2")
  await waitLong(t, absent("Ask anything"))
  await open("quick slot 2 opened")
  await t.press("x", { ctrl: true })
  await t.press("1")
  await open("quick slot 1 opened")
  await t.press("x", { ctrl: true })
  await t.press("3")
  await open("empty slot changes nothing")
  await sessionList(t)
  await t.press("f", { ctrl: true })
  await t.frame("unpinned the open session")
})
