// read tool edge cases: media attachments, binary and huge files, paging, long lines, external paths, protected
// files, near-miss names, and AGENTS.md discovery next to read files.
import { spawnSync } from "node:child_process"
import { mkdirSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

/** Prompt, then answer each permission request with the next decision until the session is idle. */
async function drive(ctx: Ctx, text: string, decisions: string[] = []) {
  const id = await ctx.session()
  let from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  for (let i = 0; i < 12; i++) {
    const state = await ctx.settle(id, from)
    if (state !== "permission.asked") break
    const pending = await ctx.http("GET", `/api/session/${id}/permission`)
    from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, {
      decision: decisions.shift() ?? "once",
    })
  }
  return id
}

const reads = (...inputs: Record<string, unknown>[]) => [...inputs.map((input) => call("read", input)), say("Read.")]

// Minimal valid media headers; Mime.detect sniffs the leading bytes.
const png = Buffer.from(
  "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da63f8cfc0f01f0005000201a5f6a6c70000000049454e44ae426082",
  "hex",
)
const jpeg = Buffer.from("ffd8ffe000104a46494600010100000100010000ffd9", "hex")
const gif = Buffer.from("47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b", "hex")
const webp = Buffer.concat([Buffer.from("RIFF"), Buffer.from([26, 0, 0, 0]), Buffer.from("WEBPVP8L"), Buffer.alloc(14, 1)])
const pdf = Buffer.from("%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n")
const bmp = Buffer.concat([Buffer.from("BM"), Buffer.alloc(30, 0)])

function binary(ctx: Ctx, files: Record<string, Buffer>) {
  for (const [rel, bytes] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(ctx.project, rel)), { recursive: true })
    writeFileSync(path.join(ctx.project, rel), bytes)
  }
}

