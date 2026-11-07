// Session view: question forms (from the question tool, or created through the API) answered in the TUI.
import { call, say, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed.
function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  const titled = Promise.withResolvers<void>()
  const [first, ...rest] = options.turns ?? []
  const turns: Turn[] | undefined =
    first && "events" in first ? [{ ...first, events: [{ until: titled.promise }, ...first.events] }, ...rest] : options.turns
  tuiScenario(name, { ...options, turns }, async (t) => {
    const started = Date.now()
    t.until((event) => event?.type === "session.renamed", 0, 60_000).then(titled.resolve, titled.resolve)
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

/** Submit a prompt and wait until the TUI shows the form it blocks on. */
async function prompt(t: TuiCtx, text: string, shown: string) {
  const from = t.events.length
  await t.type(text)
  await t.enter()
  await t.until((event) => event?.type === "form.created", from, 60_000)
  await t.waitFor(shown)
}

/** Dismiss the form: the tool fails, the execution is interrupted and the model is not asked again. */
async function dismiss(t: TuiCtx) {
  const from = t.events.length
  await t.escape()
  await t.until((event) => event?.type === "session.execution.interrupted", from, 60_000)
  await t.waitFor("interrupted")
}

/** Act, then wait until the turn is over and acknowledged, then for the text. */
async function finish(t: TuiCtx, act: () => Promise<void>, text: string) {
  const from = t.events.length
  await act()
  await t.until((event) => event?.type === "session.viewed", from, 60_000)
  await t.waitFor(text)
}

const color = {
  question: "Which color should the button be?",
  header: "Button color",
  options: [
    { label: "Blue (Recommended)", description: "Matches the brand palette" },
    { label: "Green", description: "Signals success" },
    { label: "Red", description: "Draws attention" },
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

scenario(
  "single question answered by moving and selecting",
  { height: 36, turns: [call("question", { questions: [color] }), say("Green it is.")] },
  async (t) => {
    await prompt(t, "Style the button", "Which color should the button be?")
    await t.frame("asked")
    await t.arrow("down")
    await t.frame("moved")
    await finish(t, () => t.enter(), "Green it is.")
    await t.frame("answered")
  },
)

scenario(
  "single question answered by number key",
  { height: 36, turns: [call("question", { questions: [color] }), say("Red then.")] },
  async (t) => {
    await prompt(t, "Style the button", "Which color should the button be?")
    await finish(t, () => t.type("3"), "Red then.")
    await t.frame("answered")
  },
)

scenario(
  "single question answered with a custom answer",
  { height: 36, turns: [call("question", { questions: [color] }), say("Purple noted.")] },
  async (t) => {
    await prompt(t, "Style the button", "Which color should the button be?")
    await t.arrow("up")
    await t.frame("custom-row")
    await t.enter()
    await t.type("Purple, like the logo")
    await t.frame("custom-typed")
    await finish(t, () => t.enter(), "Purple noted.")
    await t.frame("answered")
  },
)

scenario(
  "several questions with a multiselect, reviewed and submitted",
  { height: 40, turns: [call("question", { questions: [color, features] }), say("Configured.")] },
  async (t) => {
    await prompt(t, "Configure the app", "Which color should the button be?")
    await t.frame("first-question")
    await t.enter()
    await t.frame("second-question")
    await t.press(" ")
    await t.arrow("down")
    await t.arrow("down")
    await t.press(" ")
    await t.frame("toggled")
    await t.tab()
    await t.frame("review")
    await finish(t, () => t.enter(), "Configured.")
    await t.frame("answered")
  },
)

scenario(
  "several questions navigated with tab and arrows",
  { height: 40, turns: [call("question", { questions: [color, features] }), say("Partly answered.")] },
  async (t) => {
    await prompt(t, "Configure the app", "Which color should the button be?")
    await t.arrow("right")
    await t.frame("second-tab")
    await t.press("TAB", { shift: true })
    await t.frame("back-to-first")
    await t.arrow("left")
    await t.frame("wrapped-to-review")
    await finish(t, () => t.enter(), "Partly answered.")
    await t.frame("answered")
  },
)

scenario(
  "question dismissed with escape",
  { height: 36, turns: [call("question", { questions: [color] })] },
  async (t) => {
    await prompt(t, "Style the button", "Which color should the button be?")
    await dismiss(t)
    await t.frame("dismissed")
  },
)

scenario(
  "question on a narrow terminal",
  {
    width: 60,
    height: 30,
    tui: { session: { tps: false } },
    turns: [call("question", { questions: [color, features] })],
  },
  async (t) => {
    await prompt(t, "Configure the app", "Which color should the button be?")
    await t.frame("asked")
    await dismiss(t)
    await t.frame("dismissed")
  },
)

// Forms created through the API, for field types the question tool does not produce.
async function sessionID(t: TuiCtx) {
  const created = t.events.find((event: any) => event?.type === "session.created") as any
  return created.data.sessionID as string
}

async function form(t: TuiCtx, payload: Record<string, unknown>, shown: string) {
  const id = await sessionID(t)
  const from = t.events.length
  await t.http("POST", `/api/session/${id}/form`, payload)
  await t.until((event) => event?.type === "form.created", from)
  await t.waitFor(shown)
  return id
}

async function replied(t: TuiCtx, act: () => Promise<void>) {
  const from = t.events.length
  await act()
  await t.until((event) => event?.type === "form.replied" || event?.type === "form.cancelled", from)
}

scenario(
  "api form with typed fields answered in the TUI",
  { height: 40, turns: [say("Ready.")] },
  async (t) => {
    await finish(t, async () => (await t.type("Get ready"), await t.enter()), "Ready.")
    await form(
      t,
      {
        id: "frm_conformance0000000000000001",
        title: "Deployment settings",
        metadata: { message: "Configure the deployment before continuing." },
        fields: [
          { key: "confirm", type: "boolean", title: "Deploy now?", description: "Starts the rollout immediately." },
          { key: "replicas", type: "integer", title: "Replicas", minimum: 1, maximum: 5 },
          { key: "ratio", type: "number", title: "Traffic ratio", minimum: 0 },
          { key: "email", type: "string", format: "email", title: "Contact email" },
          { key: "notes", type: "string", title: "Notes", placeholder: "Anything else?", default: "none" },
        ],
      },
      "Deployment settings",
    )
    await t.frame("boolean")
    await t.enter()
    await t.frame("integer")
    await t.type("9")
    await t.enter()
    await t.frame("integer-invalid")
    await t.press("BACKSPACE")
    await t.type("3")
    await t.enter()
    await t.frame("number")
    await t.type("0.5")
    await t.enter()
    await t.frame("email")
    await t.type("not-an-email")
    await t.enter()
    await t.frame("email-invalid")
    await t.press("u", { ctrl: true })
    await t.type("ops@example.com")
    await t.enter()
    await t.frame("string-default")
    await t.enter()
    await t.frame("review")
    await replied(t, () => t.enter())
    await t.frame("submitted")
  },
)

scenario(
  "api form with conditional fields and a required multiselect",
  { height: 40, turns: [say("Ready.")] },
  async (t) => {
    await finish(t, async () => (await t.type("Get ready"), await t.enter()), "Ready.")
    await form(
      t,
      {
        id: "frm_conformance0000000000000002",
        title: "Release",
        fields: [
          {
            key: "channel",
            type: "string",
            title: "Channel",
            options: [
              { value: "stable", label: "Stable", description: "Everyone" },
              { value: "beta", label: "Beta", description: "Early adopters" },
            ],
          },
          {
            key: "cohorts",
            type: "multiselect",
            title: "Cohorts",
            required: true,
            minItems: 1,
            custom: true,
            when: [{ key: "channel", op: "eq", value: "beta" }],
            options: [
              { value: "staff", label: "Staff" },
              { value: "insiders", label: "Insiders" },
            ],
          },
          { key: "date", type: "string", format: "date", title: "Release date", required: true },
          { key: "secret", type: "string", hidden: true, default: "kept" },
        ],
      },
      "Release",
    )
    await t.frame("channel")
    await t.arrow("down")
    await t.enter()
    await t.frame("cohorts")
    await t.enter()
    await t.frame("cohorts-empty-confirmed")
    await t.press(" ")
    await t.arrow("down")
    await t.arrow("down")
    await t.enter()
    await t.type("Partners")
    await t.frame("cohorts-custom")
    await t.enter()
    await t.tab()
    await t.frame("date")
    await t.type("2026-02-30x")
    await t.enter()
    await t.frame("date-invalid")
    await t.press("u", { ctrl: true })
    await t.type("2026-03-01")
    await t.enter()
    // The hidden field is not asked: its default is submitted.
    await t.frame("review")
    await replied(t, () => t.enter())
    await t.frame("submitted")
  },
)

scenario(
  "api form dismissed with escape",
  { turns: [say("Ready.")] },
  async (t) => {
    await finish(t, async () => (await t.type("Get ready"), await t.enter()), "Ready.")
    await form(
      t,
      {
        id: "frm_conformance0000000000000003",
        title: "Survey",
        fields: [{ key: "ok", type: "boolean", title: "Is everything fine?", default: true }],
      },
      "Survey",
    )
    await t.frame("asked")
    await replied(t, () => t.escape())
    await t.frame("dismissed")
  },
)
