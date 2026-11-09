// Persistent terminals (`persistentPty.*`, experimental): terminals of a session that live in the `opencode-pty`
// daemon (packages/process-daemon), which the server starts on first use and owns; they outlive a server restart
// only through an explicit handoff. Every implementation runs the reference's build of the daemon (lib/preload.ts),
// so what is specified here is the server's side: the routes, their errors and events, and the terminal WebSocket.
//
// Programs are fixed `sh -c` scripts (through env, so no login shell reads the machine's profile).
import { type Ctx, type Frame, type Socket, scenario } from "../lib/scenario"

const READY = 'printf "ready\\n"; read a; printf "got %s\\n" "$a"; read b'

const terminals = (session: string) => `/api/experimental/session/${session}/terminal`
const terminal = (id: string) => `/api/experimental/persistent-pty/${id}`

type Info = { readonly status?: string; readonly output?: { readonly tail?: number } }

/** Polls the terminal (unrecorded) until its info satisfies `ready`. */
async function poll(ctx: Ctx, id: string, ready: (info: Info) => boolean) {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    const response = await ctx.handler(
      new Request(`http://opencode.local${terminal(id)}`, {
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
      }),
    )
    const body = (await response.json()) as { data?: Info }
    if (body.data && ready(body.data)) return
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  throw new Error(`terminal ${id} never got ready`)
}

/** Until the terminal's output has reached `tail` bytes (the program has started and printed). */
const output = (ctx: Ctx, id: string, tail: number) => poll(ctx, id, (info) => (info.output?.tail ?? 0) >= tail)

async function create(ctx: Ctx, session: string, payload: Record<string, unknown>) {
  const result = await ctx.http("POST", terminals(session), payload)
  return result.body.data?.id as string
}

scenario("persistent terminals over the API", {}, async (ctx) => {
  const session = await ctx.session()
  // Before any terminal, the daemon is not running: nothing is listed, nothing is current.
  await ctx.http("GET", terminals(session))
  await ctx.http("GET", `${terminals(session)}/read`)
  const id = await create(ctx, session, {
    command: "/usr/bin/env",
    args: ["sh", "-c", READY],
    cwd: ctx.project,
    title: "Script",
    env: { GREETING: "hi" },
    size: { cols: 40, rows: 6 },
  })
  await output(ctx, id, 7)
  await ctx.http("GET", terminals(session))
  await ctx.http("GET", terminal(id))
  await ctx.http("GET", `${terminal(id)}/snapshot`)
  // Reading does not select a terminal; a resize does.
  await ctx.http("GET", `${terminals(session)}/read`)
  await ctx.http("PUT", terminal(id), { size: { cols: 50, rows: 8 } })
  await ctx.http("GET", `${terminals(session)}/read`)
  await ctx.http("GET", `${terminals(session)}/read?lines=2`)
  await ctx.http("GET", `${terminals(session)}/read?lines=0`)
  await ctx.http("GET", `${terminals(session)}/read?lines=70000`)
  // A title the user sets wins over the program's; null clears it.
  await ctx.http("PUT", terminal(id), { title: "Renamed" })
  await ctx.http("PUT", terminal(id), { title: null })
  await ctx.http("PUT", terminal(id), {})
  await ctx.http("DELETE", terminal(id))
  await ctx.http("GET", terminal(id))
  await ctx.http("GET", terminals(session))
  await ctx.http("GET", `${terminals(session)}/read`)
  // Without a command, the terminal runs the environment's shell, in `/` without a cwd.
  const shell = await create(ctx, session, { args: [], title: "Shell", env: {} })
  await ctx.http("DELETE", terminal(shell))
  // A program that exits while no client is attached stays listed, exited with its code, until removed.
  const exits = await create(ctx, session, {
    command: "/usr/bin/env",
    args: ["sh", "-c", "exit 7"],
    title: "Exits",
    env: {},
  })
  await poll(ctx, exits, (info) => info.status === "exited")
  await ctx.http("GET", terminals(session))
  await ctx.http("DELETE", terminal(exits))
})

