// The schemas the generator covers, collected from the TS reference at runtime, in declaration order:
// every HttpApi endpoint (path params, query, headers, payloads, successes, errors), the SSE event union and
// every event definition, and the v2 config file. Each root records the wire context it is read and written in.
import { makeDefaultApi } from "@opencode/protocol/api"
import { ClientApi } from "@opencode/protocol/client"
import { OpenCodeEvent } from "@opencode/protocol/groups/event"
import { EventManifest } from "@opencode/schema/event-manifest"
import { Config } from "@opencode/schema/config"
import { Context, Schema, SchemaAST } from "effect"
import { HttpApi, HttpApiEndpoint, HttpApiMiddleware, HttpApiSchema, OpenApi } from "effect/http-api"

/**
 * How a value travels:
 * - `json`: an HTTP body. HttpApi has already applied `Schema.toCodecJson`; the encoded value goes through
 *   `JSON.stringify` / `JSON.parse`.
 * - `plain`: the plain encoded side through `JSON.stringify` / `JSON.parse`, without the JSON codec: SSE event
 *   data (`Schema.fromJsonString`) and the config file (decoded from JSONC).
 * - `tree`: path parameters, query and headers. HttpApi has applied `Schema.toCodecStringTree`; values are strings
 *   or arrays of strings.
 */
export type Wire = "json" | "plain" | "tree"

export interface Root {
  /** Suggested Rust name, used when the schema has no identifier of its own. */
  readonly hint: string
  readonly ast: SchemaAST.AST
  readonly wire: Wire
  /** Where the root came from, for diagnostics and the route table. */
  readonly origin: string
  /**
   * What HttpApi actually decodes a request part with, when that is not `ast` itself: a payload goes through
   * `Schema.Union(payloadSchemas)` (effect/http-api/HttpApiBuilder.ts, `buildPayloadDecoders`). The union of one
   * member must report like its member (the generator checks), so `ast` gives the Rust type and this the TS truth.
   */
  readonly decode?: SchemaAST.AST
}

export interface Body {
  readonly status: number
  /** `json`, `none` (no content), `binary` (octet stream), `text`, `sse`, `upgrade` */
  readonly kind: string
  readonly contentType?: string
  readonly root?: Root
}

export interface Endpoint {
  readonly group: string
  readonly name: string
  readonly method: string
  readonly path: string
  readonly operationId: string
  readonly params?: Root
  readonly query?: Root
  readonly headers?: Root
  readonly payload: ReadonlyArray<Body>
  readonly success: ReadonlyArray<Body>
  readonly errors: ReadonlyArray<Body>
  /** The location middleware the endpoint runs behind, if any (packages/server/src/{location,middleware/*}.ts). */
  readonly middleware: Middleware
}

/** `location`: the request's location; `session`: the session's location; `form`: the session's, or `global`. */
export type Middleware = "none" | "location" | "session" | "form"

// The client API gives forms and sessions one middleware identity; the server's are distinct. The API built with
// three distinct identities tells, per endpoint, which one the server places it behind.
class Location extends HttpApiMiddleware.Service<Location>()("port/LocationMiddleware") {}
class FormLocation extends HttpApiMiddleware.Service<FormLocation>()("port/FormLocationMiddleware") {}
class SessionLocation extends HttpApiMiddleware.Service<SessionLocation>()("port/SessionLocationMiddleware") {}

function middlewares(): Map<string, Middleware> {
  const api = makeDefaultApi({
    locationMiddleware: Location,
    formLocationMiddleware: FormLocation,
    sessionLocationMiddleware: SessionLocation,
  })
  const kinds = new Map<string, Middleware>([
    [Location.key, "location"],
    [FormLocation.key, "form"],
    [SessionLocation.key, "session"],
  ])
  const out = new Map<string, Middleware>()
  HttpApi.reflect(api as never, {
    onGroup() {},
    onEndpoint({ endpoint }) {
      const e = endpoint as HttpApiEndpoint.Top
      const found = [...e.middlewares].flatMap((m) => (kinds.has(m.key) ? [kinds.get(m.key)!] : []))
      if (found.length > 1) throw new Error(`several location middlewares at ${e.method} ${e.path}`)
      out.set(`${e.method} ${e.path}`, found[0] ?? "none")
    },
  })
  return out
}

const pascal = (s: string) =>
  s
    .split(/[^A-Za-z0-9]+/)
    .filter((p) => p.length > 0)
    .map((p) => p[0].toUpperCase() + p.slice(1))
    .join("")

