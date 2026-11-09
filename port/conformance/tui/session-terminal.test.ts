// Session view: the terminal pane (<leader>t), a persistent terminal of the session (scenarios/pty-persistent.test.ts)
// shown beside the transcript and attached over its WebSocket, which the TUI's proxy passes through to the server
// (lib/tui.ts); only a server process can upgrade one, so these scenarios run only against a process.
//
// The pane runs the server's shell (SHELL, /bin/bash in the sandbox), whose first prompt comes from the machine's
// bash configuration: the scenario sets a plain prompt and clears the screen before recording any frame.
import { tuiScenario, type TuiCtx } from "../lib/tui"

const PROCESS = "the terminal pane's WebSocket needs a server process to upgrade it"

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

const absent = (text: string) => ({ test: (frame: string) => !frame.includes(text) }) as unknown as RegExp
const leader = async (t: TuiCtx, key: string) => {
  await t.press("x", { ctrl: true })
  await t.press(key)
}

tuiScenario(
  "terminal pane: open, type, hide and show again",
  {
    width: 120,
    height: 30,
    processOnly: PROCESS,
    setup: async (ctx) => ({ sessionID: await ctx.session() }),
  },
  async (t) => {
    await waitLong(t, /╹▀/)
    await waitLong(t, absent("Finishing startup"))
    await waitLong(t, absent("Loading plugins"))
    await leader(t, "t")
    await waitLong(t, /\$/)
    await t.type("PS1='$ '; clear")
    await t.enter()
    await waitLong(t, { test: (frame: string) => !frame.includes("PS1") && /\$ /.test(frame) } as unknown as RegExp)
    await t.frame("terminal-open")
    await t.type("echo hello from the pane")
    await t.enter()
    await waitLong(t, /^.*hello from the pane.*\n.*hello from the pane/m)
    await t.frame("terminal-output")
    // Hidden, the terminal keeps running; shown again, it replays what it printed.
    await leader(t, "t")
    await waitLong(t, absent("hello from the pane"))
    await t.frame("terminal-hidden")
    await leader(t, "t")
    await waitLong(t, "hello from the pane")
    await t.frame("terminal-shown")
    await new Promise((resolve) => setTimeout(resolve, 1100))
  },
)
