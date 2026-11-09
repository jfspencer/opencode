// One-shot image and decision generation (core/src/generate.ts) on OpenRouter's media routes: images through
// `/images/generations` (OpenAIImages with references as `input_references`, sizing as `aspect_ratio` and a
// `resolution` tier), decisions through System One (`/systemone`, media in `state`). Results come back inline: URL
// images are downloaded and typed from their bytes. Failures map to the API's typed errors; the requests the provider
// never ran (rate limits, overload) are retried at most three times.
import { MODEL_ID } from "../lib/model"
import type { ProviderRequest, Responder, Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
// A JPEG's magic bytes and nothing else: enough to be typed from its bytes.
const JPEG = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01]).toString("base64")
const UNKNOWN = Buffer.from("not an image").toString("base64")

const IMAGE_MODEL = "google/gemini-3-pro-image"
const DECISION_MODEL = "typesafe/system-one"

const config = {
  providers: {
    openrouter: {
      models: {
        [IMAGE_MODEL]: {
          name: "Gemini 3 Pro Image",
          type: "image",
          package: "@opencode/ai/providers/openrouter/images",
          capabilities: { tools: false, input: ["text", "image"], output: ["image"] },
        },
        [DECISION_MODEL]: {
          name: "System One",
          type: "decision",
          package: "@opencode/ai/providers/openrouter/decisions",
          capabilities: { tools: false, input: ["text", "image", "audio"], output: ["text"] },
        },
        // Media models whose package has no API of their type.
        "acme/chat-package-image": {
          name: "Chat package image",
          type: "image",
          package: "@opencode/ai/providers/openrouter",
          capabilities: { tools: false, input: ["text"], output: ["image"] },
        },
        "acme/image-package-decision": {
          name: "Image package decision",
          type: "decision",
          package: "@opencode/ai/providers/openrouter/images",
          capabilities: { tools: false, input: ["text"], output: ["text"] },
        },
      },
    },
  },
}

const image = { providerID: "openrouter", id: IMAGE_MODEL }
const decision = { providerID: "openrouter", id: DECISION_MODEL }

/** Answers one endpoint path (under the OpenRouter base URL) with the given turns in order, then 500s. */
function route(path: string, ...turns: Turn[]): Responder {
  return (request: ProviderRequest) => {
    if (request.url !== `https://openrouter.ai/api/v1${path}`) return undefined
    return turns.shift() ?? { status: 500, body: { error: { message: "conformance: no more answers" } } }
  }
}

const json = (body: unknown, status = 200): Turn => ({ status, body })

const questions = [
  { type: "predicate", name: "spam", instructions: "Is it spam?" },
  {
    type: "choice",
    name: "tone",
    instructions: "What is the tone?",
    choices: [{ value: true, description: "Friendly" }, { value: "neutral" }, { value: false }],
  },
  {
    type: "score",
    name: "quality",
    instructions: "How well is it written?",
    levels: [{ label: "poor" }, { label: "fair", description: "Readable" }, { label: "good" }],
  },
]

const answers = {
  model: DECISION_MODEL,
  answers: {
    spam: { type: "noul", noul: 0.125 },
    tone: { type: "choice", choice: "true", probabilities: { neutral: 0.25, true: 0.625, false: 0.125 }, confidence: 0.75 },
    quality: { type: "score", score: 1.5, probabilities: { "2": 0.5, "0": 0.0, "1": 0.5 }, confidence: 0.5 },
  },
  usage: { input_tokens: 42, output_tokens: 3, cost: 0.0125, extra: "kept" },
  id: "dec_conformance",
  provider: "TypeSafe",
  provider_metadata: { openrouter: { region: "eu" }, other: { ignored: true } },
}

async function generateImage(ctx: Ctx, payload: Record<string, unknown>) {
  await ctx.http("POST", "/api/experimental/generate/image", { model: image, prompt: "A red pixel", ...payload })
}

