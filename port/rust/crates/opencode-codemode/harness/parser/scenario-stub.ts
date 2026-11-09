// Stands in for the conformance harness's `../lib/openrouter`, `../lib/scenario` and `../lib/model` when
// gen-corpus.ts imports the codemode scenario files: it only records the `code` of every tool call they script.
export const collected: string[] = []

function collect(value: unknown, seen: Set<unknown>) {
  if (value === null || typeof value !== "object" || seen.has(value)) return
  seen.add(value)
  if (Array.isArray(value)) return value.forEach((item) => collect(item, seen))
  for (const [key, item] of Object.entries(value)) {
    if (key === "code" && typeof item === "string") collected.push(item)
    else collect(item, seen)
  }
}

export const say = (text: string) => ({ events: [{ text }] })
export const call = (tool: string, input: unknown, text?: string) => ({ events: [...(text ? [{ text }] : []), { tool, input }] })
export function scenario(_name: string, options: unknown) {
  collect(options, new Set())
}
export const MODEL_ID = "stub"
export const MODEL_REF = "stub"
export const baseConfig = { permissions: [] }
export function config() {
  return {}
}
