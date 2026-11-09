// JSC's side of `jsregex_check --bench`: the same global match counts over the same 1 MB text, timed.
// usage: bun harness/jsregex/bench.ts
const line = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt.\n"
const text = line.repeat((1 << 20) >> 7)
const bench: Array<[string, string]> = [
  ["\\w+", "g"],
  ["(\\w+)\\s(\\w+)", "g"],
  ["[aeiou]{2}", "g"],
  ["^.*$", "gm"],
  ["(?:a|b|c|d|e)+", "g"],
  ["\\bipsum\\b", "g"],
  ["(?<=m)\\s", "g"],
  ["xyz", "g"],
  ["\\p{L}+", "gu"],
  ["[a-z]+", "gi"],
  ["(\\w)\\1", "g"],
  ["(?:(\\w+),?\\s*)+\\.", "g"],
  ["e[^e]*?t", "g"],
  ["(?:dolor|tempor|elit)", "g"],
]
for (const [pattern, flags] of bench) {
  const re = new RegExp(pattern, flags)
  const started = performance.now()
  const count = text.match(re)?.length ?? 0
  console.log(`${(performance.now() - started).toFixed(1).padStart(8)} ms  ${String(count).padStart(8)}  /${pattern}/${flags}`)
}
