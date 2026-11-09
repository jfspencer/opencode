// Inputs of the opencode-tui-markup harness: the CommonMark and GFM spec examples and marked's own test documents
// (harness/corpus/marked, from marked v17.0.1), every assistant/markdown text in port/conformance/goldens, and
// random markdown and code built from fragments that exercise the lexer, the block model and highlighting.
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join } from "node:path"

const here = import.meta.dir
const port = join(here, "../../../..")

/** A small deterministic PRNG (mulberry32). */
export class Rng {
  private state: number
  constructor(seed: number) {
    this.state = seed >>> 0
  }
  next(): number {
    let t = (this.state += 0x6d2b79f5)
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(n: number): number {
    return Math.floor(this.next() * n)
  }
  pick<T>(items: readonly T[]): T {
    return items[this.int(items.length)]!
  }
  chance(p: number): boolean {
    return this.next() < p
  }
}

export function specExamples(): { name: string; markdown: string }[] {
  const out: { name: string; markdown: string }[] = []
  for (const file of ["commonmark.0.31.2.json", "gfm.0.29.json"]) {
    const examples = JSON.parse(readFileSync(join(here, "corpus/marked", file), "utf8"))
    for (const ex of examples) out.push({ name: `${file}#${ex.example} ${ex.section}`, markdown: ex.markdown })
  }
  return out
}

export function markedDocuments(): { name: string; markdown: string }[] {
  const out: { name: string; markdown: string }[] = []
  for (const dir of ["new", "original"]) {
    for (const file of readdirSync(join(here, "corpus/marked", dir)).sort()) {
      if (!file.endsWith(".md")) continue
      let text = readFileSync(join(here, "corpus/marked", dir, file), "utf8")
      // Front matter holds marked test options, not markdown.
      text = text.replace(/^---\n[\s\S]*?\n---\n/, "")
      out.push({ name: `${dir}/${file}`, markdown: text })
    }
  }
  return out
}

/** Every string under a `text`, `content` or `delta` key in the conformance goldens (assistant text, reasoning,
 * tool output), deduplicated. */
export function goldenTexts(): { name: string; markdown: string }[] {
  const root = join(port, "conformance/goldens")
  const seen = new Set<string>()
  const out: { name: string; markdown: string }[] = []
  const visit = (value: unknown, file: string) => {
    if (Array.isArray(value)) {
      for (const v of value) visit(v, file)
    } else if (value && typeof value === "object") {
      for (const [k, v] of Object.entries(value)) {
        if ((k === "text" || k === "content" || k === "delta") && typeof v === "string" && v.trim() && !seen.has(v)) {
          seen.add(v)
          out.push({ name: `${file}:${k}`, markdown: v })
        } else visit(v, file)
      }
    }
  }
  for (const dir of readdirSync(root).sort()) {
    if (dir === "_blobs") continue
    const path = join(root, dir)
    if (!statSync(path).isDirectory()) continue
    for (const file of readdirSync(path).sort()) {
      if (!file.endsWith(".json")) continue
      visit(JSON.parse(readFileSync(join(path, file), "utf8")), `${dir}/${file}`)
    }
  }
  return out
}

const WORDS = ["alpha", "beta", "gamma", "delta", "x", "foo_bar", "snake_case", "CamelCase", "über", "naïve", "日本語", "中文", "😀", "a😀b", "é", "tab\there", "12", "3.14", "-", "--", "—", "'quoted'", '"double"', "it's", "&amp;", "&copy;", "&#35;", "<b>", "</b>", "<br>", "a<b", "a>b", "2*3", "a_b_c", "**", "__", "~", "~~", "`", "``", "\\*", "\\_", "\\`", "\\\\", "\\|", "|", "[", "]", "(", ")", "!", "#", "@", "user@example.com", "https://example.com/a_b*c", "http://x.y/(a)b", "www.example.org.", "ftp://f.tp/x", "<https://auto.link>", "<a@b.co>", ":", ";", "."]

function inline(rng: Rng, depth = 0): string {
  const parts: string[] = []
  const n = 1 + rng.int(6)
  for (let i = 0; i < n; i++) {
    const r = rng.int(depth > 2 ? 4 : 22)
    const word = rng.pick(WORDS)
    switch (r) {
      case 0:
      case 1:
      case 2:
      case 3:
        parts.push(word)
        break
      case 4:
        parts.push(`*${inline(rng, depth + 1)}*`)
        break
      case 5:
        parts.push(`**${inline(rng, depth + 1)}**`)
        break
      case 6:
        parts.push(`_${inline(rng, depth + 1)}_`)
        break
      case 7:
        parts.push(`__${inline(rng, depth + 1)}__`)
        break
      case 8:
        parts.push(`***${word}***`)
        break
      case 9:
        parts.push("`" + word + "`")
        break
      case 10:
        parts.push("`` " + word + "` ``")
        break
      case 11:
        parts.push(`~~${inline(rng, depth + 1)}~~`)
        break
      case 12:
        parts.push(`[${inline(rng, depth + 1)}](${rng.pick(["https://example.com", "/rel/path", "<with space>", "a(b)c", "x \"title\"", ""])})`)
        break
      case 13:
        parts.push(`![${word}](img.png${rng.chance(0.5) ? ' "t"' : ""})`)
        break
      case 14:
        parts.push(`[${word}][${rng.pick(["ref", "Ref", "missing", ""])}]`)
        break
      case 15:
        parts.push(rng.pick(["https://example.com/path?q=1", "http://a.b", "www.site.com/x)", "mail@site.org", "<https://x.y>"]))
        break
      case 16:
        parts.push(rng.pick(["  \n", "\\\n", "\n"]))
        break
      case 17:
        parts.push(rng.pick(["*", "_", "**", "__", "*a", "a*", "_a", "a_", "***", "~", "`"]))
        break
      case 18:
        parts.push(`<span class="x">${word}</span>`)
        break
      case 19:
        parts.push(`[${word}](${word} '${word}')`)
        break
      case 20:
        parts.push(`[${word}](https://example.com/${word})`)
        break
      default:
        parts.push(`*${word}*${rng.pick(["a", ".", " ", "*"])}`)
    }
    parts.push(rng.pick([" ", " ", " ", "", "  ", "\t"]))
  }
  return parts.join("")
}

const LANGS = ["ts", "typescript", "js", "javascript", "tsx", "jsx", "md", "markdown", "python", "py", "rust", "go", "json", "bash", "sh", "", "unknown", "c", "zig", "yaml", "diff", "html", "css"]

function block(rng: Rng, depth = 0): string {
  const indent = depth > 0 ? "" : rng.pick(["", "", "", " ", "  ", "   "])
  switch (rng.int(depth > 1 ? 4 : 16)) {
    case 0:
    case 1:
      return indent + inline(rng)
    case 2:
      return `${"#".repeat(1 + rng.int(7))}${rng.pick([" ", "", "  "])}${inline(rng)}${rng.pick(["", " #", " ##", "#"])}`
    case 3: {
      const ordered = rng.chance(0.4)
      const start = rng.pick([1, 1, 2, 9, 10, 0])
      const n = 1 + rng.int(4)
      const loose = rng.chance(0.3)
      const items: string[] = []
      for (let i = 0; i < n; i++) {
        const marker = ordered ? `${start + i}${rng.pick([".", ")"])}` : rng.pick(["-", "*", "+"])
        let item = `${marker} ${rng.chance(0.2) ? rng.pick(["[ ] ", "[x] ", "[X] "]) : ""}${inline(rng)}`
        if (rng.chance(0.25)) item += `\n${" ".repeat(marker.length + 1)}${block(rng, depth + 1).split("\n").join(`\n${" ".repeat(marker.length + 1)}`)}`
        items.push(item)
      }
      return items.join(loose ? "\n\n" : "\n")
    }
    case 4:
      return block(rng, depth + 1)
        .split("\n")
        .map((l) => `${rng.pick([">", "> ", " > "])}${l}`)
        .join("\n")
    case 5: {
      const fence = rng.pick(["```", "````", "~~~"])
      const body = rng.pick(["const x = 1", "let a = `t`\nfunction f() { return a }", "# not a heading", "def f():\n    return 1", "{\n  \"k\": [1, 2]\n}", "fn main() {}", "<div>x</div>", "- a\n- b", "**bold** in code", "echo $HOME"])
      return `${indent}${fence}${rng.pick(LANGS)}${rng.pick(["", " extra"])}\n${body}\n${rng.chance(0.85) ? fence : ""}`
    }
    case 6:
      return `    ${inline(rng)}\n    ${inline(rng)}`
    case 7: {
      const cols = 1 + rng.int(4)
      const row = (cells: string[]) => (rng.chance(0.7) ? `| ${cells.join(" | ")} |` : cells.join(" | "))
      const head = row(Array.from({ length: cols }, () => rng.pick(WORDS)))
      const align = row(Array.from({ length: cols }, () => rng.pick(["---", ":--", "--:", ":-:", "-"])))
      const rows = Array.from({ length: rng.int(4) }, () =>
        row(Array.from({ length: cols + rng.pick([0, 0, 0, -1, 1]) }, () => inline(rng, 2).replace(/\n/g, " "))),
      )
      return [head, align, ...rows].join("\n")
    }
    case 8:
      return rng.pick(["---", "***", "___", "- - -", "* * *", "-----"])
    case 9:
      return rng.pick(["<div>\nhtml block\n</div>", "<!-- comment -->", "<pre>\npre\n</pre>", "<details><summary>s</summary>", "<?php ?>", "<![CDATA[x]]>"])
    case 10:
      return `[${rng.pick(["ref", "Ref", "other"])}]: ${rng.pick(["https://example.com", "<https://b.c>", "/x"])}${rng.pick(["", ' "Title"', " 'T'", " (P)"])}`
    case 11:
      return `${inline(rng)}\n${rng.pick(["===", "---", "=", "-"])}`
    default:
      return indent + inline(rng) + (rng.chance(0.5) ? `\n${inline(rng)}` : "")
  }
}

export function randomMarkdown(rng: Rng): string {
  const blocks: string[] = []
  const n = 1 + rng.int(7)
  for (let i = 0; i < n; i++) {
    blocks.push(block(rng))
    if (i < n - 1) blocks.push(rng.pick(["\n\n", "\n\n", "\n", "\n\n\n", "\n  \n"]))
  }
  let text = blocks.join("")
  if (rng.chance(0.1)) text = text.replace(/\n/g, "\r\n")
  if (rng.chance(0.2)) text += rng.pick(["\n", "\n\n", " ", "  "])
  return text
}

/** Code samples per filetype: the grammars' own test corpora (corpus/code) and hand-written ones (code-extra.json,
 * where `*` applies to every filetype). */
export function codeSamples(): { filetype: string; name: string; code: string }[] {
  const dir = join(here, "corpus/code")
  const extra = JSON.parse(readFileSync(join(here, "corpus/code-extra.json"), "utf8"))
  const out: { filetype: string; name: string; code: string }[] = []
  for (const file of readdirSync(dir).sort()) {
    const filetype = file.replace(/\.json$/, "")
    const samples: string[] = [...JSON.parse(readFileSync(join(dir, file), "utf8")), ...(extra[filetype] ?? []), ...extra["*"]]
    samples.forEach((code, i) => out.push({ filetype, name: `${filetype}#${i}`, code }))
  }
  // Aliases and a filetype with no parser.
  for (const [alias, of] of [["javascriptreact", "javascript"], ["udiff", "diff"], ["patch", "diff"], ["makefile", "make"], ["nolang", "python"]]) {
    const samples: string[] = JSON.parse(readFileSync(join(dir, `${of}.json`), "utf8")).slice(0, 6)
    samples.forEach((code, i) => out.push({ filetype: alias!, name: `${alias}#${i}`, code }))
  }
  return out
}

/** Every unified diff in the conformance goldens (strings with a hunk header under any key). */
export function goldenDiffs(): { name: string; diff: string }[] {
  const root = join(port, "conformance/goldens")
  const seen = new Set<string>()
  const out: { name: string; diff: string }[] = []
  const visit = (value: unknown, file: string) => {
    if (typeof value === "string") {
      if (/^@@ -\d/m.test(value) && !seen.has(value)) {
        seen.add(value)
        out.push({ name: file, diff: value })
      }
    } else if (Array.isArray(value)) value.forEach((v) => visit(v, file))
    else if (value && typeof value === "object") Object.values(value).forEach((v) => visit(v, file))
  }
  for (const dir of readdirSync(root).sort()) {
    if (dir === "_blobs") continue
    const path = join(root, dir)
    if (!statSync(path).isDirectory()) continue
    for (const file of readdirSync(path).sort()) {
      if (file.endsWith(".json")) visit(JSON.parse(readFileSync(join(path, file), "utf8")), `${dir}/${file}`)
    }
  }
  return out
}

const LINES = ["const a = 1", "let b = 2;", "", "  indented", "\tTabbed", "function f() {", "}", "// comment", "日本語 ✓", "x".repeat(90), "return a + b", "import x from 'y'"]

/** A random file pair diffed with jsdiff, then sometimes damaged the ways real patches are (headers, counts,
 * markers, Git extended headers). */
export function randomDiff(rng: Rng, jsdiff: any): string {
  const old = Array.from({ length: rng.int(14) }, () => rng.pick(LINES))
  const neu = old.flatMap((l) => (rng.chance(0.2) ? [] : rng.chance(0.2) ? [rng.pick(LINES), l] : [rng.chance(0.15) ? rng.pick(LINES) : l]))
  if (rng.chance(0.3)) neu.push(rng.pick(LINES))
  const a = old.join("\n") + (rng.chance(0.8) ? "\n" : "")
  const b = neu.join("\n") + (rng.chance(0.8) ? "\n" : "")
  let patch: string = jsdiff.createTwoFilesPatch(rng.pick(["a/x.ts", "src/file.py", "dir with space/f"]), rng.pick(["b/x.ts", "src/file.py"]), a, b, rng.pick(["", "old\theader"]), rng.pick(["", "new"]), { context: rng.int(4) })
  const lines = patch.split("\n")
  switch (rng.int(10)) {
    case 0:
      lines.unshift("diff --git a/x.ts b/x.ts", rng.pick(["index 83db48f..bf269f4 100644", "new file mode 100644", "deleted file mode 100755", "similarity index 90%\nrename from x.ts\nrename to y.ts", "old mode 100644\nnew mode 100755", "Binary files a/x and b/x differ"]))
      break
    case 1:
      if (lines.length > 4) lines.splice(2 + rng.int(lines.length - 3), 1)
      break
    case 2:
      lines.push(rng.pick(["+extra", " context", "-gone", "\\ No newline at end of file", "garbage"]))
      break
    case 3:
      lines.splice(0, 2)
      break
    case 4:
      lines.unshift('diff --git "a/tab\\tname" "b/tab\\tname"', "index 1..2 100644")
      break
    case 5:
      return lines.filter((l) => !l.startsWith("===")).join("\n") + "\n" + patch
    default:
      break
  }
  return lines.join("\n")
}

/** Cut points for streaming a text: prefixes as a model would emit them. */
export function streamSteps(rng: Rng, text: string): string[] {
  const steps: string[] = []
  let at = 0
  while (at < text.length) {
    at = Math.min(text.length, at + 1 + rng.int(rng.chance(0.3) ? 3 : 24))
    // Do not split a surrogate pair: the TUI receives whole code points.
    if (at < text.length && /[\ud800-\udbff]/.test(text[at - 1]!)) at++
    steps.push(text.slice(0, at))
  }
  return steps
}