scenario(
  "read images and PDFs as attachments",
  {
    turns: reads(
      { path: "media/pixel.png" },
      { path: "media/photo.jpg" },
      { path: "media/anim.gif" },
      { path: "media/image.webp" },
      { path: "media/doc.pdf" },
      { path: "media/misnamed.txt" },
      { path: "media/bitmap.bmp" },
    ),
  },
  async (ctx) => {
    binary(ctx, {
      "media/pixel.png": png,
      "media/photo.jpg": jpeg,
      "media/anim.gif": gif,
      "media/image.webp": webp,
      "media/doc.pdf": pdf,
      "media/misnamed.txt": png,
      "media/bitmap.bmp": bmp,
    })
    const id = await drive(ctx, "Look at the media files")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "read media over the ingestion limit",
  { turns: reads({ path: "huge.png" }) },
  async (ctx) => {
    binary(ctx, { "huge.png": Buffer.concat([png, Buffer.alloc(20 * 1024 * 1024 + 1)]) })
    await drive(ctx, "Look at the huge image")
  },
)

scenario(
  "read binary files",
  {
    turns: reads({ path: "bin/data.bin" }, { path: "bin/late-nul.dat" }, { path: "bin/late-nul.dat", offset: 1, limit: 1 }, { path: "bin/invalid-utf8.txt" }),
  },
  async (ctx) => {
    binary(ctx, {
      "bin/data.bin": Buffer.from([0x00, 0x01, 0x02, 0xff, 0x10, 0x00]),
      "bin/late-nul.dat": Buffer.concat([Buffer.from("text line\n"), Buffer.from([0x41, 0x00, 0x42, 0x0a])]),
      "bin/invalid-utf8.txt": Buffer.from([0x68, 0x69, 0x0a, 0xc3, 0x28, 0x0a, 0xff, 0xfe, 0x0a]),
    })
    await drive(ctx, "Read the binary files")
  },
)

const numbered = (count: number, width = 0) =>
  Array.from({ length: count }, (_, i) => `line ${String(i + 1).padStart(width, "0")}`).join("\n") + "\n"

scenario(
  "read long lines and paging",
  {
    files: {
      "long-line.txt": `short\n${"a".repeat(2500)}\n${"é".repeat(2100)}\nend\n`,
      "many.txt": numbered(2500),
      "crlf.txt": "one\r\ntwo\r\nthree\r\n",
      "no-newline.txt": "alpha\nbeta",
      "empty.txt": "",
      "blank-lines.txt": "\n\n\n",
    },
    turns: reads(
      { path: "long-line.txt" },
      { path: "long-line.txt", offset: 2, limit: 1 },
      { path: "many.txt" },
      { path: "many.txt", offset: 2400 },
      { path: "many.txt", offset: 2501 },
      { path: "many.txt", offset: 3000 },
      { path: "many.txt", limit: 3 },
      { path: "many.txt", offset: 0, limit: 0 },
      { path: "crlf.txt" },
      { path: "crlf.txt", offset: 2 },
      { path: "no-newline.txt", offset: 2 },
      { path: "no-newline.txt", offset: 3 },
      { path: "empty.txt" },
      { path: "empty.txt", offset: 1 },
      { path: "empty.txt", offset: 2 },
      { path: "blank-lines.txt", offset: 2 },
      { path: "many.txt", offset: -1 },
    ),
  },
  async (ctx) => {
    await drive(ctx, "Read the text files in pages")
  },
)

scenario(
  "read large files across chunks",
  {
    turns: reads(
      { path: "big.txt" },
      { path: "big.txt", offset: 40000, limit: 3 },
      { path: "big.txt", offset: 79999 },
      { path: "big.txt", offset: 90000 },
      { path: "wide.txt", offset: 2 },
      { path: "huge-lines.txt", offset: 3, limit: 2 },
      { path: "tail-nul.txt", offset: 30000, limit: 1 },
      { path: "tail-nul.txt", offset: 1, limit: 1 },
    ),
  },
  async (ctx) => {
    // ~1 MB: four 256 KiB chunks, so deep offsets walk the chunk tree.
    ctx.write("big.txt", numbered(80000, 5))
    // A file over the byte budget with short lines: the page stops at 50 KiB rather than 2000 lines.
    ctx.write("wide.txt", Array.from({ length: 1000 }, (_, i) => `${i}`.padEnd(100, ".")).join("\n"))
    // Lines longer than a chunk.
    ctx.write("huge-lines.txt", ["first", "x".repeat(300_000), "y".repeat(300_000), "last"].join("\n"))
    // A NUL far into a large file is only noticed when the page reaches it.
    binary(ctx, { "tail-nul.txt": Buffer.concat([Buffer.from(numbered(40000, 5)), Buffer.from([0x00, 0x0a])]) })
    await drive(ctx, "Page through the large files")
  },
)

scenario(
  "read directories",
  {
    files: {
      "tree/b.txt": "b\n",
      "tree/a.txt": "a\n",
      "tree/sub/c.txt": "c\n",
      "tree/.hidden": "h\n",
      "tree/Zeta/z.txt": "z\n",
      "empty-dir/.keep": "",
    },
    turns: reads(
      { path: "tree" },
      { path: "tree/" },
      { path: "tree", offset: 2, limit: 2 },
      { path: "tree", offset: 10 },
      { path: "." },
      { path: "empty-dir" },
    ),
  },
  async (ctx) => {
    symlinkSync(path.join(ctx.project, "tree/a.txt"), path.join(ctx.project, "tree/link-to-a"))
    symlinkSync(path.join(ctx.project, "missing-target"), path.join(ctx.project, "tree/dangling"))
    mkdirSync(path.join(ctx.project, "empty-dir/inner"))
    await drive(ctx, "List the directories")
    // The final project snapshot follows links, so a dangling one cannot stay behind.
    unlinkSync(path.join(ctx.project, "tree/dangling"))
  },
)

scenario(
  "read with near-miss names and missing paths",
  {
    files: {
      "docs/it’s here.md": "curly apostrophe\n",
      "docs/non breaking.txt": "nbsp name\n",
      "docs/café.txt": "composed\n",
      "docs/report-2026.txt": "report\n",
      "docs/report-2025.txt": "old report\n",
      "src/index.ts": "export {}\n",
    },
    turns: reads(
      { path: "docs/it's here.md" },
      { path: "docs/non breaking.txt" },
      { path: "docs/café.txt" },
      { path: "docs/report.txt" },
      { path: "docs/report-2026" },
      { path: "docs/nothing-like-it.txt" },
      { path: "src/components/index.ts" },
      { path: "srcs/deeper/still/file.ts" },
      { path: "docs/report-2026.txt/inner" },
    ),
  },
  async (ctx) => {
    await drive(ctx, "Read files with slightly wrong names")
  },
)

scenario(
  "read outside the project",
  {
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "external_directory", resource: "*", effect: "ask" },
      ],
    },
    turns: reads(
      { path: "../outside/notes.txt" },
      { path: "../outside/notes.txt" },
      { path: "../outside/other.txt" },
      { path: "../outside" },
      { path: "../elsewhere/secret.txt" },
      { path: "../elsewhere/missing.txt" },
    ),
  },
  async (ctx) => {
    // The project's parent is the sandbox root (`<root>`), private to this test process.
    const parent = path.dirname(ctx.project)
    mkdirSync(path.join(parent, "outside"), { recursive: true })
    mkdirSync(path.join(parent, "elsewhere"), { recursive: true })
    writeFileSync(path.join(parent, "outside/notes.txt"), "outside notes\n")
    writeFileSync(path.join(parent, "outside/other.txt"), "other notes\n")
    writeFileSync(path.join(parent, "elsewhere/secret.txt"), "secret\n")
    const id = await drive(ctx, "Read files outside the project", ["once", "always", "reject", "once"])
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "read through a symlink that leaves the project",
  {
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "external_directory", resource: "*", effect: "ask" },
      ],
    },
    files: { "local.txt": "local\n" },
    turns: reads({ path: "local.txt" }, { path: "shared/notes.txt" }, { path: "shared/other.txt" }),
  },
  async (ctx) => {
    // A link inside the project to a directory outside it: reads through it meet the external directory rules.
    const outside = path.join(path.dirname(ctx.project), "linked-outside")
    mkdirSync(outside, { recursive: true })
    writeFileSync(path.join(outside, "notes.txt"), "outside notes\n")
    writeFileSync(path.join(outside, "other.txt"), "other notes\n")
    symlinkSync(outside, path.join(ctx.project, "shared"))
    const id = await drive(ctx, "Read the shared notes", ["once", "once"])
    await ctx.http("GET", `/api/session/${id}/message`)
    // The final project snapshot follows links; the outside files are not part of the project.
    unlinkSync(path.join(ctx.project, "shared"))
  },
)

