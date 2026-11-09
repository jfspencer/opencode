// Raw JSON request bodies as HttpApiBuilder reads them (effect/src/http-api/HttpApiBuilder.ts `decodePayload`):
// the body is the request's text (UTF-8, invalid bytes replaced, a leading BOM dropped), only an empty text is "no
// body", and anything JSON.parse refuses is `Expected a valid JSON body` (an InvalidRequestError of kind Payload).
import { scenario, type Ctx } from "../lib/scenario"

const post = async (ctx: Ctx, label: string, body: string | Uint8Array) => {
  const response = await ctx.handler(
    new Request(new URL("/api/session", "http://opencode.local"), {
      method: "POST",
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project), "content-type": "application/json" },
      body,
    }),
  )
  const text = await response.text()
  let parsed: unknown = text
  try {
    parsed = JSON.parse(text)
  } catch {}
  const summary =
    parsed && typeof parsed === "object" && "id" in parsed
      ? { title: (parsed as { title?: unknown }).title ?? null }
      : parsed
  ctx.note(label, { status: response.status, body: summary })
}

const bytes = (...parts: Array<string | number[]>) =>
  new Uint8Array(parts.flatMap((part) => (typeof part === "string" ? [...new TextEncoder().encode(part)] : part)))

scenario("raw JSON bodies", {}, async (ctx) => {
  await post(ctx, "empty", "")
  await post(ctx, "only white space", " \n\t ")
  await post(ctx, "white space around", "\r\n\t{\"title\":\"spaced\"}\n ")
  await post(ctx, "a byte order mark", bytes([0xef, 0xbb, 0xbf], '{"title":"bom"}'))
  await post(ctx, "two byte order marks", bytes([0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf], '{"title":"bom"}'))
  await post(ctx, "invalid UTF-8 in a string", bytes('{"title":"a', [0xff, 0xc3], 'b"}'))
  await post(ctx, "invalid UTF-8 outside a string", bytes('{"title":"x"}', [0xff]))
  await post(ctx, "null", "null")
  await post(ctx, "trailing text", '{"title":"x"} x')
  await post(ctx, "a non-breaking space", ' {"title":"nbsp"}')
  await post(ctx, "a surrogate pair escape", '{"title":"\\ud83d\\ude00"}')
  await ctx.http("GET", "/api/session")
})
