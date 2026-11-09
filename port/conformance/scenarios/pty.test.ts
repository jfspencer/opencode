// Terminal sessions (`pty.*`): create, list, get, update and remove over the API, the connect-ticket checks, and a
// terminal's WebSocket (`/api/pty/:ptyID/connect`): output and replay, input, resize, exit and the close codes.
//
// Programs are fixed `sh -c` scripts that print nothing until they get input, so the output a socket replays
// when it connects is known; a script turns echo off before its first answer and exits without printing, so no
// output races the exit. A terminal's output chunks are timing: consecutive text frames are recorded as one.
// WebSocket upgrades need a real listener: those scenarios run only against a process (CONFORMANCE_IMPL).
import { type Ctx, type Frame, scenario } from "../lib/scenario"

/** PtyProtocol frames: output as text, and a binary control frame (0x00 + JSON) with the cursor after replay. */
const terminal = {
  record(frame: Frame) {
    if (frame.type === "text") return { text: frame.data }
    if (frame.data[0] === 0) return { control: JSON.parse(new TextDecoder().decode(frame.data.subarray(1))) }
    return { binary: new TextDecoder().decode(frame.data) }
  },
  merge: (item: unknown) => ((item as { text?: unknown }).text === undefined ? undefined : "text"),
}
const text = (items: readonly unknown[]) =>
  items.map((item) => (item as { text?: string }).text ?? "").join("")

async function create(ctx: Ctx, payload: Record<string, unknown>) {
  const result = await ctx.http("POST", "/api/pty", payload)
  return result.body.data.id as string
}

/** Resolves once the server has published an event of `type` for the terminal. */
const published = (ctx: Ctx, type: string, id: string) =>
  ctx.until((event) => event?.type === type && (event.data?.id ?? event.data?.info?.id) === id)

scenario("pty sessions over the API", {}, async (ctx) => {
  await ctx.http("GET", "/api/pty")
  // The default command is the configured shell, as a login shell, in the project; the title names the terminal.
  const shell = await create(ctx, {})
  const cat = await create(ctx, { command: "/bin/cat", title: "Cat", cwd: "/" })
  await ctx.http("GET", "/api/pty")
  await ctx.http("GET", `/api/pty/${cat}`)
  await ctx.http("PUT", `/api/pty/${cat}`, { title: "Renamed", size: { rows: 30, cols: 100 } })
  await ctx.http("PUT", `/api/pty/${cat}`, {})
  await ctx.http("PUT", `/api/pty/${cat}`, { size: { rows: 0, cols: 100 } })
  // A program that exits stays listed, exited with its code, until removed.
  const exits = await create(ctx, { command: "/bin/sh", args: ["-c", "exit 3"], title: "Exits" })
  await published(ctx, "pty.exited", exits)
  await ctx.http("GET", `/api/pty/${exits}`)
  await ctx.http("PUT", `/api/pty/${exits}`, { title: "Still here", size: { rows: 10, cols: 10 } })
  for (const id of [shell, cat, exits]) await ctx.http("DELETE", `/api/pty/${id}`)
  await ctx.http("GET", "/api/pty")
  const missing = "pty_00000000000000000000000000"
  await ctx.http("GET", `/api/pty/${missing}`)
  await ctx.http("PUT", `/api/pty/${missing}`, { title: "x" })
  await ctx.http("DELETE", `/api/pty/${missing}`)
  await ctx.http("GET", "/api/pty/not-a-pty")
  await ctx.http("POST", "/api/pty", { command: 42 })
})