scenario(
  "read absolute and home paths",
  {
    files: { "src/main.ts": "console.log('main')\n" },
  },
  async (ctx) => {
    ctx.provider.push(...reads({ path: path.join(ctx.project, "src/main.ts") }, { path: "~/notes/home.txt" }, { path: "~" }))
    mkdirSync(path.join(process.env.HOME!, "notes"), { recursive: true })
    writeFileSync(path.join(process.env.HOME!, "notes/home.txt"), "home notes\n")
    await drive(ctx, "Read by absolute path", ["once", "once", "reject"])
  },
)

scenario(
  "read protected environment files",
  {
    files: {
      ".env": "SECRET=1\n",
      ".env.local": "LOCAL=1\n",
      ".env.example": "SECRET=\n",
      "config/app.env": "APP=1\n",
    },
    turns: reads({ path: ".env" }, { path: ".env.local" }, { path: ".env.example" }, { path: "config/app.env" }),
  },
  async (ctx) => {
    await drive(ctx, "Read the environment files", ["once", "always", "once", "reject"])
  },
)

scenario(
  "read discovers AGENTS.md next to read files",
  {
    files: {
      "AGENTS.md": "Root instructions.\n",
      "packages/app/AGENTS.md": "App package instructions: use tabs.\n",
      "packages/app/src/AGENTS.md": "Source instructions.\n",
      "packages/app/src/main.ts": "export const main = 1\n",
      "packages/app/src/util.ts": "export const util = 1\n",
      "packages/lib/index.ts": "export {}\n",
    },
    turns: reads(
      { path: "packages/app/src/main.ts" },
      { path: "packages/app/src/util.ts" },
      { path: "packages/app" },
      { path: "packages/lib/index.ts" },
      { path: "packages/app/AGENTS.md" },
    ),
  },
  async (ctx) => {
    const id = await drive(ctx, "Read the app sources")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "read denied by permission rules",
  {
    files: { "secret/keys.txt": "k\n", "public/readme.txt": "r\n" },
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "read", resource: "secret/*", effect: "deny" },
        { action: "read", resource: "public/*", effect: "ask" },
      ],
    },
    turns: reads({ path: "secret/keys.txt" }, { path: "public/readme.txt" }),
  },
  async (ctx) => {
    await drive(ctx, "Read the protected files", ["reject"])
  },
)

scenario(
  "read a special file",
  { files: { "README.md": "# r\n" }, turns: reads({ path: "pipe" }, { path: "pipe", offset: 2 }) },
  async (ctx) => {
    spawnSync("mkfifo", [path.join(ctx.project, "pipe")])
    await drive(ctx, "Read the named pipe")
    // The final project snapshot would block opening the pipe.
    unlinkSync(path.join(ctx.project, "pipe"))
  },
)