scenario(
  "generate an image",
  {
    config,
    responders: [
      route(
        "/images/generations",
        json({ data: [{ b64_json: PNG }], output_format: "png", size: "1024x1024", quality: "high", usage: { input_tokens: 10, output_tokens: 20, total_tokens: 30 } }),
      ),
    ],
  },
  (ctx) => generateImage(ctx, {}),
)

scenario(
  "generate images from references with a count, aspect and resolution",
  {
    config,
    responders: [
      route(
        "/images/generations",
        json({ data: [{ b64_json: PNG }, { b64_json: PNG }], output_format: "png" }),
        json({ data: [{ b64_json: PNG }], background: "transparent" }),
        json({ data: [{ b64_json: PNG }] }),
      ),
    ],
  },
  async (ctx) => {
    await generateImage(ctx, {
      images: [{ data: PNG, mime: "image/png", name: "a.png" }, { data: JPEG, mime: "image/jpeg" }],
      n: 2,
      aspect: "16:9",
      resolution: "2k",
    })
    await generateImage(ctx, { resolution: "0.5K", aspect: "1:1" })
    await generateImage(ctx, { resolution: "8K" })
  },
)

scenario(
  "image results are typed from their bytes",
  {
    config,
    responders: [route("/images/generations", json({ data: [{ b64_json: JPEG }, { b64_json: UNKNOWN }, { url: `data:image/webp;base64,${PNG}` }] }))],
  },
  (ctx) => generateImage(ctx, {}),
)

scenario(
  "image results given by URL are downloaded",
  {
    config,
    responders: [
      route(
        "/images/generations",
        json({ data: [{ url: "https://cdn.example.com/a.png" }, { url: "https://cdn.example.com/b" }], output_format: "png" }),
        json({ data: [{ url: "https://cdn.example.com/b" }, { url: "https://cdn.example.com/missing.png" }] }),
        json({ data: [{ url: "https://cdn.example.com/unanswered.png" }] }),
      ),
      (request) =>
        request.url === "https://cdn.example.com/a.png"
          ? { raw: PNG, base64: true, headers: { "content-type": "image/png" } }
          : request.url === "https://cdn.example.com/b"
            ? { raw: JPEG, base64: true, headers: { "content-type": "application/octet-stream" } }
            : request.url === "https://cdn.example.com/missing.png"
              ? { raw: "gone", status: 404, headers: { "content-type": "text/plain" } }
              : undefined,
    ],
  },
  async (ctx) => {
    for (let i = 0; i < 3; i++) await generateImage(ctx, { prompt: `Attempt ${i}` })
  },
)

scenario(
  "invalid image responses",
  {
    config,
    responders: [
      route(
        "/images/generations",
        json({ data: [] }),
        json({ nope: true }),
        json({ data: [{}] }),
        json({ data: [{ b64_json: "%%%" }] }),
        json({ data: [{ url: "data:image/png,notbase64" }] }),
        { raw: "not json", headers: { "content-type": "application/json" } },
      ),
    ],
  },
  async (ctx) => {
    for (let i = 0; i < 6; i++) await generateImage(ctx, { prompt: `Attempt ${i}` })
  },
)

scenario(
  "image provider failures",
  {
    config,
    timeout: 30_000,
    responders: [
      route(
        "/images/generations",
        json({ error: { message: "No auth credentials found" } }, 401),
        json({ error: { message: "Blocked", code: "content_policy_violation" } }, 400),
        json({ error: { message: "Bad size" } }, 400),
        { status: 429, body: { error: { message: "Slow down" } }, headers: { "retry-after": "1" } },
        json({ data: [{ b64_json: PNG }] }),
        { status: 402, body: { error: { message: "Insufficient credits" } } },
      ),
    ],
  },
  async (ctx) => {
    for (let i = 0; i < 5; i++) await generateImage(ctx, { prompt: `Attempt ${i}` })
  },
)