scenario("persistent terminal errors", { files: { "README.md": "# r\n" } }, async (ctx) => {
  const session = await ctx.session()
  await ctx.http("POST", terminals(session), { args: [], title: "Nowhere", env: {}, cwd: `${ctx.project}/missing` })
  await ctx.http("POST", terminals(session), { args: [], title: "File", env: {}, cwd: `${ctx.project}/README.md` })
  await ctx.http("POST", terminals(session), { args: [], env: {} })
  await ctx.http("POST", terminals(session), { args: [], title: "Small", env: {}, size: { cols: 0, rows: 1 } })
  await ctx.http("GET", terminal("pty_persistent_99"))
  await ctx.http("PUT", terminal("pty_persistent_99"), { title: "x" })
  await ctx.http("DELETE", terminal("pty_persistent_99"))
  await ctx.http("GET", `${terminal("pty_persistent_99")}/snapshot`)
  await ctx.http("POST", `${terminal("pty_persistent_99")}/connect-token`, undefined, { "x-opencode-ticket": "1" })
  await ctx.http("GET", "/api/experimental/session/ses_missing/terminal")
})

// Terminals belong to a session: each session lists its own, and deleting a session removes its terminals.
scenario("persistent terminals of two sessions", {}, async (ctx) => {
  const first = await ctx.session()
  const second = await ctx.session()
  const one = await create(ctx, first, { command: "/usr/bin/env", args: ["sh", "-c", READY], title: "One", env: {} })
  const two = await create(ctx, second, { command: "/usr/bin/env", args: ["sh", "-c", READY], title: "Two", env: {} })
  await output(ctx, one, 7)
  await output(ctx, two, 7)
  await ctx.http("GET", terminals(first))
  await ctx.http("GET", terminals(second))
  await ctx.http("DELETE", `/api/session/${second}`)
  await ctx.until((event) => event?.type === "persistent-pty.removed" && event.data?.ptyID === two)
  await ctx.http("GET", terminals(second))
  await ctx.http("GET", terminal(two))
  await ctx.http("GET", terminal(one))
  await ctx.http("DELETE", terminal(one))
})

// Shutting the daemon down ends every terminal; the next terminal starts a new one. A handoff descriptor lets a
// replacement server claim the running daemon (see "persistent terminals survive a handoff").
scenario("persistent terminal daemon shutdown and handoff", {}, async (ctx) => {
  const session = await ctx.session()
  await ctx.http("POST", "/api/experimental/persistent-pty/handoff")
  await ctx.http("POST", "/api/experimental/persistent-pty/shutdown")
  await create(ctx, session, { command: "/usr/bin/env", args: ["sh", "-c", READY], title: "First", env: {} })
  await ctx.http("POST", "/api/experimental/persistent-pty/shutdown")
  await ctx.http("GET", terminals(session))
  const id = await create(ctx, session, {
    command: "/usr/bin/env",
    args: ["sh", "-c", READY],
    title: "Second",
    env: {},
  })
  await output(ctx, id, 7)
  await ctx.http("GET", terminal(id))
  await ctx.http("POST", "/api/experimental/persistent-pty/handoff")
  await ctx.http("POST", "/api/experimental/persistent-pty/shutdown")
  await ctx.http("GET", terminals(session))
})

/** The terminal socket's frames: JSON control messages as text, the terminal's output as binary. */
const stream = {
  record: (frame: Frame) =>
    frame.type === "text" ? { message: JSON.parse(frame.data) } : { output: new TextDecoder().decode(frame.data) },
  merge: (item: unknown) => ((item as { output?: unknown }).output === undefined ? undefined : "output"),
}
const printed = (items: readonly unknown[]) => items.map((item) => (item as { output?: string }).output ?? "").join("")
const message = (type: string) => (items: readonly unknown[]) =>
  items.some((item) => (item as { message?: { type?: string } }).message?.type === type)
/**
 * Until the socket has replayed and, for a controller, the daemon has announced its control: that announcement
 * follows the replay on its own schedule, so input sent earlier would be recorded on either side of it.
 */
const attached = (socket: Socket, controller = true) =>
  socket.until((items) => message("replay_complete")(items) && (!controller || message("controller_changed")(items)))

const SCRIPT = [
  'printf "ready\\n"; read a; stty -echo; printf "got %s\\n" "$a"; stty size',
  'read b; printf "%s\\n" "$b"; stty size',
  "read c; exit 5",
].join("; ")

/** A framed input message (`input_protocol=1`): type (0 control, 1 input), columns, rows, then the input. */
function framed(type: number, cols: number, rows: number, data = "") {
  const bytes = new TextEncoder().encode(data)
  const out = new Uint8Array(5 + bytes.length)
  const view = new DataView(out.buffer)
  out[0] = type
  view.setUint16(1, cols)
  view.setUint16(3, rows)
  out.set(bytes, 5)
  return out
}

