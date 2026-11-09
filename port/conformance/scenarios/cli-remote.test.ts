// Remote access through OpenTunnel: `opencode service set remote true` makes sure the device has its tunnel (creating
// it through the OpenTunnel API, which the fake network answers here, with a certificate for a key made on the device)
// before the service may expose itself, and keeps a random route for the service. The tunnel identity lives in the
// OpenTunnel SDK's storage under the sandbox's XDG_DATA_HOME. The service itself is never started with remote access
// on: its tunnel would keep reconnecting to the bridge, which the fake network does not serve.
import { createPrivateKey, createPublicKey, verify } from "node:crypto"
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import type { ProviderRequest, Turn } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"

const CONFIG = "config/opencode/service-conformance.json"
const STORE = "data/opentunnel/default"
const API = "https://opentunnel.xyz/api/tunnel"
const HOSTNAME = "k7x2q.tunnel.example"
const CERTIFICATE = "-----BEGIN CERTIFICATE-----\nMIIBconformance\n-----END CERTIFICATE-----\n"
const RENEWED = "-----BEGIN CERTIFICATE-----\nMIIBrenewed\n-----END CERTIFICATE-----\n"
const CHAIN = "-----BEGIN CERTIFICATE-----\nMIIBchain\n-----END CERTIFICATE-----\n"
const STORED_KEY = "-----BEGIN PRIVATE KEY-----\nstored-key\n-----END PRIVATE KEY-----\n"

const ready = (certificate = CERTIFICATE, expiry = "2099-01-01T00:00:00Z"): Turn => ({
  status: 200,
  body: { id: "cert_conformance", state: { type: "ready", certificate, chain: CHAIN, expiry } },
})
const tagged = (status: number, tag: string, message: string, extra: Record<string, unknown> = {}): Turn => ({
  status,
  body: { _tag: tag, message, ...extra },
})
/** A transient failure the SDK retries at once (`Retry-After: 0`). */
const busy = (status: number): Turn => ({ raw: "busy", status, headers: { "content-type": "text/plain", "retry-after": "0" } })

/** OpenTunnel API answers by method and path below /api/tunnel, in order: the last one repeats. */
function tunnelApi(routes: Record<string, readonly Turn[]>) {
  const served = new Map<string, number>()
  return (request: ProviderRequest): Turn | undefined => {
    if (!request.url.startsWith(API)) return undefined
    const key = `${request.method} ${request.url.slice(API.length) || "/"}`
    const answers = routes[key]
    if (!answers) return { status: 404, body: { error: `conformance: no answer for ${key}` } }
    const index = served.get(key) ?? 0
    served.set(key, index + 1)
    return answers[Math.min(index, answers.length - 1)]
  }
}

/** A tunnel identity in the OpenTunnel storage, as the SDK saves it. */
function seed(ctx: CliCtx, options: { expiry?: string; omit?: string } = {}) {
  const root = path.join(ctx.sandbox, STORE)
  mkdirSync(root, { recursive: true })
  const files: Record<string, string> = {
    "tunnel.json": JSON.stringify({ id: "tun_conformance", hostname: HOSTNAME, certificateExpiry: options.expiry ?? "2099-01-01T00:00:00.000Z" }, null, 2),
    token: "tok_conformance\n",
    "private-key.pem": STORED_KEY,
    "certificate.pem": CERTIFICATE,
    "chain.pem": CHAIN,
  }
  for (const [name, content] of Object.entries(files)) if (name !== options.omit) writeFileSync(path.join(root, name), content, { mode: 0o600 })
}

function store(ctx: CliCtx, ...names: string[]) {
  for (const name of names) ctx.file(`${STORE}/${name}`)
}

/** The route the service config keeps is 64 random bits as 16 hex characters. */
function routes(ctx: CliCtx) {
  ctx.scrub((text) => (/^[0-9a-f]{16}$/.test(text) ? "<route>" : text.replace(/("route": ")([0-9a-f]{16})(")/g, "$1<route>$3")))
}

// --- certificate requests and keys made on the device ---

type Node = { tag: number; start: number; header: number; end: number }