// Browsers cannot set headers on a WebSocket, so they connect with a single-use ticket instead of the password.
// Minting one needs a header that forces a CORS preflight, and an allowed origin.
scenario("pty connect tickets", {}, async (ctx) => {
  const id = await create(ctx, { command: "/bin/cat", title: "Cat" })
  const other = await create(ctx, { command: "/bin/cat", title: "Other" })
  await ctx.http("POST", `/api/pty/${id}/connect-token`)
  await ctx.http("POST", `/api/pty/${id}/connect-token`, undefined, { "x-opencode-ticket": "yes" })
  await ctx.http("POST", `/api/pty/${id}/connect-token`, undefined, {
    "x-opencode-ticket": "1",
    origin: "https://evil.example",
  })
  await ctx.http("POST", `/api/pty/${id}/connect-token`, undefined, { "x-opencode-ticket": "1" })
  const minted = await ctx.http("POST", `/api/pty/${other}/connect-token`, undefined, { "x-opencode-ticket": "1" })
  const ticket = minted.body.data.ticket as string
  await ctx.http("POST", "/api/pty/pty_00000000000000000000000000/connect-token", undefined, {
    "x-opencode-ticket": "1",
  })
  // Refused before any upgrade: a ticket for another terminal, an unknown ticket, a disallowed origin, a
  // terminal that does not exist.
  await ctx.http("GET", `/api/pty/${id}/connect?ticket=${ticket}`)
  await ctx.http("GET", `/api/pty/${id}/connect?ticket=00000000-0000-4000-8000-000000000000`)
  await ctx.http("GET", `/api/pty/${id}/connect`, undefined, { origin: "https://evil.example" })
  await ctx.http("GET", "/api/pty/pty_00000000000000000000000000/connect")
  for (const pty of [id, other]) await ctx.http("DELETE", `/api/pty/${pty}`)
})

const SCRIPT = [
  'read a; stty -echo; printf "got %s %s %s\\n" "$a" "$TERM" "$OPENCODE_TERMINAL"; stty size',
  'read b; printf "%s\\n" "$b"; stty size',
  "read c; exit 7",
].join("; ")

scenario(
  "pty websocket input, output, replay and exit",
  { processOnly: "WebSocket upgrades need a listener" },
  async (ctx) => {
    // Through env: a shell command would run as a login shell, which reads the machine's profile.
    const id = await create(ctx, { command: "/usr/bin/env", args: ["sh", "-c", SCRIPT], title: "Script" })
    const url = `/api/pty/${id}/connect`
    // Nothing printed yet: no replay, cursor 0. Input is echoed until the script turns echo off.
    const first = await ctx.socket(url, terminal)
    await first.until((items) => items.length > 0)
    first.send("hello\r")
    await first.until((items) => text(items).endsWith("24 80\r\n"))
    await first.close()
    // Replay from the start, from the end (-1), and from a cursor inside the output.
    const replay = await ctx.socket(`${url}?cursor=0`, terminal)
    const tail = await ctx.socket(`${url}?cursor=-1`, terminal)
    const middle = await ctx.socket(`${url}?cursor=7`, terminal)
    for (const socket of [replay, tail, middle])
      await socket.until((items) => items.some((item) => "control" in (item as object)))
    // A resize reaches the program; every attached socket gets the same output.
    await ctx.http("PUT", `/api/pty/${id}`, { size: { rows: 30, cols: 100 } })
    replay.send("again\r")
    for (const socket of [replay, tail, middle]) await socket.until((items) => text(items).endsWith("30 100\r\n"))
    // The program exits without printing: every socket closes normally.
    tail.send("q\r")
    for (const socket of [replay, tail, middle]) await socket.closed
    await published(ctx, "pty.exited", id)
    await ctx.http("GET", `/api/pty/${id}`)
    // An exited terminal is still listed, but there is nothing to attach to.
    const exited = await ctx.socket(url, terminal)
    await exited.closed
    await ctx.http("DELETE", `/api/pty/${id}`)
    await ctx.http("GET", url)
  },
)

scenario(
  "pty websocket authentication and tickets",
  { processOnly: "WebSocket upgrades need a listener, and authentication exists only in a process" },
  async (ctx) => {
    const id = await create(ctx, { command: "/bin/cat", title: "Cat" })
    const url = `/api/pty/${id}/connect`
    const mint = async () =>
      (await ctx.http("POST", `/api/pty/${id}/connect-token`, undefined, { "x-opencode-ticket": "1" })).body.data
        .ticket as string
    // Without the password or a ticket the upgrade is refused.
    await ctx.socket(url, { ...terminal, auth: false })
    // A ticket stands in for the password, once.
    const ticket = await mint()
    const browser = await ctx.socket(`${url}?ticket=${ticket}`, { ...terminal, auth: false })
    await browser.until((items) => items.length > 0)
    await browser.close()
    await ctx.socket(`${url}?ticket=${ticket}`, { ...terminal, auth: false })
    // A ticket is scoped to the location that minted it.
    const scoped = await mint()
    await ctx.socket(`${url}?ticket=${scoped}`, { ...terminal, auth: false, directory: false })
    await ctx.http("DELETE", `/api/pty/${id}`)
  },
)
