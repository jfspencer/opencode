// Generates JSONC documents (generated ones in every layout, their mutations, and hand-picked edge cases) and root
// property edits, and records what the reference's jsonc-parser makes of each: `applyEdits(text, modify(text, [key],
// value, { formattingOptions: { tabSize: 2, insertSpaces: true } }))`, or `null` where it throws. That is the edit
// `Config.update` makes (packages/core/src/config.ts), with other keys and values too.
//
// usage: bun jsonccases.ts <reference packages dir> <seed> <count>
import path from "node:path"

const [packages, seedArg, countArg] = process.argv.slice(2)
const library = Bun.resolveSync("jsonc-parser", path.join(packages, "core"))
const { applyEdits, modify } = await import(library)

let seed = Number(seedArg ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const int = (n: number) => Math.floor(random() * n)
const pick = <T>(items: readonly T[]) => items[int(items.length)]
const chance = (p: number) => random() < p

const words = ["shell", "a", "b", "model", "", "with space", "ü", "日本", "😀", 'q"uote', "back\\slash", "__proto__", "1", "tab\t"]
const texts = [
  "/bin/sh",
  "/bin/bash",
  "",
  "zsh",
  "C:\\Program Files\\Git\\bin\\bash.exe",
  'say "hi"',
  "line\nbreak",
  "ctl\u0001\u001f\u007f",
  "ü日本😀",
  "\u2028\u2029",
  "/usr/local/bin/fish -l",
  "  spaced  ",
]

// One document at a time draws its layout: line breaks, indentation, comments, trailing commas.
interface Layout {
  eol: () => string
  indent: (depth: number) => string
  space: () => string
  comments: number
  trailing: number
  inline: number
}

const layout = (): Layout => {
  const eols = pick([["\n"], ["\r\n"], ["\r"], ["\n", "\r\n"], ["\n", "\r", "\r\n"]])
  const unit = pick(["  ", "    ", "\t", " ", "", "\t ", "   "])
  const odd = chance(0.2)
  return {
    eol: () => pick(eols),
    indent: (depth) => (odd ? pick(["", " ", "\t", "  ", "      "]) : unit.repeat(depth)),
    space: () => pick(["", " ", " ", "  ", "\t"]),
    comments: pick([0, 0, 0.1, 0.3]),
    trailing: pick([0, 0, 0.2, 0.6]),
    inline: pick([0, 0.2, 0.5, 1]),
  }
}

const comment = (l: Layout) =>
  pick([
    `// ${pick(words)}${l.eol()}`,
    `/* ${pick(words)} */`,
    `/*${l.eol()} multi ${l.eol()}*/`,
    `/**/`,
    `//${l.eol()}`,
  ])

const gap = (l: Layout, depth: number, broken: boolean) => {
  let out = broken ? l.eol() + l.indent(depth) : l.space()
  if (chance(l.comments)) out += comment(l) + (chance(0.5) ? l.eol() + l.indent(depth) : l.space())
  return out
}

const key = () => {
  const word = chance(0.4) ? "shell" : pick(words)
  if (chance(0.05)) return '"sh\\u0065ll"'
  if (chance(0.03)) return '"shell\\x"'
  return JSON.stringify(word)
}

const scalar = (): string =>
  pick([
    () => JSON.stringify(pick(texts)),
    () => String(int(1000) - 500),
    () => pick(["0", "-0", "1.5", "1e3", "-2.5E-3", "12345678901234567890"]),
    () => pick(["true", "false", "null"]),
  ])()

const value = (l: Layout, depth: number): string => {
  const kind = random()
  if (depth > 3 || kind < 0.55) return scalar()
  const object = kind < 0.8
  const count = int(4)
  const broken = !chance(l.inline)
  const items = Array.from({ length: count }, () =>
    object ? `${key()}${l.space()}:${l.space()}${value(l, depth + 1)}` : value(l, depth + 1),
  )
  let body = items.map((item, i) => gap(l, depth + 1, broken) + item + (i < count - 1 ? l.space() + "," : "")).join("")
  if (count > 0 && chance(l.trailing)) body += ","
  if (count > 0 || chance(0.5)) body += gap(l, depth, broken)
  return object ? `{${body}}` : `[${body}]`
}

const document = () => {
  const l = layout()
  const count = int(6)
  const broken = !chance(l.inline / 2)
  const properties = Array.from({ length: count }, () => {
    const name = chance(0.3) ? '"shell"' : key()
    return `${name}${l.space()}:${l.space()}${value(l, 1)}`
  })
  let body = properties.map((p, i) => gap(l, 1, broken) + p + (i < count - 1 ? l.space() + "," : "")).join("")
  if (count > 0 && chance(l.trailing)) body += ","
  if (count > 0 || chance(0.5)) body += gap(l, 0, broken)
  let out = `${l.indent(int(2))}{${body}}`
  if (chance(0.3)) out = comment(l) + l.eol() + out
  if (chance(0.2)) out = l.indent(1) + out
  if (chance(0.7)) out += l.eol()
  if (chance(0.15)) out += comment(l)
  if (chance(0.1)) out += l.eol() + l.eol()
  return out
}

// Mutations by code point, so the text stays well formed.
const mutate = (text: string) => {
  const chars = Array.from(text)
  for (let n = 1 + int(3); n > 0; n--) {
    const at = int(chars.length + 1)
    const op = random()
    if (op < 0.35 && chars.length) chars.splice(Math.min(at, chars.length - 1), 1)
    else if (op < 0.7) chars.splice(at, 0, pick(['"', "{", "}", "[", "]", ",", ":", "/", "*", "\\", "\n", "\r", " ", "\t", "x", "-", "1", "\ufeff", "\u00a0", "\f"]))
    else if (op < 0.85) chars.splice(at)
    else chars.splice(at, 0, ...Array.from(pick(["//", "/*", "*/", "true", "nul", "1.", "1e", "-", '"shell":', ",,", "\r\n"])))
  }
  return chars.join("")
}

const handPicked = [
  "",
  " ",
  "\n",
  "{}",
  "{}\n",
  "{\n}\n",
  "[]",
  "[1]",
  '"str"',
  "1",
  "true",
  "null",
  "garbage",
  "garbage {}",
  ": {}",
  "{",
  "}",
  '{"shell"',
  '{"shell":',
  '{"shell":}',
  '{"shell" 1}',
  '{"shell": 1 "a": 2}',
  "{,}",
  "{,,}",
  "/*",
  "/* x",
  "{/*}",
  "// only a comment",
  "/* a */ /* b */",
  '{"a":1,}',
  '{"a":1,,}',
  '{"shell":1,"shell":2}',
  '{"a":{"shell":1}}',
  '{"shell":{"a":[1,{"b":2}]}}',
  '{ "a": 1 } {"b": 2}',
  '{"a": [1, 2,], }',
  "\ufeff{}",
  '{"a":1}\r\n',
  '{\r\n\t"a": 1\r\n}\r\n',
  '{\r"a": 1\r}',
  '{"shell": "x" // c\n}',
  '{"shell": "x" /* c */}',
  '{"a": 1, // c\n "shell": 2}',
  '{\n  "a": 1 // c\n}',
  '{\n  // c\n}',
  '{\n  /* c */\n}',
  '{"a": "\\u12"}',
  '{"a": "unterminated}',
  '{"a": "line\nbreak"}',
  '{"a": -}',
  '{"a": 01}',
  '  {\n    "deep": 1\n  }\n',
  '\t{\n\t\t"deep": 1\n\t}\n',
]

const valueFor = (): unknown => {
  const kind = random()
  if (kind < 0.3) return undefined
  if (kind < 0.85) return pick(texts)
  return pick<unknown>([
    0,
    -1.5,
    1e21,
    true,
    null,
    [],
    {},
    ["a", 1],
    { a: { b: [1, "x"] } },
    { "a b": null },
  ])
}

const record = (text: string, key: string, value: unknown) => {
  let result: string | null
  try {
    result = applyEdits(text, modify(text, [key], value, { formattingOptions: { tabSize: 2, insertSpaces: true } }))
  } catch {
    result = null
  }
  return value === undefined ? { text, key, result } : { text, key, value, result }
}

const cases: unknown[] = []
for (const text of handPicked)
  for (const value of [undefined, "/bin/sh", { a: [1] }]) for (const key of ["shell", "a"]) cases.push(record(text, key, value))
const count = Number(countArg ?? 20000)
while (cases.length < count) {
  const base = document()
  const text = chance(0.3) ? mutate(base) : base
  const name = chance(0.8) ? "shell" : pick(words)
  cases.push(record(text, name, valueFor()))
}
console.log(JSON.stringify(cases))