/** The DER elements directly inside `bytes[from, to)`. */
function children(bytes: Uint8Array, from: number, to: number): Node[] {
  const out: Node[] = []
  let at = from
  while (at < to) {
    const tag = bytes[at]
    let length = bytes[at + 1]
    let header = 2
    if (length & 0x80) {
      const count = length & 0x7f
      length = 0
      for (let i = 0; i < count; i++) length = length * 256 + bytes[at + 2 + i]
      header += count
    }
    out.push({ tag, start: at, header, end: at + header + length })
    at += header + length
  }
  if (at !== to) throw new Error("DER overrun")
  return out
}

const body = (bytes: Uint8Array, node: Node) => bytes.subarray(node.start + node.header, node.end)
const raw = (bytes: Uint8Array, node: Node) => bytes.subarray(node.start, node.end)

function der(tag: number, ...contents: Uint8Array[]) {
  const content = Buffer.concat(contents)
  const length = content.length < 0x80 ? [content.length] : (() => {
    const out: number[] = []
    for (let n = content.length; n > 0; n >>= 8) out.unshift(n & 0xff)
    return [0x80 | out.length, ...out]
  })()
  return new Uint8Array(Buffer.concat([Buffer.from([tag, ...length]), content]))
}
const oid = (hex: string) => der(0x06, Buffer.from(hex, "hex"))
const text = (tag: number, value: string) => der(tag, Buffer.from(value, "utf8"))

/** The CertificationRequestInfo the SDK signs: CN and SANs for the hostname and its wildcard. */
function expectedInfo(hostname: string, spki: Uint8Array) {
  return der(
    0x30,
    der(0x02, Buffer.from([0])),
    der(0x30, der(0x31, der(0x30, oid("550403"), text(0x0c, hostname)))),
    spki,
    der(0xa0, der(0x30, oid("2a864886f70d01090e"), der(0x31, der(0x30, der(0x30, oid("551d11"), der(0x04, der(0x30, text(0x82, hostname), text(0x82, `*.${hostname}`)))))))),
  )
}

const pem = (label: string, der: Uint8Array) =>
  `-----BEGIN ${label}-----\n${Buffer.from(der).toString("base64").replace(/.{1,64}/g, "$&\n")}-----END ${label}-----`

/**
 * Certificate requests and private keys in recorded text become `<csr N for HOST>` and `<private key N>`, numbered by
 * their public key in order of appearance (a key and the request it signed share a number), but only when the request
 * is exactly the SDK's (PEM layout, structure, P-256 key, CN and SANs, a valid ECDSA-SHA256 signature) and the key is a
 * P-256 PKCS#8 key in the SDK's PEM layout. Anything else stays as it is.
 */
