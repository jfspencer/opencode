// Skills: discovery from the project (.opencode/skill[s]), config paths and URLs, the skill tool, reading skill
// files (including the built-in skill:// skills), and user activation.
//
// GET /api/skill is not recorded: it returns every skill's content, and the built-in `report` skill embeds a
// diagnostics snapshot of the host (OS release, terminal, shell), which differs between machines.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const deploy = [
  "---",
  "name: Deploy",
  "description: Deploy the application to staging. Use when the user asks to deploy or ship.",
  "---",
  "# Deploy",
  "",
  "1. Run `scripts/deploy.sh staging`.",
  "2. Check `reference/checklist.md` before announcing.",
  "",
].join("\n")

const skills = {
  ".opencode/skills/deploy/SKILL.md": deploy,
  ".opencode/skills/deploy/scripts/deploy.sh": "#!/bin/sh\necho deploying $1\n",
  ".opencode/skills/deploy/reference/checklist.md": "- tests pass\n- changelog updated\n",
  ".opencode/skills/review/SKILL.md": "---\ndescription: Review a pull request for style issues.\n---\nLook for style issues.\n",
  ".opencode/skills/manual-only/SKILL.md":
    "---\nname: Manual\ndescription: Only when the user asks.\ndisable-model-invocation: true\n---\nManual steps.\n",
  ".opencode/skills/meta-off/SKILL.md":
    '---\nname: Meta off\ndescription: Hidden through metadata.\nmetadata:\n  "opencode/autoinvoke": "no"\n---\nMeta steps.\n',
  ".opencode/skills/meta-on/SKILL.md":
    "---\nname: Meta on\ndescription: Listed despite the opt-out.\ndisable-model-invocation: yes\nmetadata:\n  opencode/autoinvoke: 1\n---\nMeta on steps.\n",
  ".opencode/skills/no-description/SKILL.md": "---\nname: Bare\n---\nNo description here.\n",
  ".opencode/skills/bad-frontmatter/SKILL.md": "---\nname: [unclosed\n---\nBroken.\n",
  ".opencode/skills/wrong-types/SKILL.md": "---\nname: 42\ndescription: true\n---\nWrong types.\n",
  ".opencode/skills/nested/group/inner/SKILL.md": "---\ndescription: A deeply nested skill.\n---\nInner.\n",
  ".opencode/skills/.hidden/SKILL.md": "---\ndescription: Hidden directory skill.\n---\nHidden.\n",
  ".opencode/skill/lint.md": "---\ndescription: Lint the code base.\n---\nRun the linter.\n",
  ".opencode/skill/notes.txt": "not a skill\n",
  ".opencode/skill/empty.md": "",
  "src/app.ts": "export const app = true\n",
}

