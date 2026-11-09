// webfetch status errors name the request (`StatusCode: non 2xx status code (<status> GET <url>)`, Effect's
// HttpClientError). Effect redacts the values of signed-URL query parameters there (`UrlParams.redactUrl` with the
// OpenTelemetry `url.full` names: X-Amz-Signature, X-Amz-Credential, X-Amz-Security-Token, AWSAccessKeyId, Signature,
// sig, X-Goog-Signature; names compared decoded and case-insensitively), so a presigned link never reaches the model.
import { call, say, type Responder } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const denied: Responder = (request) =>
  request.kind === "other" && request.url.startsWith("https://files.example.com/")
    ? { raw: "denied", status: 403, headers: { "content-type": "text/plain" } }
    : undefined

const redirect: Responder = (request) =>
  request.kind === "other" && request.url.startsWith("https://short.example.com/")
    ? { raw: "", status: 302, headers: { location: "https://files.example.com/moved?X-Amz-Signature=moved-secret&id=7" } }
    : undefined

async function run(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "signed" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}/message`)
}

const urls = [
  "https://files.example.com/report.pdf?X-Amz-Algorithm=AWS4&X-Amz-Credential=AKIA%2F20260101&X-Amz-Signature=abc123&size=1",
  "https://files.example.com/a?sig=s1&SIG=s2&signature=s3&AWSAccessKeyId=k&X-Goog-Signature=g&X-Amz-Security-Token=t&keep=me",
  "https://files.example.com/b?X%2DAmz%2DSignature=encoded&X-Amz-Signature+=spaced&Signature&Signature=&%E0%A4%A=bad",
  "https://files.example.com/c?Signature=before#frag?Signature=after",
  "https://files.example.com/d#frag?Signature=fragment-only",
  "https://files.example.com/e?a=1&&Signature=x&=y&b",
]

scenario(
  "signed query parameters are redacted in status errors",
  { responders: [denied], turns: [...urls.map((url) => call("webfetch", { url })), say("Denied.")] },
  (ctx) => run(ctx, "Fetch the presigned links"),
)

scenario(
  "a redirect to a signed URL keeps the requested URL in the error",
  { responders: [redirect, denied], turns: [call("webfetch", { url: "https://short.example.com/r?Signature=short-secret" }), say("Denied.")] },
  (ctx) => run(ctx, "Fetch the short link"),
)
