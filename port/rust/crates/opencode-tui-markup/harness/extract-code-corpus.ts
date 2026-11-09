// Builds harness/corpus/code/<filetype>.json: code samples per bundled language, taken from each grammar
// repository's tree-sitter test corpus (test/corpus/**/*.txt, MIT like the grammars), from the archives
// port/scripts/vendor-grammars.sh fetches (port/.vendor/tree-sitter/.archives/<dir>.tar.gz) extracted whole to
// <download dir>/src/<dir>.
//
// usage: bun harness/extract-code-corpus.ts <download dir>
import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync, existsSync } from "node:fs"
import { join } from "node:path"

const download = process.argv[2]!
const crate = join(import.meta.dir, "..")
const out = join(crate, "harness/corpus/code")
mkdirSync(out, { recursive: true })
const PER_LANGUAGE = 48
const MAX_LENGTH = 1200

function walk(dir: string): string[] {
  if (!existsSync(dir)) return []
  const files: string[] = []
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) files.push(...walk(path))
    else if (name.endsWith(".txt") || name.endsWith(".mk")) files.push(path)
  }
  return files
}

/** The inputs of a tree-sitter test file: `===\nname\n[:attributes]\n===\ninput\n---\n(expected)`. */
function inputs(text: string): string[] {
  const out: string[] = []
  const lines = text.split("\n")
  const isRule = (l: string | undefined) => !!l && /^={3,}\S*$/.test(l)
  const bodies: string[][] = []
  let i = 0
  while (i < lines.length) {
    if (!isRule(lines[i])) {
      bodies.at(-1)?.push(lines[i]!)
      i++
      continue
    }
    // A header: the opening rule, name and attribute lines, the closing rule.
    let j = i + 1
    while (j < lines.length && !isRule(lines[j])) j++
    bodies.push([])
    i = j + 1
  }
  for (const body of bodies) {
    const sep = body.findIndex((l) => /^-{3,}\S*$/.test(l))
    const input = (sep >= 0 ? body.slice(0, sep) : body).join("\n").replace(/^\n/, "").replace(/\n+$/, "")
    if (input.trim() && input.length <= MAX_LENGTH) out.push(input)
  }
  return out
}

for (const line of readFileSync(join(crate, "grammars/SOURCES.txt"), "utf8").split("\n")) {
  if (!line.trim() || line.startsWith("#")) continue
  const [filetype, , , , , src] = line.split(" ")
  const root = join(download, "src", src!.replace(/\/src$/, ""))
  const repo = join(download, "src", src!.split("/")[0]!)
  let files = walk(join(root, "test/corpus"))
  if (files.length === 0) files = walk(join(repo, "test/corpus"))
  if (files.length === 0) files = walk(join(repo, "corpus"))
  const samples = files.flatMap((f) => inputs(readFileSync(f, "utf8")))
  // Spread the picks over the whole corpus.
  const step = Math.max(1, samples.length / PER_LANGUAGE)
  const picked: string[] = []
  for (let i = 0; i < samples.length && picked.length < PER_LANGUAGE; i += step) picked.push(samples[Math.floor(i)]!)
  writeFileSync(join(out, `${filetype}.json`), JSON.stringify(picked, null, 1) + "\n")
  console.log(filetype, files.length, "files", samples.length, "samples", picked.length, "picked")
}
