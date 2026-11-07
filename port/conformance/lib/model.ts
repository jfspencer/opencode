// The reference configuration: the product the Rust implementation must reproduce. Current (v2) config format
// only; legacy v1 keys (`provider`, `enabled_providers`, `agent`...) are out of scope. It mirrors the user's
// real setup: OpenRouter only, one hand-defined model, every agent on the `max` reasoning variant.
export const MODEL_ID = "deepseek/deepseek-v4.1-flash-1017"
export const MODEL_REF = `openrouter/${MODEL_ID}`
export const OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"

const AGENTS = ["build", "plan", "general", "explore", "compaction", "title", "summary"]

/** Tools whose features are out of scope; denying them on every input hides them from the model. */
export const HIDDEN_TOOLS = [
  "experimental_image_generate",
  "websearch",
  "opencode_list_mcp_resources",
  "opencode_read_mcp_resource",
]

export const baseConfig = {
  model: MODEL_REF,
  agents: Object.fromEntries(AGENTS.map((agent) => [agent, { model: `${MODEL_REF}#max` }])),
  providers: {
    openrouter: {
      name: "OpenRouter",
      env: ["OPENROUTER_API_KEY"],
      package: "@opencode/ai/providers/openrouter",
      settings: { baseURL: OPENROUTER_BASE_URL },
      models: {
        [MODEL_ID]: {
          name: "DeepSeek V4.1 Flash 1017",
          family: "deepseek-flash",
          package: "@opencode/ai/providers/openrouter",
          capabilities: {
            tools: true,
            input: ["text", "image/jpeg", "image/png", "image/gif", "image/webp"],
            output: ["text"],
            temperature: true,
            reasoning: { supported: true, effort: ["none", "low", "high", "max"] },
          },
          limit: { context: 1048576, output: 393216 },
        },
      },
    },
  },
  policies: [
    { action: "provider.use", resource: "*", effect: "deny" },
    { action: "provider.use", resource: "openrouter", effect: "allow" },
  ],
  permissions: HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
  websearch: false,
  share: "disabled",
  update: "disable",
}

/** Deep-merge scenario overrides into the base config (objects merge; arrays and scalars replace). */
export function config(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const merge = (a: unknown, b: unknown): unknown => {
    if (!isObject(a) || !isObject(b)) return b
    const out: Record<string, unknown> = { ...a }
    for (const [key, value] of Object.entries(b)) out[key] = key in a ? merge(a[key], value) : value
    return out
  }
  // A deep copy: callers may mutate the result (e.g. point the base URL at a local fake), and the base config
  // is shared by every scenario in the process.
  return structuredClone(merge(baseConfig, overrides)) as Record<string, unknown>
}

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)