function keys(ctx: CliCtx, hostname: string) {
  const seen: string[] = []
  const number = (spki: Uint8Array) => {
    const hex = Buffer.from(spki).toString("hex")
    if (!seen.includes(hex)) seen.push(hex)
    return seen.indexOf(hex) + 1
  }
  ctx.scrub((input) =>
    input
      .replace(/-----BEGIN CERTIFICATE REQUEST-----(?:\n|\\n)[A-Za-z0-9+/=\n\\]+?-----END CERTIFICATE REQUEST-----/g, (found) => {
        // Inside a JSON file recorded as text, the line breaks are escaped.
        const escaped = found.includes("\\n")
        const request = escaped ? found.replaceAll("\\n", "\n") : found
        try {
          const bytes = new Uint8Array(Buffer.from(request.split("\n").slice(1, -1).join(""), "base64"))
          if (pem("CERTIFICATE REQUEST", bytes) !== request) return found
          const [outer] = children(bytes, 0, bytes.length)
          const [info, algorithm, signature] = children(bytes, outer.start + outer.header, outer.end)
          const [, , spki] = children(bytes, info.start + info.header, info.end)
          const key = createPublicKey({ key: Buffer.from(raw(bytes, spki)), format: "der", type: "spki" })
          if (key.asymmetricKeyDetails?.namedCurve !== "prime256v1") return found
          if (!Buffer.from(raw(bytes, info)).equals(Buffer.from(expectedInfo(hostname, raw(bytes, spki))))) return found
          if (!Buffer.from(raw(bytes, algorithm)).equals(Buffer.from(der(0x30, oid("2a8648ce3d040302"))))) return found
          const bits = body(bytes, signature)
          if (bits[0] !== 0) return found
          if (!verify("sha256", raw(bytes, info), key, bits.subarray(1))) return found
          return `<csr ${number(raw(bytes, spki))} for ${hostname}>`
        } catch {
          return found
        }
      })
      .replace(/-----BEGIN PRIVATE KEY-----\n[A-Za-z0-9+/=\n]+-----END PRIVATE KEY-----\n/g, (block) => {
        try {
          const bytes = new Uint8Array(Buffer.from(block.split("\n").slice(1, -2).join(""), "base64"))
          if (`${pem("PRIVATE KEY", bytes)}\n` !== block) return block
          const key = createPrivateKey({ key: Buffer.from(bytes), format: "der", type: "pkcs8" })
          if (key.asymmetricKeyDetails?.namedCurve !== "prime256v1") return block
          const spki = createPublicKey(key).export({ type: "spki", format: "der" })
          return `<private key ${number(new Uint8Array(spki))}>\n`
        } catch {
          return block
        }
      }),
  )
}

cliScenario(
  "remote access with the device's tunnel",
  { responders: [tunnelApi({ "GET /tun_conformance/certificate": [ready()] })] },
  async (ctx) => {
    routes(ctx)
    seed(ctx)
    await ctx.cli(["service", "get", "remote"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "get", "remote"])
    await ctx.cli(["service", "get"])
    ctx.file(CONFIG)
    // Turning it on again keeps the route.
    await ctx.cli(["service", "set", "remote", "true"])
    ctx.file(CONFIG)
    await ctx.cli(["service", "set", "remote", "false"])
    await ctx.cli(["service", "get", "remote"])
    ctx.file(CONFIG)
    // A new route after it was off; unset turns it off as well.
    await ctx.cli(["service", "set", "remote", "true"])
    ctx.file(CONFIG)
    await ctx.cli(["service", "unset", "remote"])
    await ctx.cli(["service", "get"])
    ctx.file(CONFIG)
    store(ctx, "tunnel.json", "certificate.pem")
  },
)

cliScenario(
  "remote access saves a renewed certificate",
  { responders: [tunnelApi({ "GET /tun_conformance/certificate": [ready(RENEWED, "2099-06-30T12:00:00Z")] })] },
  async (ctx) => {
    routes(ctx)
    seed(ctx)
    await ctx.cli(["service", "set", "remote", "true"])
    store(ctx, "tunnel.json", "token", "private-key.pem", "certificate.pem", "chain.pem")
    ctx.file(CONFIG)
  },
)

cliScenario(
  "remote access keeps an unexpired certificate while the API is unavailable",
  { responders: [tunnelApi({ "GET /tun_conformance/certificate": [busy(503)] })] },
  async (ctx) => {
    routes(ctx)
    seed(ctx)
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "get", "remote"])
  },
)

cliScenario(
  "remote access fails when the certificate cannot be checked",
  {
    responders: [
      tunnelApi({
        "GET /tun_conformance/certificate": [
          busy(503),
          busy(503),
          busy(503),
          busy(503),
          busy(503),
          busy(503),
          busy(503),
          tagged(401, "UnauthorizedError", "Invalid tunnel token"),
          tagged(404, "CertificateNotFoundError", "No certificate yet", { tunnelID: "tun_conformance" }),
          { status: 418, body: {} },
          { status: 200, body: { id: "cert_conformance", state: { type: "unknown" } } },
        ],
      }),
    ],
  },
  async (ctx) => {
    routes(ctx)
    // An expired certificate cannot outlast the API.
    seed(ctx, { expiry: "2020-01-01T00:00:00.000Z" })
    // Retried, then given up on; then answers that are not retried: an API error, an undeclared status, a body that
    // does not decode.
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "get", "remote"])
    ctx.file(CONFIG)
  },
)