export function collect() {
  const endpoints: Endpoint[] = []
  const roots: Root[] = []
  const add = (root: Root) => (roots.push(root), root)
  const middleware = middlewares()
  HttpApi.reflect(ClientApi as never, {
    onGroup() {},
    onEndpoint({ group, endpoint }) {
      const e = endpoint as HttpApiEndpoint.Top
      const operationId = Context.getOrElse(e.annotations, OpenApi.Identifier, () => e.identifier)
      const base = pascal(operationId)
      const origin = `${e.method} ${e.path}`
      const params = e.params && add({ hint: `${base}Params`, ast: e.params.ast, wire: "tree", origin: `${origin} params` })
      // HttpApi decodes the query with `toCodecArrayFromSingle(query)`: an array property also takes one string.
      const query =
        e.query &&
        add({ hint: `${base}Query`, ast: Schema.toCodecArrayFromSingle(e.query as never).ast, wire: "tree", origin: `${origin} query` })
      const headers =
        e.headers && add({ hint: `${base}Headers`, ast: e.headers.ast, wire: "tree", origin: `${origin} headers` })
      const payload: Body[] = []
      const hasBody = !["GET", "HEAD", "OPTIONS"].includes(e.method)
      for (const [contentType, entry] of e.payload) {
        const schemas = entry.schemas.filter((schema) => !HttpApiSchema.isNoContent(schema.ast))
        if (schemas.length > 1) throw new Error(`several payload schemas for ${contentType} at ${origin}`)
        for (const schema of schemas) {
          const kind = entry.encoding._tag === "Json" ? "json" : entry.encoding._tag === "Uint8Array" ? "binary" : "text"
          // GET-style payloads are query parameters (HttpApi decodes them from the query string, like a query).
          const wire = hasBody ? (kind === "json" ? "json" : "plain") : "tree"
          const ast = hasBody ? schema.ast : Schema.toCodecArrayFromSingle(schema as never).ast
          const decode = hasBody
            ? Schema.Union(entry.schemas as never).ast
            : Schema.toCodecArrayFromSingle(Schema.Union(entry.schemas as never)).ast
          payload.push({
            status: 0,
            kind: hasBody ? kind : "query",
            contentType,
            root:
              kind === "json" || !hasBody
                ? add({ hint: `${base}Payload`, ast, wire, origin: `${origin} payload`, decode })
                : undefined,
          })
        }
      }
      const success = bodies(HttpApiEndpoint.getSuccessSchemas(e), HttpApiSchema.getStatusSuccessSchema, `${base}Response`, origin, add)
      const errors = bodies(
        HttpApiEndpoint.getErrorSchemas(e),
        HttpApiSchema.getStatusErrorSchema,
        `${base}Error`,
        origin,
        add,
      )
      endpoints.push({
        group: group.identifier,
        name: e.identifier,
        method: e.method,
        path: e.path,
        operationId,
        params,
        query,
        headers,
        payload,
        success,
        errors,
        middleware: middleware.get(origin) ?? (() => {
          throw new Error(`no middleware entry for ${origin}`)
        })(),
      })
    },
  })
  const sse = add({ hint: "V2Event", ast: OpenCodeEvent.ast, wire: "plain", origin: "GET /api/event (SSE data)" })
  const events = EventManifest.Definitions.map((definition) => {
    const d = definition as unknown as { type: string; durable?: { version: number; aggregate: string }; ast: SchemaAST.AST }
    return {
      type: d.type,
      version: d.durable?.version,
      aggregate: d.durable?.aggregate,
      root: add({ hint: pascal(d.type), ast: d.ast, wire: "plain", origin: `event ${d.type}` }),
    }
  })
  const config = add({ hint: "ConfigInfo", ast: Config.Info.ast, wire: "plain", origin: "opencode.json" })
  return { endpoints, roots, sse, events, config }
}

function bodies(
  schemas: ReadonlyArray<{ readonly ast: SchemaAST.AST }>,
  status: (schema: never) => number,
  hint: string,
  origin: string,
  add: (root: Root) => Root,
): Body[] {
  return schemas.map((schema) => {
    const s = schema as unknown as { ast: SchemaAST.AST; schema?: unknown; events?: { ast: SchemaAST.AST } }
    const code = status(schema as never)
    if (HttpApiSchema.isStreamSchema(s as never)) {
      // SSE in "data" mode: each event's data is `Schema.fromJsonString(data)`; the generated type is `data`.
      const stream = s as unknown as { _tag: string; events: { fields: { data: { ast: SchemaAST.AST } } } }
      if (stream._tag !== "StreamSse") return { status: 200, kind: "stream" }
      const data = stream.events.fields.data.ast
      const links = data.encoding!
      const inner = SchemaAST.replaceEncoding(data, links.length > 1 ? (links.slice(0, -1) as never) : undefined)
      const root = add({ hint: `${hint}Event`, ast: inner, wire: "plain", origin: `${origin} SSE data` })
      return { status: 200, kind: "sse", contentType: "text/event-stream", root }
    }
    if (HttpApiSchema.isUpgrade(s as never)) return { status: 101, kind: "upgrade" }
    if (HttpApiSchema.isNoContent(s.ast)) return { status: code, kind: "none" }
    const encoding = HttpApiSchema.getResponseEncodingSchema(schema as never)
    if (encoding._tag !== "Json") return { status: code, kind: encoding._tag === "Uint8Array" ? "binary" : "text", contentType: encoding.contentType }
    return { status: code, kind: "json", contentType: encoding.contentType, root: add({ hint, ast: s.ast, wire: "json", origin: `${origin} ${code}` }) }
  })
}
