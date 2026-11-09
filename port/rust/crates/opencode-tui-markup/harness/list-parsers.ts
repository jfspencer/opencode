// Prints the parsers of a parsers-config.ts as JSON lines (input to gen-grammars.py).
const mod = await import(process.argv[2])
for (const p of mod.default.parsers) console.log(JSON.stringify(p))
