// The hand-written part of the parser corpus: snippets.txt, plus programs that need characters a text file would
// blur (line separators, odd whitespace, lone surrogates, astral identifiers) and a few combinatorial families.
import path from "node:path"

function readSnippets(text: string) {
  const out: string[] = []
  let block: string[] | undefined
  for (const line of text.split("\n")) {
    if (block) {
      if (line === ">>>") {
        out.push(block.join("\n"))
        block = undefined
      } else block.push(line)
      continue
    }
    if (line === "<<<") block = []
    else if (line.length > 0 && !line.startsWith("### ")) out.push(line)
  }
  return out
}

const fromFile = [
  ...readSnippets(await Bun.file(path.join(import.meta.dir, "snippets.txt")).text()),
  // `%uXXXX` stands for a four-digit unicode escape there
  ...readSnippets(await Bun.file(path.join(import.meta.dir, "snippets-escapes.txt")).text()).map((code) =>
    code.replace(/%u(?=[0-9a-fA-F]{4})/g, "\\u"),
  ),
]

const special: string[] = []
const spaces = ["\t", "\v", "\f", " ", " ", " ", " ", " ", " ", " ", " ", "　", "﻿", "᠎", "​"]
const breaks = ["\n", "\r", "\r\n", " ", " ", "\n\r"]
for (const space of spaces) special.push(`a${space}=${space}1`, `${space}"use strict";${space}010`, `x${space}/re/`)
for (const lb of breaks) {
  special.push(
    `a${lb}b`,
    `a${lb}++b`,
    `return${lb}a`,
    `throw${lb}a`,
    `'a${lb}b'`,
    `'a\\${lb}b'`,
    `\`a${lb}b\` + c${lb}d`,
    `tag\`a${lb}b\\x${lb}\` + c${lb}d`,
    `/*${lb}*/ x${lb}y`,
    `// x${lb}y`,
    `"use strict"${lb}010`,
    `"use strict"${lb}+1`,
    `let${lb}x`,
    `async${lb}function f() {}`,
    `a${lb}-->b${lb}c`,
    `x = 1${lb}<!--y${lb}z`,
    `/a${lb}/`,
    `a ?${lb}.5 : 1`,
    `\`\${a${lb}}\`${lb}x`,
    `"\\u2028${lb}x"`,
    `x${lb}{`,
    `{${lb}`,
    `function f() {${lb}"use strict"${lb}010${lb}}`,
  )
}
special.push(
  "'\ud800'",
  "'\udc00'",
  "\"😀\"",
  "`\ud800`",
  "x = '\ud800' + 1",
  "var 𝒜 = 1",
  "var a𝒜 = 1",
  "var \ud835 = 1",
  "\ud800",
  "x\ud800",
  "a + \udc00",
  "/\ud800/u",
  "/[\ud800-\udbff]/u",
  "/😀/u",
  "/[😀-😂]/v",
  "/(?<𝒜>.)/",
  "/(?<𝒜>.)/u",
  "#𝒜",
  "class A { #𝒜; m() { this.#𝒜 } }",
  " ",
  " x",
  "﻿x",
  "x‌y",
  "var x‌y = 1",
  "var x‍y = 1",
  "var ‌ = 1",
  "'\\ '",
  "`\\ `",
  "x = 1; y = 2",
  "a b",
  "#",
  "a #",
  "#!",
  "#! x",
  "#!/x\r\ny",
  "\u0000",
  "x\u0000",
  "'\u0000'",
  "`\u0000`",
  "'\u0001\u001f'",
  "\u007f",
  "­",
  "\u0080",
  "·",
  "x·",
  "̀",
  "x̀",
  "ⸯ",
  "゛",
  "\u{1F600}",
  "x = '\u{1F600}'",
  "\u{10FFFF}",
  "\u{E0100}",
  "x\u{E0100}",
)

// Every keyword and contextual word in the positions that treat them differently.
const words = [
  "break", "case", "catch", "continue", "debugger", "default", "do", "else", "finally", "for", "function", "if",
  "return", "switch", "throw", "try", "var", "const", "while", "with", "new", "this", "super", "class", "extends",
  "export", "import", "null", "true", "false", "in", "instanceof", "typeof", "void", "delete", "let", "static",
  "yield", "await", "async", "of", "get", "set", "using", "enum", "implements", "interface", "package", "private",
  "protected", "public", "eval", "arguments", "target", "meta", "from", "as", "constructor", "prototype",
]
const families = [
  (w: string) => `var ${w}`,
  (w: string) => `let ${w} = 1`,
  (w: string) => `${w} = 1`,
  (w: string) => `${w}: 1`,
  (w: string) => `x.${w}`,
  (w: string) => `({ ${w}: 1 })`,
  (w: string) => `({ ${w} })`,
  (w: string) => `({ ${w}() {} })`,
  (w: string) => `class A { ${w}() {} }`,
  (w: string) => `class A { static ${w} = 1 }`,
  (w: string) => `function ${w}() {}`,
  (w: string) => `function f(${w}) {}`,
  (w: string) => `(${w}) => 1`,
  (w: string) => `"use strict"; var ${w}`,
  (w: string) => `"use strict"; ${w} = 1`,
  (w: string) => `function* g() { var ${w} }`,
  (w: string) => `async function f() { var ${w} }`,
  (w: string) => `import { ${w} } from "m"`,
  (w: string) => `export { ${w} }`,
  (w: string) => `class A { #${w}; m() { this.#${w} } }`,
  (w: string) => `${w}`,
  (w: string) => `${w}()`,
  (w: string) => `${w}\n(x)`,
  (w: string) => `x = ${w} => 1`,
  (w: string) => `for (${w} of x) {}`,
  (w: string) => `for (${w} in x) {}`,
  (w: string) => `try {} catch (${w}) {}`,
  (w: string) => `class ${w} {}`,
  (w: string) => `label: ${w}`,
  (w: string) => `\\u{${w.charCodeAt(0).toString(16)}}${w.slice(1)} = 1`,
]
const combos = words.flatMap((word) => families.map((family) => family(word)))

export const snippets = [...fromFile, ...special, ...combos]

/** Contexts each snippet is also parsed in. */
export const contexts: Array<(code: string) => string> = [
  (code) => `function f() {\n${code}\n}`,
  (code) => `function* g() {\n${code}\n}`,
  (code) => `async function f() {\n${code}\n}`,
  (code) => `async function* g() {\n${code}\n}`,
  (code) => `"use strict";\n${code}`,
  (code) => `class C extends B {\n  m() {\n${code}\n  }\n}`,
  (code) => `class C {\n  static {\n${code}\n  }\n}`,
  (code) => `x = () => {\n${code}\n}`,
  (code) => `x = async () => {\n${code}\n}`,
  (code) => `{\n${code}\n}`,
  (code) => `label: for (;;) {\n${code}\n}`,
  (code) => `class C { #x; m() {\n${code}\n} }`,
]