scenario("image requests the API rejects", { config }, async (ctx) => {
  await generateImage(ctx, { aspect: "wide" })
  await generateImage(ctx, { n: 0 })
  await generateImage(ctx, { images: [{ data: "%%", mime: "image/png" }] })
  await ctx.http("POST", "/api/experimental/generate/image", { model: { providerID: "openrouter", id: MODEL_ID }, prompt: "A pixel" })
  await ctx.http("POST", "/api/experimental/generate/image", { model: decision, prompt: "A pixel" })
  await ctx.http("POST", "/api/experimental/generate/image", { model: { providerID: "openrouter", id: "missing/model" }, prompt: "A pixel" })
  await ctx.http("POST", "/api/experimental/generate/image", { model: { ...image, variant: "fast" }, prompt: "A pixel" })
  await ctx.http("POST", "/api/experimental/generate/image", { model: { providerID: "openrouter", id: "acme/chat-package-image" }, prompt: "A pixel" })
  await ctx.http("POST", "/api/experimental/generate/decisions", {
    model: { providerID: "openrouter", id: "acme/image-package-decision" },
    input: [{ type: "text", text: "Hello" }],
    questions: [questions[0]],
  })
})

scenario(
  "decisions answer predicate, choice and score questions",
  { config, responders: [route("/systemone", json(answers))] },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/generate/decisions", {
      model: decision,
      input: [{ type: "text", text: "Buy cheap watches now!!!" }],
      questions,
    })
  },
)

scenario(
  "decisions take text, JSON and media input",
  {
    config,
    responders: [
      route(
        "/systemone",
        json({ model: DECISION_MODEL, answers: { spam: { type: "noul", noul: 0.5 } } }),
        json({ model: DECISION_MODEL, answers: { spam: { type: "noul", noul: 0.25 } } }),
        json({ model: DECISION_MODEL, answers: { spam: { type: "noul", noul: 0 } } }),
      ),
    ],
  },
  async (ctx) => {
    const ask = (input: unknown[]) =>
      ctx.http("POST", "/api/experimental/generate/decisions", { model: decision, input, questions: [questions[0]] })
    await ask([{ type: "json", value: { subject: "Offer", links: 12 } }])
    await ask([
      { type: "text", text: "Is this image spam?" },
      { type: "file", data: PNG, mime: "image/png", name: "ad.png" },
      { type: "json", value: [1, 2] },
    ])
    await ask([
      { type: "file", data: Buffer.from("ID3audio").toString("base64"), mime: "audio/mpeg" },
      { type: "file", data: PNG, mime: "video/mp4" },
    ])
    await ask([{ type: "file", data: Buffer.from("%PDF-1.4").toString("base64"), mime: "application/pdf", name: "doc.pdf" }])
  },
)

scenario(
  "decision failures",
  {
    config,
    timeout: 30_000,
    responders: [
      route(
        "/systemone",
        json({ model: DECISION_MODEL, answers: { other: { type: "noul", noul: 0.5 } } }),
        json({ model: DECISION_MODEL, answers: { spam: { type: "noul", noul: 1.5 } } }),
        json({ answers: {} }),
        json({ error: { message: "No auth credentials found" } }, 401),
        { status: 503, body: { error: { message: "Overloaded" } } },
        json({ model: DECISION_MODEL, answers: { spam: { type: "noul", noul: 0.5 } } }),
      ),
    ],
  },
  async (ctx) => {
    const ask = () =>
      ctx.http("POST", "/api/experimental/generate/decisions", {
        model: decision,
        input: [{ type: "text", text: "Hello" }],
        questions: [questions[0]],
      })
    for (let i = 0; i < 5; i++) await ask()
    // Options are map keys: `true` and "true" would collide.
    await ctx.http("POST", "/api/experimental/generate/decisions", {
      model: decision,
      input: [{ type: "text", text: "Hello" }],
      questions: [{ type: "choice", name: "pick", instructions: "Pick", choices: [{ value: true }, { value: "true" }] }],
    })
    await ctx.http("POST", "/api/experimental/generate/decisions", {
      model: image,
      input: [{ type: "text", text: "Hello" }],
      questions: [questions[0]],
    })
  },
)
