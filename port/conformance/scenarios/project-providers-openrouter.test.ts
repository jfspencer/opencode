// The OpenRouter package's AI SDK spellings in provider `settings`, moved to the request by `AISDKNative.rewrite`
// (packages/core/src/aisdk-native.ts `openRouterRequest`): `appName` and `appUrl` become headers, and `api_keys`, a
// lenient record of strings, becomes `X-Provider-API-Keys` (its JSON) when it has any key. The listed models show the
// result.
import { scenario } from "../lib/scenario"

const cases: Record<string, Record<string, unknown>> = {
  "api keys": { api_keys: { anthropic: "sk-a", "2": "two", "1": "one" } },
  "api keys beside the app headers": { appName: "My App", appUrl: "https://example.com", api_keys: { openai: "sk-o" } },
  "empty api keys": { api_keys: {} },
  "api keys that are not all strings": { api_keys: { openai: "sk-o", bad: 1 } },
  "api keys that are not a record": { api_keys: ["sk-o"] },
  "api keys with headers of their own": { headers: { "X-Provider-API-Keys": "own", "X-Other": "o" }, api_keys: { openai: "sk-o" } },
}

for (const [name, settings] of Object.entries(cases))
  scenario(`openrouter settings: ${name}`, { config: { providers: { openrouter: { settings } } } }, async (ctx) => {
    await ctx.http("GET", "/api/model")
  })
