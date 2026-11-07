// Prompt admission and delivery through the public API: resume:false admit-only, idempotent message IDs,
// steering and queueing into a busy session, inbox list/cancel/update, interrupt, wait, active, and
// synthetic input.
import { say, type Turn } from "../lib/openrouter"
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

const UNKNOWN_MSG = "msg_0000000000000000000000unknown"
const id = (suffix: string) => `msg_00000000000000conformance${suffix}`

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/** A turn that streams some text, then holds the response open until the gate opens. */
const held = (promise: Promise<void>, text = "Working"): Turn => ({
  events: [{ text }, { until: promise }, { text: " done." }],
})

/** Send a prompt without settling, then wait until the model is streaming its reply. */
async function startHeld(ctx: Ctx, sessionID: string, text: string, payload: Record<string, unknown> = {}) {
  const from = ctx.events.length
  const result = await ctx.http("POST", `/api/session/${sessionID}/prompt`, { text, ...payload })
  await ctx.until((event) => event?.type === "session.text.delta" && event?.data?.sessionID === sessionID, from)
  return result
}

const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)
const inbox = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/inbox`)

scenario(
  "prompt without resuming only admits the input",
  { turns: [say("Both steers read."), say("Queued prompt read.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "admit only" })
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "First, parked", resume: false })
    await ctx.http("POST", `/api/session/${session}/prompt`, {
      text: "Second, parked",
      resume: false,
      delivery: "queue",
    })
    await ctx.wait(session)
    await inbox(ctx, session)
    await messages(ctx, session)
    await ctx.http("GET", "/api/session/active")
    // The next resuming prompt starts execution: the parked steer and the new steer are delivered together,
    // and the queued prompt waits for a later turn.
    await ctx.prompt(session, "Third, resumes")
    await inbox(ctx, session)
    await messages(ctx, session)
  },
)

scenario("prompt message IDs are idempotent", { turns: [say("Handled the first prompt.")] }, async (ctx) => {
  const session = await ctx.session({ title: "idempotent" })
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("A"), text: "original", resume: false })
  // Re-admitting a pending ID returns the pending input without replacing it.
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("A"), text: "replacement", resume: false })
  await inbox(ctx, session)
  await ctx.prompt(session, "ignored text", { id: id("A") })
  await messages(ctx, session)
  // Re-admitting a delivered ID returns the delivered message as input.
  await ctx.http("POST", `/api/session/${session}/prompt`, {
    id: id("A"),
    text: "late retry",
    delivery: "queue",
    resume: false,
  })
  // The same ID as another kind of input conflicts.
  await ctx.http("POST", `/api/session/${session}/synthetic`, { id: id("A"), text: "synthetic", resume: false })
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("A") })
  const assistant = (await ctx.http("GET", `/api/session/${session}/message?type=assistant`)).body.data[0].id
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: assistant, text: "clash", resume: false })
  // An ID owned by another session conflicts too.
  const other = await ctx.session({ title: "other" })
  await ctx.http("POST", `/api/session/${other}/prompt`, { id: id("A"), text: "elsewhere", resume: false })
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: "not-a-message-id", text: "x" })
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "x", delivery: "later" })
  await ctx.http("POST", `/api/session/${session}/prompt`, {})
  // A pending prompt's ID cannot be reused as a compaction request either.
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("B"), text: "parked", resume: false })
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("B") })
  await ctx.http("POST", `/api/session/${session}/synthetic`, { id: id("B"), text: "clash", resume: false })
  await inbox(ctx, session)
})

scenario("steer a prompt into a busy session", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise), say("Also handled the steer."))
  const session = await ctx.session({ title: "steer" })
  await startHeld(ctx, session, "Start the long task")
  await ctx.http("GET", "/api/session/active")
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Also check the tests" })
  await inbox(ctx, session)
  g.open()
  await ctx.settle(session)
  await inbox(ctx, session)
  await messages(ctx, session)
  await ctx.http("GET", "/api/session/active")
})

scenario("queue a prompt behind the running turn", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise), say("Queued prompt handled."))
  const session = await ctx.session({ title: "queue" })
  await startHeld(ctx, session, "First turn")
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Next turn", delivery: "queue" })
  await inbox(ctx, session)
  g.open()
  await ctx.settle(session)
  await messages(ctx, session)
})

scenario("change and cancel pending inbox items", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise), say("Handled what was steered."), say("Handled the queued one."))
  const session = await ctx.session({ title: "inbox" })
  await startHeld(ctx, session, "Busy turn")
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("Q1"), text: "queued one", delivery: "queue" })
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("Q2"), text: "queued two", delivery: "queue" })
  await ctx.http("POST", `/api/session/${session}/synthetic`, { id: id("S1"), text: "synthetic steer" })
  await inbox(ctx, session)
  // queue -> steer, steer -> queue, and a repeated change that no longer applies.
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("Q1")}`, { delivery: "steer" })
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("S1")}`, { delivery: "queue" })
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("S1")}`, { delivery: "queue" })
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("Q1")}`, { delivery: "steer" })
  await inbox(ctx, session)
  // Cancel, cancel again (a no-op), then try to change the cancelled item.
  await ctx.http("DELETE", `/api/session/${session}/inbox/${id("Q2")}`)
  await ctx.http("DELETE", `/api/session/${session}/inbox/${id("Q2")}`)
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("Q2")}`, { delivery: "steer" })
  await ctx.http("DELETE", `/api/session/${session}/inbox/${UNKNOWN_MSG}`)
  await ctx.http("PATCH", `/api/session/${session}/inbox/${UNKNOWN_MSG}`, { delivery: "queue" })
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("Q1")}`, { delivery: "later" })
  await ctx.http("PATCH", `/api/session/${session}/inbox/not-a-message`, { delivery: "steer" })
  await inbox(ctx, session)
  g.open()
  await ctx.settle(session)
  await inbox(ctx, session)
  await messages(ctx, session)
  // Delivered input can no longer be cancelled or changed.
  await ctx.http("DELETE", `/api/session/${session}/inbox/${id("Q1")}`)
  await ctx.http("PATCH", `/api/session/${session}/inbox/${id("Q1")}`, { delivery: "queue" })
})

scenario("interrupt a running turn", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise))
  const session = await ctx.session({ title: "interrupt" })
  await ctx.http("POST", `/api/session/${session}/interrupt`)
  await startHeld(ctx, session, "Start something long")
  await ctx.http("POST", `/api/session/${session}/interrupt`)
  await ctx.wait(session)
  g.open()
  await ctx.http("POST", `/api/session/${session}/interrupt`)
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
  await ctx.http("POST", `/api/session/${session}/interrupt?resume=maybe`)
})

scenario("interrupt and resume pending steering input", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise), say("Picked up the steer after the interrupt."))
  const session = await ctx.session({ title: "interrupt resume" })
  await startHeld(ctx, session, "Long task")
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Queued for later", delivery: "queue" })
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Change of plan" })
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${session}/interrupt?resume=true`)
  await ctx.until(
    (event) => event?.type === "session.execution.interrupted" && event?.data?.sessionID === session,
    from,
  )
  g.open()
  await ctx.settle(session)
  await inbox(ctx, session)
  await messages(ctx, session)
})

