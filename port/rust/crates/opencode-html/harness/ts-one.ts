// One TS conversion of a file, to stdout: bun harness/ts-one.ts m|t <file>. With a third argument `events`, prints
// the htmlparser2 events html-markdown.ts would see instead.
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const reference = join(dirname(fileURLToPath(import.meta.url)), "../../../../.reference")
const tool = join(reference, "packages/core/src/tool")
const [kind, file, mode] = process.argv.slice(2)
const html = new TextDecoder("utf-8", { ignoreBOM: true }).decode(readFileSync(file))
if (mode === "events") {
  const { Parser } = await import(join(reference, "packages/core/node_modules/htmlparser2/lib/esm/Parser.js"))
  const parser = new Parser({
    onopentag: (name: string, attributes: Record<string, string>) => console.log("open", JSON.stringify(name), JSON.stringify(attributes)),
    ontext: (text: string) => console.log("text", JSON.stringify(text)),
    onclosetag: (name: string) => console.log("close", JSON.stringify(name)),
  })
  if (kind === "m") for (let i = 0; i < html.length; i += 64 * 1024) parser.write(html.slice(i, i + 64 * 1024))
  else parser.write(html)
  parser.end()
} else {
  const { convertHTMLToMarkdown } = await import(join(tool, "html-markdown.ts"))
  const { extractTextFromHTML } = await import(join(tool, "plugin/webfetch.ts"))
  process.stdout.write(kind === "m" ? convertHTMLToMarkdown(html) : extractTextFromHTML(html))
}
