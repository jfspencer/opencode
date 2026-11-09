// Cases of the contexts milestone: the TUI's client, data, location and event contexts end to end, generated
// against the TS providers (harness/cases/context-generator.ts).
export function contextCases(seed: number, count: number) {
  return Array.from({ length: count }, (_, index) => ({
    kind: "context",
    name: `context #${index}`,
    generate: { seed: seed * 7_919 + index, steps: 30 + ((index * 11) % 70) },
  }))
}