scenario("interrupt with resume leaves queued prompts parked", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise))
  const session = await ctx.session({ title: "interrupt parked" })
  await startHeld(ctx, session, "Long task")
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Queued for later", delivery: "queue" })
  await ctx.http("POST", `/api/session/${session}/interrupt?resume=true`)
  await ctx.wait(session)
  g.open()
  await inbox(ctx, session)
  await ctx.http("GET", `/api/session/${session}`)
})

scenario(
  "synthetic input",
  { turns: [say("Noted the synthetic context."), say("Noted the queued context.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "synthetic" })
    await ctx.http("POST", `/api/session/${session}/synthetic`, {
      id: id("S1"),
      text: "Context injected by a client",
      description: "client context",
      metadata: { source: "conformance", n: 1 },
      resume: false,
    })
    // Same ID again returns the pending input.
    await ctx.http("POST", `/api/session/${session}/synthetic`, { id: id("S1"), text: "other text", resume: false })
    await ctx.http("POST", `/api/session/${session}/synthetic`, {
      text: "Queued context",
      delivery: "queue",
      resume: false,
    })
    await inbox(ctx, session)
    await ctx.http("POST", `/api/session/${session}/prompt`, {
      id: id("S1"),
      text: "conflicting prompt",
      resume: false,
    })
    await ctx.http("POST", `/api/session/${session}/synthetic`, { text: "Resume now" })
    await ctx.settle(session)
    await messages(ctx, session)
    // A delivered synthetic ID returns the delivered message.
    await ctx.http("POST", `/api/session/${session}/synthetic`, { id: id("S1"), text: "again", resume: false })
    await ctx.http("POST", `/api/session/${session}/synthetic`, { description: "missing text" })
    await ctx.http("GET", `/api/session/${session}/message/${id("S1")}`)
  },
)

scenario("wait on an idle and a busy session", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise))
  const session = await ctx.session({ title: "wait" })
  await ctx.wait(session)
  await startHeld(ctx, session, "Hold on")
  const waiting = ctx.wait(session)
  g.open()
  await waiting
  await ctx.http("GET", `/api/session/${session}`)
})