cliScenario("remote access with an incomplete tunnel identity", {}, async (ctx) => {
  seed(ctx, { omit: "token" })
  await ctx.cli(["service", "set", "remote", "true"])
  await ctx.cli(["service", "get", "remote"])
})

cliScenario(
  "remote access creates the device's tunnel",
  {
    responders: [
      tunnelApi({
        "POST /": [{ status: 201, body: { tunnel: { id: "tun_created", hostname: HOSTNAME, state: "offline" }, token: "tok_created" } }],
        "POST /tun_created/certificate": [{ status: 202, body: { id: "cert_created", state: { type: "issuing" } } }],
        "GET /tun_created/certificate": [
          { status: 200, body: { id: "cert_created", state: { type: "challenge", token: "challenge-token", key: "challenge-key" } } },
          ready(CERTIFICATE, "2099-03-01T00:00:00Z"),
        ],
      }),
    ],
  },
  async (ctx) => {
    routes(ctx)
    keys(ctx, HOSTNAME)
    await ctx.cli(["service", "set", "remote", "true"])
    store(ctx, "tunnel.json", "token", "private-key.pem", "certificate.pem", "chain.pem", "pending.json")
    await ctx.cli(["service", "get", "remote"])
    ctx.file(CONFIG)
    // The tunnel exists now: turning remote access on again only checks its certificate.
    await ctx.cli(["service", "set", "remote", "false"])
  },
)

cliScenario(
  "remote access resumes an interrupted certificate issuance",
  {
    responders: [
      tunnelApi({
        "POST /": [{ status: 201, body: { tunnel: { id: "tun_created", hostname: HOSTNAME, state: "offline" }, token: "tok_created" } }],
        "POST /tun_created/certificate": [
          { status: 202, body: { id: "cert_created", state: { type: "issuing" } } },
          tagged(409, "CertificateInProgressError", "Already issuing", { tunnelID: "tun_created" }),
        ],
        "GET /tun_created/certificate": [
          { status: 200, body: { id: "cert_created", state: { type: "failed", reason: "DNS challenge timed out" } } },
          ready(CERTIFICATE, "2099-03-01T00:00:00Z"),
        ],
      }),
    ],
  },
  async (ctx) => {
    routes(ctx)
    keys(ctx, HOSTNAME)
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "get", "remote"])
    store(ctx, "pending.json", "token", "private-key.pem", "tunnel.json")
    // The request already made is sent again with the same key; the issuance in flight is waited for.
    await ctx.cli(["service", "set", "remote", "true"])
    store(ctx, "pending.json", "tunnel.json", "private-key.pem", "certificate.pem")
    await ctx.cli(["service", "get", "remote"])
  },
)

cliScenario(
  "remote access when the tunnel cannot be created",
  {
    responders: [
      tunnelApi({
        "POST /": [
          tagged(400, "InvalidRequestError", "Tunnel names are no longer supported"),
          tagged(409, "HostnameUnavailableError", "Taken", { name: "k7x2q" }),
          { status: 500, body: { error: "boom" } },
          busy(522),
          busy(522),
          busy(522),
          busy(522),
          busy(522),
          busy(522),
          busy(522),
          busy(429),
          { status: 201, body: { tunnel: { id: "tun_created", hostname: HOSTNAME, state: "offline" }, token: "tok_created" } },
        ],
        "POST /tun_created/certificate": [tagged(400, "InvalidHostnameError", "Wrong hostname", { provided: "a", expected: "b" })],
      }),
    ],
  },
  async (ctx) => {
    routes(ctx)
    keys(ctx, HOSTNAME)
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "set", "remote", "true"])
    // Cloudflare's statuses are retried, then reported with their meaning.
    await ctx.cli(["service", "set", "remote", "true"])
    // Created, but the certificate request is refused.
    await ctx.cli(["service", "set", "remote", "true"])
    await ctx.cli(["service", "get", "remote"])
    store(ctx, "pending.json", "tunnel.json")
    ctx.file(CONFIG)
  },
)
