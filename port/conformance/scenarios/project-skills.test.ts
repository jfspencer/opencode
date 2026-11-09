// Skills and references: discovery from config directories, compatibility roots (.claude, .agents), configured
// paths and URLs; loading a skill through the tool and the session endpoint; and local project references.
import { mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { baseConfig } from "../lib/model"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

async function reload(ctx: Ctx) {
  const from = ctx.events.length
  await ctx.http("POST", "/api/location/reload")
  await ctx.until((event) => event?.type === "plugin.updated", from)
}

const skill = (front: Record<string, string>, body: string) =>
  ["---", ...Object.entries(front).map(([k, v]) => `${k}: ${v}`), "---", "", body, ""].join("\n")

const skillFiles = {
  ".opencode/skills/deploy/SKILL.md": skill({ name: "Deploy", description: "Deploy the application safely." }, "# Deploy\n\nRun the deploy script."),
  ".opencode/skills/deploy/scripts/run.sh": "echo deploy\n",
  ".opencode/skills/deploy/reference/notes.md": "Notes.\n",
  ".opencode/skills/flat.md": skill({ description: "A flat skill file at the top of a skill root." }, "Flat body."),
  ".opencode/skill/manual/SKILL.md": skill(
    { description: "Only on request.", "disable-model-invocation": "yes" },
    "Manual skill body.",
  ),
  ".opencode/skills/auto/SKILL.md": [
    "---",
    "description: Explicitly auto-invokable.",
    "metadata:",
    "  opencode/autoinvoke: false",
    "---",
    "Auto body.",
  ].join("\n"),
  ".opencode/skills/group/inner/SKILL.md": skill({ description: "Nested two levels down." }, "Inner body."),
  ".opencode/skills/group/ignored.md": "Markdown below depth zero without SKILL.md is not a skill.\n",
  ".opencode/skills/.hidden/SKILL.md": skill({ description: "Hidden directories are skipped." }, "Hidden."),
  ".opencode/skills/badfront/SKILL.md": ["---", "name: [unclosed", "description: 5", "---", "Bad."].join("\n"),
  ".opencode/skills/numeric/SKILL.md": ["---", "description: 42", "---", "Numeric description."].join("\n"),
  ".claude/skills/claude-skill/SKILL.md": skill({ description: "From the .claude compatibility root." }, "Claude body."),
  ".agents/skills/agents-skill/SKILL.md": skill({ description: "From the .agents compatibility root." }, "Agents body."),
  "extra-skills/custom/SKILL.md": skill({ description: "From a configured skill path." }, "Custom body."),
}

scenario("built-in skills", {}, async (ctx) => {
  await ctx.http("GET", "/api/skill")
})

// The links are made before the location boots: made after, they would reach the listing only when the file
// watcher rescans, which races the scenario.
scenario(
  "skills from config directories and compatibility roots",
  { boot: false, files: skillFiles, config: { skills: ["extra-skills", "missing-skills/sub"] } },
  async (ctx) => {
    // A symlinked skill directory and a dangling link inside a skill root.
    symlinkSync(path.join(ctx.project, "extra-skills/custom"), path.join(ctx.project, ".opencode/skills/linked"))
    symlinkSync(path.join(ctx.project, "nowhere"), path.join(ctx.project, ".opencode/skills/dangling"))
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "skills from absolute and home paths",
  { files: { "abs/one/SKILL.md": skill({ description: "Absolute skill." }, "One.") } },
  async (ctx) => {
    const home = path.join(process.env.HOME!, "conformance-skills")
    mkdirSync(path.join(home, "homeskill"), { recursive: true })
    writeFileSync(path.join(home, "homeskill", "SKILL.md"), skill({ description: "Home skill." }, "Home."))
    const updated = (from: number) =>
      ctx.until((event) => event?.type === "skill.updated" && event?.location?.directory === ctx.project, from)
    try {
      const from = ctx.events.length
      ctx.write("opencode.json", JSON.stringify({ skills: [path.join(ctx.project, "abs"), "~/conformance-skills"] }))
      // The file watcher reloads the location: wait for it, or the reload races the end of the scenario.
      await updated(from)
      await ctx.http("GET", "/api/skill")
      // Removing the home skill directory drops its skill when the watcher rescans: wait for that too.
      const removed = ctx.events.length
      rmSync(home, { recursive: true, force: true })
      await updated(removed)
      await ctx.http("GET", "/api/skill")
    } finally {
      rmSync(home, { recursive: true, force: true })
    }
  },
)

// Each URL scenario uses its own source URL: downloads are cached per URL in the shared global cache directory.
// One downloadable file per scenario keeps the outbound request order deterministic (downloads run concurrently).
const urlSkills = (base: string, index: unknown, files: Record<string, string>) => ({
  config: { skills: [base] },
  responders: [
    (request: { url: string }) => {
      if (!request.url.startsWith(base)) return undefined
      if (request.url === `${base}/index.json`) return { raw: JSON.stringify(index), headers: { "content-type": "application/json" } }
      const body = files[request.url.slice(base.length + 1)]
      return body === undefined ? { status: 404, body: { error: "missing" } } : { raw: body, headers: { "content-type": "text/markdown" } }
    },
  ],
})

scenario(
  "skills from a URL index",
  urlSkills(
    "https://skills.example.test/pack",
    {
      skills: [
        { name: "remote", files: ["SKILL.md"] },
        { name: "../escape", files: ["SKILL.md"] },
        { name: "nomd", files: ["readme.txt"] },
        { name: "unsafe", files: ["SKILL.md", "../outside.md"] },
        { name: "query", files: ["SKILL.md", "a?b.md"] },
        { name: "encoding", files: ["SKILL.md", "bad%E0%A4%A.md"] },
        { name: ".", files: ["SKILL.md"] },
      ],
    },
    { "remote/SKILL.md": skill({ description: "Downloaded skill." }, "Remote body.") },
  ),
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "versioned skill from a URL index",
  urlSkills(
    "https://skills.example.test/versioned",
    { skills: [{ name: "versioned", version: "2", files: ["versioned.md"] }] },
    { "versioned/versioned.md": skill({ description: "Versioned skill." }, "V2.") },
  ),
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "URL skill whose file is missing",
  urlSkills("https://skills.example.test/broken", { skills: [{ name: "broken", files: ["SKILL.md"] }] }, {}),
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "versioned URL skill whose download fails",
  urlSkills("https://skills.example.test/vbroken", { skills: [{ name: "vbroken", version: "1", files: ["SKILL.md"] }] }, {}),
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "URL skill index with an invalid shape",
  urlSkills("https://skills.example.test/badindex", { skills: "nope" }, {}),
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "skills from an unreachable URL index",
  { config: { skills: ["https://skills.example.test/down/"] } },
  async (ctx) => {
    await ctx.http("GET", "/api/skill")
  },
)

scenario(
  "model loads a project skill with the skill tool",
  {
    files: skillFiles,
    turns: [call("skill", { id: "deploy" }), call("skill", { id: "no-such-skill" }), say("Loaded.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Deploy please")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "model loads a built-in skill with the skill tool",
  { turns: [call("skill", { id: "opencode" }), say("Loaded.")] },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "How do I configure opencode?")
  },
)

scenario(
  "activate a skill through the session endpoint",
  { files: skillFiles, turns: [say("Using the skill."), say("Second.")] },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "flat" })
    await ctx.settle(id, from)
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "manual", resume: false })
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "missing" })
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "denied skills are hidden from the model",
  {
    files: skillFiles,
    config: {
      permissions: [...baseConfig.permissions, { action: "skill", resource: "deploy", effect: "deny" }, { action: "skill", resource: "group*", effect: "deny" }],
    },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "List skills")
  },
)

