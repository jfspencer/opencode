// Forms: creating session forms (field definitions and their validation), listing pending forms for a location,
// answering them (every answer validation rule), cancelling, and settled-form errors.
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
// Form routes don't boot the location by themselves, so boot it first.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await ctx.http("GET", "/api/location")
    await ctx.until((event) => event?.type === "plugin.updated")
    await body(ctx)
  })

const fields = [
  { key: "name", type: "string", title: "Name", required: true, minLength: 2, maxLength: 10, pattern: "^[a-z]+$" },
  { key: "email", type: "string", format: "email" },
  { key: "site", type: "string", format: "uri" },
  { key: "day", type: "string", format: "date" },
  { key: "when", type: "string", format: "date-time" },
  { key: "color", type: "string", options: [{ value: "red", label: "Red" }, { value: "blue", label: "Blue" }] },
  { key: "flavor", type: "string", options: [{ value: "plain", label: "Plain" }], custom: true },
  { key: "count", type: "integer", minimum: 1, maximum: 5 },
  { key: "ratio", type: "number", minimum: 0, maximum: 1 },
  { key: "agree", type: "boolean" },
  {
    key: "tags",
    type: "multiselect",
    options: [
      { value: "a", label: "A" },
      { value: "b", label: "B" },
      { value: "c", label: "C" },
    ],
    minItems: 1,
    maxItems: 2,
  },
  { key: "reason", type: "string", required: true, when: [{ key: "agree", op: "eq", value: false }] },
  { key: "extra", type: "string", when: [{ key: "tags", op: "neq", value: "c" }] },
  { key: "docs", type: "external", url: "https://example.test/docs", title: "Read the docs" },
]

const valid = {
  name: "alice",
  email: "alice@example.test",
  site: "https://example.test",
  day: "2026-02-28",
  when: "2026-02-28T10:00:00Z",
  color: "red",
  flavor: "anything",
  count: 3,
  ratio: 0.5,
  agree: true,
  tags: ["a", "b"],
  extra: "fine",
  docs: true,
}

scenario("create, list, answer and cancel forms", {}, async (ctx) => {
  const sessionID = await ctx.session()
  const form = await ctx.http("POST", `/api/session/${sessionID}/form`, {
    title: "Profile",
    metadata: { source: "conformance" },
    fields,
  })
  const id = form.body.data?.id ?? form.body.id
  await ctx.http("POST", `/api/session/${sessionID}/form`, { id: "frm_fixed_identifier_01", title: "Second", fields: [{ key: "ok", type: "boolean" }] })
  await ctx.http("POST", `/api/session/${sessionID}/form`, { id: "frm_fixed_identifier_01", title: "Duplicate", fields: [{ key: "ok", type: "boolean" }] })
  await ctx.http("GET", "/api/form")
  await ctx.http("GET", `/api/session/${sessionID}/form`)
  await ctx.http("GET", `/api/session/${sessionID}/form/${id}`)
  const reply = (answer: Record<string, unknown>) =>
    ctx.http("POST", `/api/session/${sessionID}/form/${id}/reply`, { answer })
  const without = (key: string) => Object.fromEntries(Object.entries(valid).filter(([k]) => k !== key))
  await reply({ ...valid, unknown: "x" })
  await reply(without("docs"))
  await reply(without("name"))
  await reply({ ...valid, name: 5 })
  await reply({ ...valid, name: "" })
  await reply({ ...valid, name: "a" })
  await reply({ ...valid, name: "abcdefghijkl" })
  await reply({ ...valid, name: "Alice" })
  await reply({ ...valid, email: "not-an-email" })
  await reply({ ...valid, site: "not a uri" })
  await reply({ ...valid, day: "2026-02-30" })
  await reply({ ...valid, when: "yesterday" })
  await reply({ ...valid, color: "green" })
  await reply({ ...valid, count: "3" })
  await reply({ ...valid, count: 2.5 })
  await reply({ ...valid, count: 0 })
  await reply({ ...valid, count: 9 })
  await reply({ ...valid, agree: "yes" })
  await reply({ ...valid, tags: "a" })
  await reply({ ...valid, tags: [] })
  await reply({ ...valid, tags: ["a", "b", "c"] })
  await reply({ ...valid, tags: ["z"] })
  await reply({ ...valid, agree: false })
  await reply({ ...valid, reason: "because" })
  await reply({ ...valid, tags: ["c"], extra: "hidden now" })
  await reply(valid)
  await reply(valid)
  await ctx.http("GET", `/api/session/${sessionID}/form/${id}`)
  await ctx.http("DELETE", `/api/session/${sessionID}/form/frm_fixed_identifier_01`)
  await ctx.http("DELETE", `/api/session/${sessionID}/form/frm_fixed_identifier_01`)
  await ctx.http("GET", "/api/form")
  await ctx.http("GET", `/api/session/${sessionID}/form/frm_missing_form_00000`)
  await ctx.http("POST", `/api/session/${sessionID}/form/frm_missing_form_00000/reply`, { answer: {} })
})

scenario("invalid form definitions", {}, async (ctx) => {
  const sessionID = await ctx.session()
  const create = (fields: unknown[]) => ctx.http("POST", `/api/session/${sessionID}/form`, { title: "Bad", fields })
  await create([])
  await create([
    { key: "a", type: "boolean" },
    { key: "a", type: "string" },
  ])
  await create([{ key: "b", type: "string", when: [{ key: "later", op: "eq", value: "x" }] }, { key: "later", type: "string" }])
  await create([{ key: "flag", type: "boolean" }, { key: "x", type: "string", when: [{ key: "flag", op: "eq", value: "yes" }] }])
  await create([{ key: "n", type: "number" }, { key: "x", type: "string", when: [{ key: "n", op: "eq", value: "1" }] }])
  await create([{ key: "s", type: "string" }, { key: "x", type: "string", when: [{ key: "s", op: "eq", value: 1 }] }])
  await create([
    { key: "pick", type: "string", options: [{ value: "a", label: "A" }] },
    { key: "x", type: "string", when: [{ key: "pick", op: "eq", value: "z" }] },
  ])
  await create([
    { key: "multi", type: "multiselect", options: [{ value: "a", label: "A" }] },
    { key: "x", type: "string", when: [{ key: "multi", op: "eq", value: "z" }] },
  ])
  await create([
    { key: "open", type: "multiselect", options: [{ value: "a", label: "A" }], custom: true },
    { key: "free", type: "string" },
    { key: "x", type: "string", when: [{ key: "open", op: "eq", value: "z" }, { key: "free", op: "neq", value: "q" }] },
  ])
  await create([{ key: "x", type: "rainbow" }])
  await ctx.http("POST", "/api/session/ses_00000000000000000000000000/form", { title: "No session", fields: [{ key: "x", type: "boolean" }] })
})

scenario("answer with an invalid pattern", {}, async (ctx) => {
  const sessionID = await ctx.session()
  const form = await ctx.http("POST", `/api/session/${sessionID}/form`, {
    title: "Pattern",
    fields: [{ key: "p", type: "string", pattern: "(" }, { key: "m", type: "multiselect", options: [{ value: "a", label: "A" }], custom: true, required: true }],
  })
  const id = form.body.data?.id ?? form.body.id
  await ctx.http("POST", `/api/session/${sessionID}/form/${id}/reply`, { answer: { p: "x", m: ["a"] } })
  await ctx.http("POST", `/api/session/${sessionID}/form/${id}/reply`, { answer: { m: [] } })
  await ctx.http("POST", `/api/session/${sessionID}/form/${id}/reply`, { answer: { m: ["anything"] } })
})
