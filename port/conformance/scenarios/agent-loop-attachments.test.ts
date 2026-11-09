// Prompt attachments as the model request carries them (session/runner/to-llm-message.ts, model-request.ts
// `unsupportedParts` and `boundImages`, ai/src/protocols/openai-chat.ts `lowerMedia`): text and directories inline,
// images, videos and PDFs as content parts, media the model cannot read as a note asking it to tell the user, and media
// the model reads but Chat Completions cannot carry (audio) refused before the request is sent (attachments are typed
// by their bytes, and the sniffer knows no audio, so a prompt never carries any). Images a tool returns
// follow the tool message as a user message.
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const b64 = (value: string) => Buffer.from(value).toString("base64")
// Binary payloads (text-looking bytes would be read as text attachments).
const bytes = (...values: number[]) => Buffer.from(values).toString("base64")
const MP3 = bytes(0x49, 0x44, 0x33, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xfb, 0x90, 0x00, 0x00, 0xff, 0xee)
const MP4 = bytes(0, 0, 0, 0x18, 0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6f, 0x6d, 0, 0, 2, 0, 0x69, 0x73, 0x6f, 0x6d, 0x69, 0x73, 0x6f, 0x32, 0x80, 0xff)
const WAV = bytes(0x52, 0x49, 0x46, 0x46, 0x24, 0, 0, 0, 0x57, 0x41, 0x56, 0x45, 0x66, 0x6d, 0x74, 0x20, 0x10, 0, 0, 0, 1, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0)
const BLOB = bytes(0x00, 0x01, 0xfe, 0xff, 0x80, 0x81)

const listener = {
  name: "Listener",
  package: "@opencode/ai/providers/openrouter",
  capabilities: { tools: true, input: ["text", "image", "audio", "video"], output: ["text"], temperature: true, reasoning: { supported: false } },
  limit: { context: 131072, output: 8192 },
}
const blind = { ...listener, name: "Blind", capabilities: { ...listener.capabilities, input: ["text"] } }
const config = { providers: { openrouter: { models: { "acme/listener": listener, "acme/blind": blind } } } }
const ref = (id: string) => ({ providerID: "openrouter", id })

async function attach(ctx: Ctx, files: unknown[], model?: string, text = "Look at these") {
  const id = await ctx.session({ title: "attachments", ...(model ? { model: ref(model) } : {}) })
  await ctx.prompt(id, text, { files })
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

scenario(
  "an audio attachment is typed by its bytes, not its declared type",
  { config, turns: [say("Never sent.")] },
  (ctx) => attach(ctx, [{ uri: `data:audio/mpeg;base64,${MP3}`, name: "clip.mp3" }], "acme/listener").then(() => undefined),
)

scenario(
  "audio and video the model cannot read become notes",
  { turns: [say("Noted.")] },
  (ctx) =>
    attach(ctx, [
      { uri: `data:audio/wav;base64,${WAV}`, name: "voice.wav" },
      { uri: `data:video/mp4;base64,${MP4}` },
      { uri: `data:application/octet-stream;base64,${BLOB}`, name: "blob.bin" },
    ]).then(() => undefined),
)

scenario(
  "an image that cannot be decoded is refused",
  { turns: [say("Never sent.")] },
  (ctx) => attach(ctx, [{ uri: `data:image/bmp;base64,${b64("BM bitmap")}`, name: "old.bmp" }]).then(() => undefined),
)

scenario(
  "video the model reads is sent as a video part",
  { config, turns: [say("A video.")] },
  (ctx) => attach(ctx, [{ uri: `data:video/mp4;base64,${MP4}`, name: "clip.mp4" }], "acme/listener").then(() => undefined),
)

scenario(
  "mixed attachments keep their order",
  { files: { "notes.txt": "first line\nsecond line\n" }, turns: [say("Mixed.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    await attach(ctx, [
      { uri: `file://${ctx.project}/notes.txt`, description: "Notes" },
      { uri: `file://${ctx.project}/pixel.png` },
      { uri: `data:application/pdf;base64,${b64("%PDF-1.4\n%%EOF\n")}`, name: "doc.pdf" },
      { uri: `data:image/png;base64,${PNG}`, name: "inline.png" },
    ])
  },
)

scenario(
  "images replayed to a model without image input become notes",
  { config, turns: [say("A pixel."), say("No eyes.")] },
  async (ctx) => {
    const id = await attach(ctx, [{ uri: `data:image/png;base64,${PNG}`, name: "a.png" }])
    await ctx.http("POST", `/api/session/${id}/model`, { model: ref("acme/blind") })
    await ctx.prompt(id, "What was in the picture?")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "an image a tool reads follows the tool message",
  { turns: [call("read", { path: "pixel.png" }), say("A pixel.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Read the picture")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "an image a tool reads for a model without image input becomes a note",
  { config, turns: [call("read", { path: "pixel.png" }), say("No eyes.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    const id = await ctx.session({ title: "attachments", model: ref("acme/blind") })
    await ctx.prompt(id, "Read the picture")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "an attachment-only prompt",
  { turns: [say("Just a picture.")] },
  (ctx) => attach(ctx, [{ uri: `data:image/png;base64,${PNG}`, name: "only.png" }], undefined, "").then(() => undefined),
)