scenario(
  "skill list changes between turns",
  // The file watcher is off: the scenario reloads after each write, and the watcher's reload would race it.
  { server: { filewatcher: false }, files: { ".opencode/skills/first/SKILL.md": skill({ description: "First skill." }, "First.") }, turns: [say("One."), say("Two.")] },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Turn one")
    ctx.write(".opencode/skills/second/SKILL.md", skill({ description: "Second skill." }, "Second."))
    ctx.write(".opencode/skills/first/SKILL.md", skill({ description: "First skill, revised." }, "First."))
    await reload(ctx)
    await ctx.prompt(id, "Turn two")
  },
)

scenario(
  "local references",
  {
    files: {
      "docs/readme.md": "Docs.\n",
      "shared/lib.ts": "export {}\n",
      ".opencode/opencode.json": JSON.stringify({ references: { fromdot: { path: "../docs", description: "Relative to .opencode." } } }),
    },
    config: {
      references: {
        docs: "./docs",
        shared: { path: "./shared", description: "Shared library code." },
        hiddenref: { path: "./docs", description: "Hidden from listings.", hidden: true },
        home: "~/conformance-ref",
        absolute: "/opt/conformance-ref",
        "bad name": "./docs",
        "bad/name": "./docs",
        filerepo: "file:///srv/repos/local.git",
        nonsense: "plainword",
      },
    },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    // GET /api/reference does not wait for plugin activation: boot the location first.
    await ctx.http("GET", "/api/location")
    await ctx.until((event) => event?.type === "plugin.updated")
    await ctx.http("GET", "/api/reference")
    const id = await ctx.session()
    await ctx.prompt(id, "What references exist?")
  },
)

// The first request to a location boots it, and the listing waits for its plugins to register the references.
scenario(
  "references on a location's first request",
  { boot: false, files: { "docs/readme.md": "Docs.\n" }, config: { references: { docs: "./docs" } } },
  async (ctx) => {
    await ctx.http("GET", "/api/reference")
  },
)

scenario(
  "reference changes between turns",
  {
    // The file watcher is off: the scenario reloads after each write, and the watcher's reload would race it.
    server: { filewatcher: false },
    files: {
      "a/x.txt": "a\n",
      "b/y.txt": "b\n",
      "opencode.json": JSON.stringify({
        references: { alpha: { path: "./a", description: "Alpha." }, beta: { path: "./b", description: "Beta." } },
      }),
    },
    turns: [say("One."), say("Two."), say("Three."), say("Four.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Turn one")
    ctx.write(
      "opencode.json",
      JSON.stringify({
        references: {
          alpha: { path: "./a", description: "Alpha, revised." },
          gamma: { path: "./b", description: "Gamma." },
        },
      }),
    )
    await reload(ctx)
    await ctx.prompt(id, "Turn two")
    ctx.write(
      "opencode.json",
      JSON.stringify({ references: { alpha: { path: "./a", description: "Alpha, revised." }, gamma: { path: "./b", description: "Gamma." } } }) + "\n",
    )
    await reload(ctx)
    await ctx.prompt(id, "Turn three")
    ctx.write("opencode.json", JSON.stringify({ references: {} }))
    await reload(ctx)
    await ctx.prompt(id, "Turn four")
  },
)
