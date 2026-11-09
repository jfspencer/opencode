// `opencode pair`: one-time links (and a QR code for addresses another device can reach) to connect a browser or app
// to the background service, which the command starts on the sandbox's pinned port. Remote pairing asks for consent
// before it turns remote access on; the tunnel it would then wait for is not served by the fake network, so the
// scenarios stop where the tunnel would be created.
import { renderUnicodeCompact } from "../ref/packages/util/src/qr"
import type { ProviderRequest, Turn } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"

const CONFIG = "config/opencode/service-conformance.json"

/**
 * Pairing codes are 16 random bytes in base64url: each one in a link becomes `<pairing code>`, and the QR code beside
 * it is replaced with a description of its payload only when it is exactly the reference's rendering of
 * `{"code":…,"urls":…}` for one of the given URL lists (indented by two spaces).
 */
function pairing(ctx: CliCtx, remotes: readonly (readonly string[])[]) {
  ctx.scrub((text) => {
    let out = text
    for (const [, code] of text.matchAll(/\/auth\/connect\/([A-Za-z0-9_-]{22})(?![A-Za-z0-9_-])/g)) {
      for (const urls of remotes) {
        const qr = renderUnicodeCompact(JSON.stringify({ code, urls }), { border: 2 })
          .split("\n")
          .map((line) => `  ${line}`)
          .join("\n")
        out = out.split(qr).join(`  <QR code of {"code":"<pairing code>","urls":${JSON.stringify(urls)}}>`)
      }
      out = out.split(code).join("<pairing code>")
    }
    return out
  })
}

/** The OpenTunnel API refuses to create a tunnel. */
const refused = (request: ProviderRequest): Turn | undefined =>
  request.url === "https://opentunnel.xyz/api/tunnel" && request.method === "POST"
    ? { status: 400, body: { _tag: "InvalidRequestError", message: "Tunnel creation is closed" } }
    : undefined

cliScenario("pair with the local service", {}, async (ctx) => {
  pairing(ctx, [])
  await ctx.cli(["pair"])
  await ctx.cli(["service", "status"])
  // A second pairing reuses the running service.
  await ctx.cli(["pair"])
})

cliScenario("pair through an external URL", {}, async (ctx) => {
  pairing(ctx, [["https://opencode.example.test/base"], ["http://192.168.1.20:4096"]])
  await ctx.cli(["pair", "--url", "https://opencode.example.test/base///"])
  await ctx.cli(["pair", "--url", "http://192.168.1.20:4096"])
  // Loopback addresses get links but no QR code.
  await ctx.cli(["pair", "--url", "http://localhost:4096/"])
  await ctx.cli(["pair", "--url", "http://127.0.0.2:4096"])
})

cliScenario("pair URL validation", {}, async (ctx) => {
  await ctx.parallel([
    ["pair", "--url", "ftp://files.example.test"],
    ["pair", "--url", "https://user:secret@opencode.example.test"],
    ["pair", "--url", "https://opencode.example.test/?token=1"],
    ["pair", "--url", "https://opencode.example.test/#fragment"],
    ["pair", "--url", "not a url"],
    ["pair", "--url", ""],
    ["pair", "--url"],
  ])
  await ctx.cli(["service", "status"])
})

cliScenario("pair refuses conflicting flags and a disabled service", {}, async (ctx) => {
  await ctx.cli(["pair", "--remote", "--url", "https://opencode.example.test"])
  await ctx.cli(["service", "set", "disabled", "true"])
  await ctx.cli(["pair"])
  await ctx.cli(["pair", "--remote", "--url", "https://opencode.example.test"])
  await ctx.cli(["service", "status"])
})

cliScenario("remote pairing needs consent off a terminal", { responders: [refused] }, async (ctx) => {
  await ctx.cli(["pair", "--remote"])
  ctx.file(CONFIG)
  // --yes accepts; the tunnel is then set up first, and here its creation fails.
  await ctx.cli(["pair", "--remote", "--yes"])
  ctx.file(CONFIG)
  // Accepted once, it is not asked again.
  await ctx.cli(["pair", "--remote"])
  await ctx.cli(["service", "status"])
})

cliScenario("remote pairing consent on a terminal", {}, async (ctx) => {
  // Declined: arrows move to No.
  await ctx.terminal(["pair", "--remote"], [
    { waitFor: "Expose your server to the internet?", type: "\x1b[C" },
    { waitFor: "● No", type: "\r" },
  ])
  ctx.file(CONFIG)
  // Cancelled.
  await ctx.terminal(["pair", "--remote"], [{ waitFor: "Expose your server to the internet?", type: "\x03" }])
  ctx.file(CONFIG)
  // Declined with n. (Accepting goes on to set up the tunnel, as `--yes` does off a terminal.)
  await ctx.terminal(["pair", "--remote"], [{ waitFor: "Expose your server to the internet?", type: "n" }])
  ctx.file(CONFIG)
  await ctx.cli(["service", "status"])
})
