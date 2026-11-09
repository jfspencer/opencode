// Random files read through the reference's ReadToolFileSystem.read (in its memory environment), recorded with their
// pages or errors. Run from the reference's packages/core so its imports resolve.
const core = process.argv[4] ?? process.cwd()
const { Effect } = await import(Bun.resolveSync("effect", core))
const { ReadToolFileSystem } = await import(`${core}/src/tool/read-filesystem.ts`)
const { Environment } = await import(`${core}/src/environment/index.ts`)

let seed = Number(process.argv[2] ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T,>(items: T[]) => items[Math.floor(random() * items.length)]

const file = () => {
  // Up to about 1 MiB: past the 50 KiB budget and across 256 KiB chunks, with a few lines longer than a chunk.
  const big = random() < 0.15
  const lines = big ? 2000 + Math.floor(random() * 40000) : Math.floor(random() * 60)
  let huge = big && random() < 0.3 ? 2 : 0
  const parts: string[] = []
  for (let i = 0; i < lines; i++) {
    const kind = random()
    const length =
      huge > 0 && kind < 0.001 ? (huge--, 260000 + Math.floor(random() * 40000)) : kind < 0.05 ? 1990 + Math.floor(random() * 30) : Math.floor(random() * 40)
    parts.push(pick(["x", "é", "ab", "\t", "€"]).repeat(Math.max(0, Math.floor(length / 2))))
    parts.push(pick(["\n", "\n", "\n", "\r\n"]))
  }
  let text = parts.join("")
  if (random() < 0.3) text = text.replace(/\r?\n$/, "")
  const bytes = Buffer.from(text)
  if (random() < 0.1 && bytes.length > 0) bytes[Math.floor(random() * bytes.length)] = 0
  if (random() < 0.05) return Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), bytes])
  return bytes
}

const cases = []
for (let i = 0; i < Number(process.argv[3] ?? 300); i++) {
  const bytes = file()
  const lines = bytes.toString().split("\n").length
  const page: { offset?: number; limit?: number } = {}
  const mode = random()
  if (mode < 0.6) page.offset = pick([0, 1, 2, Math.floor(random() * (lines + 3)), lines, lines + 1])
  if (mode > 0.3 && mode < 0.9) page.limit = pick([0, 1, 3, 2000, 5000, Math.floor(random() * 100)])
  const files = Environment.makeFiles(Environment.makeMemoryDriver())
  const result = await Effect.runPromise(
    files.write("/f.txt", bytes).pipe(
      Effect.andThen(ReadToolFileSystem.read(files, "/f.txt", "f.txt", page)),
      Effect.map((output: any) => ({ output: { ...output } })),
      Effect.catch((error: any) => Effect.succeed({ error: error.message })),
    ),
  )
  cases.push({ bytes: bytes.toString("base64"), page, ...result })
}
console.log(JSON.stringify(cases))
