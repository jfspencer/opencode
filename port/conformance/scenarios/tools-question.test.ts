// The question tool asks the user through a form; the scenario answers it through the session form endpoints.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const color = {
  question: "Which color should the button be?",
  header: "Button color",
  options: [
    { label: "Blue (Recommended)", description: "Matches the brand palette" },
    { label: "Green", description: "Signals success" },
  ],
}

const features = {
  question: "Which features should be enabled?",
  header: "Features",
  multiple: true,
  options: [
    { label: "Search", description: "Full-text search" },
    { label: "Export", description: "CSV export" },
    { label: "Sharing", description: "Share links" },
  ],
}

// `ctx.settle` only recognizes blocking events that carry `data.sessionID`; `form.created` carries the session
// in `data.form.sessionID`, so wait for it here.
async function prompted(ctx: Ctx, id: string, text: string) {
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  const event = await ctx.until((event) => event?.type === "form.created" && event?.data?.form?.sessionID === id, from)
  ctx.note("blocked", event.type)
}

/** Prompt, wait for the question form, and return the pending form's id. */
async function ask(ctx: Ctx, text: string) {
  const id = await ctx.session()
  await prompted(ctx, id, text)
  const pending = await ctx.http("GET", `/api/session/${id}/form`)
  const form = pending.body?.data?.[0]
  if (!form) throw new Error(`no pending form: ${JSON.stringify(pending.body)}`)
  await ctx.http("GET", `/api/session/${id}/form/${form.id}`)
  return { id, form: form.id as string }
}

async function finish(ctx: Ctx, id: string) {
  await ctx.settle(id)
  await ctx.http("GET", `/api/session/${id}/form`)
  await ctx.http("GET", `/api/session/${id}/message`)
}

scenario(
  "question with one choice answered by option",
  { turns: [call("question", { questions: [color] }), say("Blue it is.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Style the button")
    await ctx.http("GET", "/api/form")
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: "Blue (Recommended)" } })
    await finish(ctx, id)
    await ctx.http("GET", `/api/session/${id}/form/${form}`)
  },
)

scenario(
  "question with several questions and a multiselect",
  { turns: [call("question", { questions: [color, features] }), say("Configured.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Configure the app")
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: "Green", q1: ["Search", "Sharing"] } })
    await finish(ctx, id)
  },
)

scenario(
  "question answered with a custom answer",
  { turns: [call("question", { questions: [color, features] }), say("Custom noted.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Configure the app")
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, {
      answer: { q0: "Purple, like the logo", q1: ["Search", "Offline mode"] },
    })
    await finish(ctx, id)
  },
)

scenario(
  "question with an unanswered question",
  { turns: [call("question", { questions: [color, features] }), say("Partially answered.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Configure the app")
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q1: [] } })
    await finish(ctx, id)
  },
)

scenario(
  "question reply with invalid answers, then a valid one",
  { turns: [call("question", { questions: [color, features] }), say("Thanks.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Configure the app")
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: ["Blue"] } })
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q1: "Search" } })
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q9: "Extra" } })
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: 3 } })
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, {})
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: "Green", q1: ["Export"] } })
    await finish(ctx, id)
    // Settled forms reject further replies and cancellation.
    await ctx.http("POST", `/api/session/${id}/form/${form}/reply`, { answer: { q0: "Blue" } })
    await ctx.http("DELETE", `/api/session/${id}/form/${form}`)
  },
)

scenario(
  "question dismissed by the user",
  { turns: [call("question", { questions: [color] }), say("Not reached.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Style the button")
    await ctx.http("DELETE", `/api/session/${id}/form/${form}`)
    await finish(ctx, id)
    await ctx.http("GET", `/api/session/${id}/form/${form}`)
  },
)

scenario(
  "question dismissed with a message",
  { turns: [call("question", { questions: [color] }), say("I'll pick blue myself.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Style the button")
    await ctx.http("DELETE", `/api/session/${id}/form/${form}?message=${encodeURIComponent("Decide yourself, I am away.")}`)
    await finish(ctx, id)
  },
)

scenario(
  "question interrupted while pending",
  { turns: [call("question", { questions: [color] }), say("Not reached.")] },
  async (ctx) => {
    const { id, form } = await ask(ctx, "Style the button")
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    await ctx.http("GET", `/api/session/${id}/form/${form}`)
    await ctx.http("GET", `/api/session/${id}/form`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "question form endpoints with unknown ids",
  { turns: [] },
  async (ctx) => {
    const id = await ctx.session()
    // Location services boot (and announce themselves) asynchronously; a location-scoped request loads them, and
    // waiting for the last announcement keeps a scenario without a model turn from ending first.
    await ctx.http("GET", "/api/form")
    await ctx.until((event) => event?.type === "plugin.updated")
    await ctx.http("GET", `/api/session/${id}/form`)
    await ctx.http("GET", `/api/session/${id}/form/frm_doesnotexist`)
    await ctx.http("POST", `/api/session/${id}/form/frm_doesnotexist/reply`, { answer: { q0: "x" } })
    await ctx.http("DELETE", `/api/session/${id}/form/frm_doesnotexist`)
    await ctx.http("GET", `/api/session/ses_doesnotexist/form`)
    await ctx.http("GET", `/api/session/${id}/form/not-a-form-id`)
  },
)

scenario(
  "question with no questions or without options",
  {
    turns: [
      call("question", { questions: [] }),
      call("question", { questions: [{ question: "Anything else?", header: "Other", options: [] }] }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session()
    await prompted(ctx, id, "Ask me").catch(() => ctx.note("blocked", "never"))
    const pending = await ctx.http("GET", `/api/session/${id}/form`)
    const form = pending.body?.data?.[0]
    if (form) {
      await ctx.http("POST", `/api/session/${id}/form/${form.id}/reply`, { answer: { q0: "Nothing" } })
      await finish(ctx, id)
    }
  },
)

scenario(
  "question denied by permission rules",
  {
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "question", resource: "*", effect: "deny" },
      ],
    },
    turns: [call("question", { questions: [color] }), say("Cannot ask.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Style the button")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