scenario(
  "persistent terminal websocket: control, observers, input and exit",
  { processOnly: "WebSocket upgrades need a listener" },
  async (ctx) => {
    const session = await ctx.session()
    const id = await create(ctx, session, {
      command: "/usr/bin/env",
      args: ["sh", "-c", SCRIPT],
      cwd: ctx.project,
      title: "Script",
      env: {},
      size: { cols: 40, rows: 6 },
    })
    await output(ctx, id, 7)
    const url = `${terminal(id)}/connect`
    // The first controller replays everything; input is echoed until the script turns echo off.
    const first = await ctx.socket(`${url}?attachment_id=first`, stream)
    await attached(first)
    first.send("hello\r")
    await first.until((items) => printed(items).endsWith("6 40\r\n"))
    // An observer from an offset; it takes control when it types.
    const observer = await ctx.socket(`${url}?attachment_id=watcher&role=observer&cursor=7`, stream)
    await attached(observer, false)
    // A resize through the API, by the controller.
    await ctx.http("PUT", terminal(id), { size: { cols: 50, rows: 8 }, attachmentID: "first" })
    await first.until(message("resized"))
    await observer.until(message("resized"))
    // A framed client sends its size with every message.
    const framedClient = await ctx.socket(`${url}?attachment_id=framed&input_protocol=1&takeover=true`, stream)
    await attached(framedClient)
    framedClient.send(framed(1, 60, 10, "again\r"))
    for (const socket of [first, observer, framedClient])
      await socket.until((items) => printed(items).endsWith("10 60\r\n"))
    await ctx.http("GET", `${terminals(session)}/read`)
    // The script exits: every socket gets the exit, then closes, and the server removes the terminal.
    observer.send("q\r")
    for (const socket of [first, observer, framedClient]) await socket.closed
    await ctx.until((event) => event?.type === "persistent-pty.removed" && event.data?.ptyID === id)
    await ctx.http("GET", terminal(id))
    // A removed terminal: the upgrade happens before the daemon is asked, so the socket opens and then closes.
    const gone = await ctx.socket(url, stream)
    await gone.closed
  },
)

scenario(
  "persistent terminal websocket tickets",
  { processOnly: "WebSocket upgrades need a listener, and authentication exists only in a process" },
  async (ctx) => {
    const session = await ctx.session()
    const id = await create(ctx, session, {
      command: "/usr/bin/env",
      args: ["sh", "-c", SCRIPT],
      title: "Script",
      env: {},
    })
    await output(ctx, id, 7)
    const url = `${terminal(id)}/connect`
    await ctx.http("POST", `${terminal(id)}/connect-token`)
    await ctx.http("POST", `${terminal(id)}/connect-token`, undefined, {
      "x-opencode-ticket": "1",
      origin: "https://evil.example",
    })
    const ticket = (await ctx.http("POST", `${terminal(id)}/connect-token`, undefined, { "x-opencode-ticket": "1" }))
      .body.data.ticket as string
    await ctx.socket(url, { ...stream, auth: false })
    const browser = await ctx.socket(`${url}?ticket=${ticket}&attachment_id=browser`, { ...stream, auth: false })
    await attached(browser)
    await browser.close()
    await ctx.socket(`${url}?ticket=${ticket}`, { ...stream, auth: false })
    await ctx.http("GET", `${url}?cursor=-1`)
    await ctx.http("GET", url, undefined, { origin: "https://evil.example" })
    await ctx.http("DELETE", terminal(id))
  },
)

// A replacement server started with the handoff descriptor (OPENCODE_PTY_HANDOFF, as `opencode service restart`
// passes it) claims the running daemon: the terminal survives, with its output. Without one, a daemon stops with
// the server that owned it.
scenario(
  "persistent terminals survive a restart only through a handoff",
  { processOnly: "a restart replaces the server process" },
  async (ctx) => {
    const session = await ctx.session()
    const kept = await create(ctx, session, {
      command: "/usr/bin/env",
      args: ["sh", "-c", SCRIPT],
      cwd: ctx.project,
      title: "Kept",
      env: {},
      size: { cols: 40, rows: 6 },
    })
    await output(ctx, kept, 7)
    const handoff = (await ctx.http("POST", "/api/experimental/persistent-pty/handoff")).body.handoff
    await ctx.restart({ OPENCODE_PTY_HANDOFF: JSON.stringify(handoff) })
    await ctx.http("GET", terminals(session))
    const socket = await ctx.socket(`${terminal(kept)}/connect?attachment_id=after`, stream)
    await attached(socket)
    socket.send("hello\r")
    await socket.until((items) => printed(items).endsWith("6 40\r\n"))
    await socket.close()
    await ctx.restart()
    await ctx.http("GET", terminals(session))
    await ctx.http("GET", terminal(kept))
  },
)