scenario("get one message", { turns: [say("A reply.")] }, async (ctx) => {
  const session = await ctx.session({ title: "message" })
  await ctx.prompt(session, "Hello", { id: id("M1") })
  const other = await ctx.session({ title: "other" })
  await ctx.http("GET", `/api/session/${session}/message/${id("M1")}`)
  const assistant = (await ctx.http("GET", `/api/session/${session}/message?type=assistant`)).body.data[0].id
  await ctx.http("GET", `/api/session/${session}/message/${assistant}`)
  // A message is only found through the session that owns it.
  await ctx.http("GET", `/api/session/${other}/message/${id("M1")}`)
  await ctx.http("GET", `/api/session/${session}/message/${UNKNOWN_MSG}`)
  await ctx.http("GET", `/api/session/${session}/message/not-a-message`)
})

const SUMMARY =
  "## Objective\n- Answer the user's questions\n\n## Work State\n### Completed\n- Started the long task\n\n## Next Move\n1. Wait for the next question"

scenario("compaction requests wait in the inbox of a busy session", {}, async (ctx) => {
  const g = gate()
  ctx.provider.push(held(g.promise), say(SUMMARY), say("Spare reply."))
  const session = await ctx.session({ title: "compact while busy" })
  await startHeld(ctx, session, "Long task")
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C1") })
  // The same ID returns the pending request; another ID joins the one already pending.
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C1") })
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C2"), delivery: "queue" })
  await inbox(ctx, session)
  await ctx.http("DELETE", `/api/session/${session}/inbox/${id("C1")}`)
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C3"), delivery: "queue" })
  await inbox(ctx, session)
  await ctx.http("POST", `/api/session/${session}/compact`, { delivery: "sometime" })
  g.open()
  await ctx.settle(session)
  await ctx.wait(session)
  await inbox(ctx, session)
  await messages(ctx, session)
  await ctx.http("GET", `/api/session/${session}/context`)
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C3") })
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("C3"), text: "reuse", resume: false })
})

// Message cursors carry only the anchor message ID, order and direction, so clients can build them from known IDs.
scenario(
  "page through messages with cursors",
  { turns: [say("First reply."), say("Second reply."), say("Third reply.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "message pages" })
    for (const [suffix, text] of [
      ["P1", "first"],
      ["P2", "second"],
      ["P3", "third"],
    ])
      await ctx.prompt(session, text, { id: id(suffix) })
    const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url")
    const page = (query: string) => ctx.http("GET", `/api/session/${session}/message?${query}`)
    await page("limit=2")
    await page("limit=2&order=asc")
    await page(`limit=2&cursor=${encode({ id: id("P2"), order: "asc", direction: "next" })}`)
    await page(`limit=2&cursor=${encode({ id: id("P2"), order: "asc", direction: "previous" })}`)
    await page(`limit=2&cursor=${encode({ id: id("P2"), order: "desc", direction: "next" })}`)
    await page(`limit=2&cursor=${encode({ id: id("P2"), order: "desc", direction: "previous" })}`)
    await page(`type=user&cursor=${encode({ id: id("P1"), order: "asc", direction: "next" })}`)
    // An anchor that is not in this session yields an empty page.
    await page(`cursor=${encode({ id: UNKNOWN_MSG, order: "asc", direction: "next" })}`)
    // Malformed cursors and cursors combined with an order are rejected.
    await page(`cursor=${encode({ id: id("P1"), order: "asc", direction: "next" })}&order=asc`)
    await page("cursor=not-a-cursor")
    await page(`cursor=${encode({ id: id("P1"), order: "sideways", direction: "next" })}`)
    await page("limit=0")
    await page("limit=201")
    await page("type=bogus")
  },
)
