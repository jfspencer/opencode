// Background subagents and background jobs as seen through the session API: child sessions, the job list (with
// descendants), a parent that goes idle while its child still works, completion notices delivered back to the parent,
// and slash commands that delegate to a subagent.
import { rmSync } from "node:fs"
import path from "node:path"
import { call, say, type Responder, type Turn } from "../lib/openrouter"
import { scenario as baseScenario, type Ctx } from "../lib/scenario"

// The live event subscription is only in place once the server says so; waiting for that keeps the first events of
// a scenario (such as its first session.created) from racing the subscription. Several reads boot the project's
// location without waiting for its plugins, which then announce themselves in the background; finishing every
// scenario with the project's location fully loaded (through a read that waits for plugins, made outside the
// transcript) keeps the recorded global events the same on every run.
const scenario: typeof baseScenario = (name, options, body) =>
  baseScenario(name, options, async (ctx) => {
    if (options.events === false) return body(ctx)
    await ctx.until((event) => event?.type === "server.connected")
    await body(ctx)
    await ctx.handler(
      new Request("http://opencode.local/api/skill", {
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
      }),
    )
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  })

const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/** Answers requests whose first user message contains `marker` (a child session's task) with `turn`. */
function child(marker: string, turn: () => Turn): Responder {
  return (request) => {
    if (request.kind !== "chat") return undefined
    const body = request.body as { messages?: Array<{ role: string; content: unknown }> }
    const first = body.messages?.find((message) => message.role === "user")
    return JSON.stringify(first?.content ?? "").includes(marker) ? turn() : undefined
  }
}

const childGate = gate()

scenario(
  "a background subagent reports back to its parent",
  {
    turns: [
      call("subagent", {
        agent: "general",
        description: "Survey the files",
        prompt: "CHILD-TASK: list what is here",
        background: true,
      }),
      say("The child is working; waiting for it."),
      say("The child reported back."),
    ],
    responders: [
      child("CHILD-TASK", () => ({
        events: [{ text: "Child is looking" }, { until: childGate.promise }, { text: ", found nothing." }],
      })),
    ],
  },
  async (ctx) => {
    const parent = await ctx.session({ title: "parent" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${parent}/prompt`, { text: "Start a background child" })
    // The child streams its first words, then holds until released.
    const delta = await ctx.until(
      (event) => event?.type === "session.text.delta" && event?.data?.delta === "Child is looking",
      from,
    )
    const childID = delta.data.sessionID
    // The parent finishes its own turn while the child is still running.
    await ctx.wait(parent)
    await ctx.http("GET", `/api/session?parentID=${parent}`)
    await ctx.http("GET", "/api/session/active")
    await ctx.http("GET", `/api/experimental/job?sessionID=${parent}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${parent}&descendants=true`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${parent}&descendants=true&kind=subagent&status=running`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${childID}&descendants=true`)
    await ctx.http("GET", "/api/experimental/job?kind=shell")
    const notified = ctx.events.length
    childGate.open()
    await ctx.until(
      (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === childID,
      notified,
    )
    const notice = await ctx.until(
      (event) =>
        event?.type === "session.inbox.enqueued" &&
        event?.data?.sessionID === parent &&
        event?.data?.item?.type === "synthetic",
      notified,
    )
    await ctx.until(
      (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === parent,
      ctx.events.indexOf(notice),
    )
    await ctx.wait(parent)
    await ctx.wait(childID)
    await messages(ctx, parent)
    await messages(ctx, childID)
    await ctx.http("GET", `/api/experimental/job?sessionID=${parent}&descendants=true`)
  },
)

scenario(
  "a slash command delegates to a subagent",
  {
    config: {
      commands: { delegate: { template: "SUBTASK-CMD: $ARGUMENTS", description: "Delegated work", subagent: true } },
    },
    turns: [say("Acknowledged the delegated result.")],
    responders: [child("SUBTASK-CMD", () => say("Delegated work is done."))],
  },
  async (ctx) => {
    const parent = await ctx.session({ title: "delegating parent" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${parent}/command`, { name: "delegate", text: "check the docs" })
    const notice = await ctx.until(
      (event) =>
        event?.type === "session.inbox.enqueued" &&
        event?.data?.sessionID === parent &&
        event?.data?.item?.type === "synthetic",
      from,
    )
    await ctx.until(
      (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === parent,
      ctx.events.indexOf(notice),
    )
    await ctx.wait(parent)
    const children = await ctx.http("GET", `/api/session?parentID=${parent}`)
    await messages(ctx, parent)
    await messages(ctx, children.body.data[0].id)
  },
)

const parentGate = gate()

scenario(
  "a background subagent whose turn fails",
  {
    turns: [
      call("subagent", {
        agent: "general",
        description: "Doomed task",
        prompt: "FAILING-CHILD: try",
        background: true,
      }),
      // The parent's next step holds, before its first output, until the child's failure has been reported, so
      // the report always lands while the parent is busy and is picked up at its next step boundary.
      { events: [{ until: parentGate.promise }, { text: "Started the child." }] },
      say("The child failed; noted."),
    ],
    responders: [
      child("FAILING-CHILD", () => ({
        status: 400,
        body: { error: { message: "child request rejected" } },
        headers: { "x-should-retry": "false" },
      })),
    ],
  },
  async (ctx) => {
    const parent = await ctx.session({ title: "parent of a failing child" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${parent}/prompt`, { text: "Start a child that fails" })
    await ctx.until(
      (event) =>
        event?.type === "session.inbox.enqueued" &&
        event?.data?.sessionID === parent &&
        String(event?.data?.item?.payload?.text).startsWith("<subagent"),
      from,
    )
    parentGate.open()
    await ctx.settle(parent)
    await messages(ctx, parent)
  },
)

const allowShell = [{ action: "shell", resource: "*", effect: "allow" }]
const waitFor = (flag: string) => `while [ ! -f ${flag} ]; do sleep 0.05; done; echo ${flag}`

scenario(
  "background jobs of a child session listed from its ancestors",
  {
    files: { "a.txt": "a\n" },
    turns: [
      call("shell", { command: waitFor("one.flag"), description: "First wait", background: true }),
      call("shell", { command: waitFor("two.flag"), description: "Second wait", background: true }),
      say("Both commands are running."),
      say("A command finished."),
      say("The other command finished."),
    ],
  },
  async (ctx) => {
    const root = await ctx.session({ title: "root", permissions: allowShell })
    const child = (await ctx.http("POST", "/api/session", { parentID: root, title: "child" })).body.data.id
    await ctx.prompt(child, "Start two background commands")
    await ctx.http("GET", `/api/experimental/job?sessionID=${root}&descendants=true`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${root}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${child}&descendants=true&status=running`)
    for (const flag of ["one.flag", "two.flag"]) {
      const from = ctx.events.length
      ctx.write(flag, "")
      const notice = await ctx.until(
        (event) =>
          event?.type === "session.inbox.enqueued" &&
          event?.data?.sessionID === child &&
          event?.data?.item?.type === "synthetic",
        from,
      )
      await ctx.until(
        (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === child,
        ctx.events.indexOf(notice),
      )
      await ctx.wait(child)
    }
    await ctx.http("GET", `/api/experimental/job?sessionID=${root}&descendants=true`)
    await messages(ctx, child)
    rmSync(path.join(ctx.project, "one.flag"))
    rmSync(path.join(ctx.project, "two.flag"))
  },
)