async function run(ctx: Ctx, text: string) {
  const id = await ctx.session()
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

scenario(
  "skill discovery and loading from the project",
  {
    files: skills,
    turns: [
      call("skill", { id: "deploy" }),
      call("read", { path: ".opencode/skills/deploy/reference/checklist.md" }),
      call("skill", { id: "manual-only" }),
      call("skill", { id: "nested" }),
      call("skill", { id: "inner" }),
      call("skill", { id: "lint" }),
      call("skill", { id: "missing" }),
      say("Loaded."),
    ],
  },
  async (ctx) => {
    await run(ctx, "Deploy the app.")
  },
)

scenario(
  "skill built-in loaded and read through skill URIs",
  {
    turns: [
      call("skill", { id: "opencode" }),
      call("read", { path: "skill://opencode" }),
      call("read", { path: "skill://opencode/" }),
      call("read", { path: "skill://opencode/SKILL.md", offset: 3, limit: 4 }),
      call("read", { path: "skill://opencode/missing.md" }),
      call("read", { path: "skill://nope/SKILL.md" }),
      call("read", { path: "skill://../etc/passwd" }),
      call("read", { path: "skill://opencode/../report/SKILL.md" }),
      say("Read."),
    ],
  },
  async (ctx) => {
    await run(ctx, "How do I configure OpenCode?")
  },
)

scenario(
  "skill URI of a directory skill",
  {
    files: skills,
    turns: [call("read", { path: "skill://deploy/SKILL.md" }), call("read", { path: "skill://review" }), say("Not virtual.")],
  },
  async (ctx) => {
    await run(ctx, "Read the deploy skill by URI.")
  },
)

scenario(
  "skill denied and asked by permission rules",
  {
    files: skills,
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "skill", resource: "deploy", effect: "deny" },
        { action: "skill", resource: "review", effect: "ask" },
      ],
    },
    turns: [call("skill", { id: "deploy" }), call("skill", { id: "review" }), say("Denied.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Use the skills." })
    const state = await ctx.settle(id, from)
    if (state === "permission.asked") {
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      const request = pending.body.data[0]
      await ctx.http("POST", `/api/session/${id}/permission/${request.id}/reply`, { decision: "reject" })
      await ctx.settle(id)
    }
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "skill asked by permission and approved",
  {
    files: skills,
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "skill", resource: "*", effect: "ask" },
      ],
    },
    turns: [call("skill", { id: "review" }), call("read", { path: "skill://opencode/SKILL.md", limit: 2 }), say("Approved.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Review this." })
    for (let i = 0; i < 4; i++) {
      const state = await ctx.settle(id, from)
      if (state !== "permission.asked") break
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      from = ctx.events.length
      await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, { decision: "once" })
    }
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "skill activated by the user",
  {
    files: skills,
    turns: [say("Following the deploy skill."), say("Following the review skill.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "deploy" })
    await ctx.settle(id)
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "review", resume: false })
    await ctx.http("POST", `/api/experimental/session/${id}/skill`, { id: "missing" })
    await ctx.prompt(id, "Go on.")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "skill prompt with skills selected by the user",
  {
    files: skills,
    turns: [say("Using the selected skills.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Ship it.", {
      skills: [
        { id: "deploy", name: "Deploy" },
        { id: "missing", name: "Missing", text: "/missing" },
      ],
    })
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "skill from configured paths",
  {
    files: {
      "team-skills/format/SKILL.md": "---\ndescription: Format code with the team style.\n---\nUse tabs.\n",
      "team-skills/standalone.md": "---\ndescription: A standalone markdown skill.\n---\nStandalone.\n",
      "src/app.ts": "export const app = true\n",
    },
    config: { skills: ["team-skills", "does-not-exist", "~/home-skills"] },
    turns: [call("skill", { id: "format" }), call("skill", { id: "standalone" }), say("Formatted.")],
  },
  async (ctx) => {
    await run(ctx, "Format the code.")
  },
)

// Remote skills: an index.json lists skills and their files, which are downloaded into the cache.
const remote = (base: string, routes: Record<string, Turn>) => (request: ProviderRequest) => {
  if (request.kind !== "other" || !request.url.startsWith(base)) return
  return routes[request.url.slice(base.length)] ?? { status: 404, body: { error: "not found" } }
}

const text = (body: string, contentType = "text/markdown") => ({ raw: body, status: 200, headers: { "content-type": contentType } })

// Downloads within one index run concurrently, so each index here has at most one file to download, and the
// indexes (which load one after another) keep the recorded request order deterministic.
scenario(
  "skill from a remote index",
  {
    config: {
      skills: [
        "https://skills.example.com/catalog",
        "https://plain.example.com/skills",
        "https://partial.example.com/skills",
        "https://unversioned.example.com/skills",
      ],
    },
    responders: [
      remote("https://skills.example.com/catalog/", {
        "index.json": {
          status: 200,
          body: {
            skills: [
              { name: "remote-deploy", version: "1.0.0", files: ["SKILL.md"] },
              { name: "../escape", files: ["SKILL.md"] },
              { name: "no-skill-file", files: ["README.md"] },
              { name: "unsafe-file", files: ["SKILL.md", "../outside.md"] },
              { name: "absolute-file", files: ["SKILL.md", "https://elsewhere.example.com/x.md"] },
              { name: "query-file", files: ["SKILL.md", "a?b=1"] },
              { name: "bad-escape", files: ["SKILL.md", "bad%E0%A4%A.md"] },
            ],
          },
        },
        "remote-deploy/SKILL.md": text("---\ndescription: Deploy from a remote skill.\n---\nRemote deploy steps.\n"),
      }),
      remote("https://plain.example.com/skills/", {
        "index.json": { status: 200, body: { skills: [{ name: "plain", files: ["plain.md"] }] } },
        "plain/plain.md": text("---\ndescription: A plain remote skill.\n---\nPlain.\n"),
      }),
      // A versioned skill whose download fails is never installed.
      remote("https://partial.example.com/skills/", {
        "index.json": { status: 200, body: { skills: [{ name: "missing-file", version: "2", files: ["SKILL.md"] }] } },
      }),
      remote("https://unversioned.example.com/skills/", {
        "index.json": { status: 200, body: { skills: [{ name: "unversioned-missing", files: ["SKILL.md"] }] } },
      }),
    ],
    turns: [call("skill", { id: "remote-deploy" }), call("skill", { id: "plain" }), call("skill", { id: "missing-file" }), say("Remote skills loaded.")],
  },
  async (ctx) => {
    await run(ctx, "Deploy with the remote skill.")
  },
)

scenario(
  "skill remote index that fails",
  {
    config: { skills: ["https://broken.example.com/skills/", "https://invalid.example.com/skills"] },
    responders: [
      remote("https://broken.example.com/skills/", { "index.json": { status: 500, body: { error: "boom" } } }),
      remote("https://invalid.example.com/skills/", { "index.json": { status: 200, body: { skills: "nope" } } }),
    ],
    turns: [say("No remote skills.")],
  },
  async (ctx) => {
    await run(ctx, "List skills.")
  },
)

// The same index listed twice (with and without a trailing slash) shares one cache: the second pull sees the
// first one's files, and a new version replaces them.
scenario(
  "skill from a remote index updated to a new version",
  {
    config: { skills: ["https://versioned.example.com/skills", "https://versioned.example.com/skills/"] },
    responders: [
      (() => {
        let pulls = 0
        return remote("https://versioned.example.com/skills/", {
          get "index.json"() {
            pulls++
            return {
              status: 200,
              body: { skills: [{ name: "release", version: pulls === 1 ? "1.0.0" : "2.0.0", files: ["SKILL.md"] }] },
            }
          },
          "release/SKILL.md": text("---\ndescription: Cut a release.\n---\nRelease steps.\n"),
        })
      })(),
    ],
    turns: [call("skill", { id: "release" }), say("Released.")],
  },
  async (ctx) => {
    await run(ctx, "Cut a release.")
  },
)
