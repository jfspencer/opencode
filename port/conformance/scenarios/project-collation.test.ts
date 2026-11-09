// `localeCompare` order through the API: directory listings and the read tool sort names with Bun's ICU (root
// collation). The names are where ICU's CLDR root differs from the DUCET and from plain UCA over NFD: Han in
// radical-stroke order across the extension blocks, radicals and compatibility ideographs, characters added since
// Unicode 13, the `l·` prefix mapping, the U+FDD1 Hangul marker contraction, and marks ICU keeps in text order.
import { call, say } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

// Every name compares unequal to every other, so the order never depends on the directory's own order.
const names = [
  // Latin, and the middle dot that is only a secondary difference after `l`.
  "ll.txt",
  "l·l.txt",
  "L·L.txt",
  "l.l.txt",
  "a.txt",
  "B.txt",
  // Han: unified ideographs of the core block and of extensions A, B, C, G and H, a Kangxi radical, a CJK radical,
  // a compatibility ideograph.
  "一.md",
  "丁.md",
  "㐀.md",
  "乙.md",
  "人.md",
  "日.md",
  "鿿.md",
  "\u{20000}.md",
  "\u{2a700}.md",
  "\u{30000}.md",
  "\u{31350}.md",
  "⼀.md",
  "⺀.md",
  "豈.md",
  // Emoji: of Unicode 6, 15 and 16.
  "\u{1f600}.txt",
  "\u{1fae8}.txt",
  "\u{1f6dc}.txt",
  "\u{1fa75}.txt",
  "\u{1fae9}.txt",
  "\u{1fac6}.txt",
  // Hangul: the first-Hangul marker contraction matches the syllable, not its jamo.
  "가.txt",
  "﷑가.txt",
  "﷑가.txt",
  // Marks after a contraction starter that ICU leaves unnormalized, and the same marks after a plain letter.
  "ي\u{1e01b}ႍ.txt",
  "يႍ\u{1e01b}.txt",
  "x\u{1e01b}ႍ.txt",
  // Thai: a prevowel contracts with the consonant after it.
  "เก.txt",
  "กา.txt",
]

const files = Object.fromEntries([...names.map((name) => [`names/${name}`, `${name}\n`]), ["names/目录/x.txt", "x\n"]])

scenario("directory listings in ICU root order", { files }, async (ctx) => {
  await ctx.http("GET", "/api/fs/list?path=names")
})

scenario(
  "the read tool lists a directory in ICU root order",
  { files, turns: [call("read", { path: "names" }), say("Listed.")] },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "List the names")
  },
)
