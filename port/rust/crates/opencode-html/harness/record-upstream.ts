// A `bun test --preload` for the upstream tests: records every document an htmlparser2 Parser is given (the
// chunks joined) and which converter parses it, then writes them as JSON lines to $OPENCODE_HTML_RECORD on exit.
// html-markdown.ts's `onopentag(name, attributes)` takes two parameters, webfetch.ts's `onopentag(name)` one.
import { appendFileSync } from "node:fs"
import { join } from "node:path"

const target = process.env.OPENCODE_HTML_RECORD
if (!target) throw new Error("OPENCODE_HTML_RECORD is not set")
const reference = process.env.OPENCODE_HTML_REFERENCE!
// The module instance packages/core resolves (bun resolves the symlink to the same file).
const { Parser } = await import(join(reference, "packages/core/node_modules/htmlparser2/lib/esm/Parser.js"))

const written = new WeakMap<object, string[]>()
const write = Parser.prototype.write
Parser.prototype.write = function (chunk: string) {
  let chunks = written.get(this)
  if (!chunks) written.set(this, (chunks = []))
  chunks.push(chunk)
  return write.call(this, chunk)
}
const end = Parser.prototype.end
Parser.prototype.end = function (chunk?: string) {
  const html = (written.get(this) ?? []).join("") + (chunk ?? "")
  const kind = this.cbs?.onopentag?.length === 2 ? "markdown" : "text"
  appendFileSync(target, JSON.stringify({ kind, html }) + "\n")
  return end.call(this, chunk)
}
