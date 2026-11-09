// The JS side of the RegExp differential check: generates cases from fixed seeds and records what
// JavaScriptCore (bun) does with each, as lines examples/jsregex_check.rs recomputes and compares:
//
//   R <pattern> <flags> <constructor result>     a regex: ERR:F|P:<message> or OK:<source>:<flags>
//   S <subject> <results>                        one case: every operation on that regex and subject
//   X <input> <RegExp.escape result>
//   T <text> <search> <replacement> <replace> <replaceAll> <positions>
//
// Strings are written with `enc` below (printable ASCII but space as is, everything else \uXXXX), results with the small
// serializer `ser`; the Rust side writes the same text. The operation list must match jsregex_check.rs exactly.
//
// usage: bun harness/jsregex/oracle.ts [scale] | cargo run --release --offline --example jsregex_check

const scale = Number(process.argv[2] ?? "1")

// ---- deterministic randomness ---------------------------------------------------------------------------------
let seed = 0x9e3779b9
const random = () => {
  seed |= 0
  seed = (seed + 0x6d2b79f5) | 0
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}
const pick = <T>(list: readonly T[]): T => list[Math.floor(random() * list.length)]
const int = (n: number) => Math.floor(random() * n)

// ---- serialization --------------------------------------------------------------------------------------------
const enc = (s: string) => {
  let out = ""
  for (let i = 0; i < s.length; i++) {
    const u = s.charCodeAt(i)
    if (u === 0x22) out += '\\"'
    else if (u === 0x5c) out += "\\\\"
    else if (u > 0x20 && u < 0x7f) out += s[i]
    else out += "\\u" + u.toString(16).padStart(4, "0")
  }
  return out
}
const str = (s: string | undefined | null) => (s === undefined || s === null ? "null" : '"' + enc(s) + '"')
const num = (n: number) => (Object.is(n, -0) ? "-0" : String(n))
const groupsOf = (groups: Record<string, string | undefined> | undefined) =>
  groups === undefined ? "null" : "{" + Object.keys(groups).map((k) => str(k) + ":" + str(groups[k])).join(",") + "}"
const matchOf = (m: RegExpExecArray | RegExpMatchArray | null) =>
  m === null ? "null" : `[${m.index},[${Array.from(m, (c) => str(c)).join(",")}],${groupsOf(m.groups)}]`

const out: string[] = []
let cases = 0
const flush = () => {
  if (out.length) process.stdout.write(out.join("\n") + "\n")
  out.length = 0
}
const emit = (line: string) => {
  out.push(line)
  if (out.length >= 2000) flush()
}

const replacements = ["[$&]", "$1|$2", "$<n>$<m>", "$$-$`-$'", "$0$00$01$10$11$99$", "x", "$<$<>$<n$<x>", "$9$1$$$&$01x", "\\$1$'$`"]
const limits: Array<number | undefined> = [undefined, 0, 1, 2, 3]

// Every operation on one regex and subject, in the order jsregex_check.rs performs them.
const operations = (pattern: string, flags: string, s: string, full: boolean) => {
  const fresh = () => new RegExp(pattern, flags)
  const results: string[] = []
  const lastIndexes = full
    ? [0, 1, 2, s.length, s.length + 1, -1, 2.5, NaN, Infinity, -Infinity, 4294967296, 9007199254740993, -0.5]
    : [0, 1, s.length]
  for (const at of lastIndexes) {
    const re = fresh()
    re.lastIndex = at
    const m = re.exec(s)
    results.push(matchOf(m) + "@" + num(re.lastIndex))
  }
  {
    const re = fresh()
    for (let k = 0; k < 3; k++) {
      const m = re.exec(s)
      results.push(matchOf(m) + "@" + num(re.lastIndex))
    }
  }
  for (const r of full ? replacements : replacements.slice(0, 2)) {
    const re = fresh()
    re.lastIndex = 1
    results.push(str(s.replace(re, r)) + "@" + num(re.lastIndex))
  }
  {
    const re = fresh()
    const calls: string[] = []
    s.replace(re, (...args: unknown[]) => {
      const hasGroups = typeof args[args.length - 1] === "object" && args[args.length - 1] !== null
      const groups = hasGroups ? (args[args.length - 1] as Record<string, string | undefined>) : undefined
      const end = hasGroups ? args.length - 3 : args.length - 2
      const caps = args.slice(0, end) as Array<string | undefined>
      calls.push(`[${args[end]},[${caps.map((c) => str(c)).join(",")}],${groupsOf(groups)}]`)
      return ""
    })
    results.push("[" + calls.join(",") + "]@" + num(re.lastIndex))
  }
  for (const limit of full ? limits : limits.slice(0, 2)) {
    const re = fresh()
    re.lastIndex = 1
    const parts = s.split(re, limit)
    results.push("[" + parts.map((p) => str(p)).join(",") + "]@" + num(re.lastIndex))
  }
  const globalFlags = flags.includes("g") ? flags : flags + "g"
  {
    const re = new RegExp(pattern, globalFlags)
    re.lastIndex = 1
    const m = s.match(re)
    results.push((m === null ? "null" : "[" + Array.from(m, (c) => str(c)).join(",") + "]") + "@" + num(re.lastIndex))
  }
  {
    const re = new RegExp(pattern, globalFlags)
    re.lastIndex = 1
    const all = [...s.matchAll(re)]
    results.push("[" + all.map(matchOf).join(",") + "]@" + num(re.lastIndex))
  }
  for (const at of [3, -0]) {
    const re = fresh()
    re.lastIndex = at
    const index = s.search(re)
    results.push(index + "@" + num(re.lastIndex))
  }
  return results.join(" ")
}

const regex = (pattern: string, flags: string, subjects: string[], full = true) => {
  let re: RegExp
  try {
    re = new RegExp(pattern, flags)
  } catch (e) {
    const message = (e as Error).message
    const kind = /flags/i.test(message) && !message.startsWith("Invalid regular expression") ? "F" : "P"
    emit(`R\t${enc(pattern)}\t${enc(flags)}\tERR:${kind}:${enc(message)}`)
    cases++
    return
  }
  emit(`R\t${enc(pattern)}\t${enc(flags)}\tOK:${enc(re.source)}:${re.flags}`)
  for (const s of subjects) {
    emit(`S\t${enc(s)}\t${operations(pattern, flags, s, full)}`)
    cases++
  }
}

// ---- curated patterns -----------------------------------------------------------------------------------------
const curated: string[] = [
  // basics
  "", "a", "abc", "a|b", "a|", "|a", "ab|cd|ef", ".", "..", "a.c", "^", "$", "^a", "a$", "^$", "^abc$", "\\^", "\\$",
  "a b", "\\.", "a\\.b", "$a", "a^",
  // quantifiers
  "a*", "a+", "a?", "a*?", "a+?", "a??", "a{2}", "a{2,}", "a{2,3}", "a{0}", "a{0,1}", "a{1,2}?", "a{0}b", "(a){0}",
  "(?:ab)*", "(?:ab)+?", "(?:a|b)*c", "(a)*", "(a)+", "(a|b)*", "(a*)*", "(a*)+", "(a?)*", "(?:a*)*", "(a*?)*",
  "(a|ab)(c|bcd)(d*)", "(?:(a)|b)*", "(?:(a)|(b))+", "((a)|b)+", "(z)((a+)?(b+)?(c))*", "(.*?)a(?!(a+)b\\2c)\\2(.*)",
  "a{3}b", "x*y+$", "(a{2})*", ".*", ".+", ".*?", "[ab]*", "[^a]+", "\\w+", "\\W+", "\\d+", "\\D*", "\\s+", "\\S+",
  "(a+)+b", "(?:a?)+?b", "(a?)??", "(?:){3}", "()*", "(|a)+", "(a|)+", "a{1,3}a", "(?:aa|a)*?b", "a**", "a{,2}",
  "x{2147483648}", "a{4294967295,}", "(?:a{0,2}){2}", "(a{0,2}){2,3}?", "^(?:a+|b)*$", "(a|b)*?b",
  // classes
  "[abc]", "[^abc]", "[a-z]", "[A-Z]", "[a-zA-Z0-9_]", "[-a]", "[a-]", "[a\\-z]", "[\\]]", "[\\[]", "[]", "[^]",
  "[\\w-a]", "[a-\\w]", "[\\d-\\w]", "[\\s\\S]", "[\\b]", "[\\B]", "[\\cA]", "[\\c1]", "[\\c_]", "[\\c]", "[\\c-a]",
  "[\\1]", "[\\0]", "[\\01]", "[\\8]", "[.]", "[$^]", "[\\^]", "[^^]", "[\\u0041-\\u005A]", "[\\x41-\\x5a]", "[😀]",
  "[😀-😂]", "[\\ud83d\\ude00]", "[\\u{1F600}]", "[\\p{L}]", "[\\P{L}]", "[\\p{Lu}\\p{Nd}]", "[^\\p{Ll}]", "[z-a]",
  "[\\d-]", "[-\\d]", "[\\w-\\d]", "[a-c-e]", "[--a]", "[\\--a]", "[\\k]", "[\\k<a>]", "[\\-]", "[\\/]", "[/]",
  "[\\u{61}-\\u{7A}]", "[\\uD800-\\uDBFF]", "[\\uDC00-\\uDFFF]", "[^\\uD800-\\uDFFF]", "[\\x]", "[\\u]", "[\\u12]",
  "[a-zA-Z]+", "[^\\s\\d]", "[\\W\\d]", "[^\\W]", "[\\S\\s]+?",
  // escapes
  "\\n", "\\t", "\\r", "\\f", "\\v", "\\0", "\\00", "\\01", "\\012", "\\123", "\\377", "\\400", "\\8", "\\9", "\\18",
  "\\0100", "\\0042*", "[\\0100-\\u017f]", "\\00000", "\\1234", "\\3777", "\\777", "[\\0123]", "[\\3777]", "(a)\\0100",
  "\\x41", "\\x4", "\\xg", "\\u0041", "\\u004", "\\u{41}", "\\u{1F600}", "\\uD83D\\uDE00", "\\uD83D", "\\uDE00",
  "\\u{D83D}\\u{DE00}", "\\uD83D\\u{DE00}", "\\u{110000}", "\\u{}", "\\u{0041", "\\cJ", "\\cj", "\\c1", "\\c", "\\c*",
  "\\a", "\\q", "\\-", "\\/", "\\k", "\\k<a>", "\\k<a", "\\p", "\\p{L}", "\\P{L}", "\\p{Lu}", "\\p{Script=Greek}",
  "\\p{sc=Latn}", "\\p{scx=Hira}", "\\p{Script_Extensions=Latin}", "\\p{General_Category=Nd}", "\\p{gc=L}",
  "\\p{ASCII}", "\\p{Any}", "\\p{Assigned}", "\\p{Emoji}", "\\p{Emoji_Presentation}", "\\p{Extended_Pictographic}",
  "\\p{RGI_Emoji}", "\\p{Basic_Emoji}", "\\p{Emoji_Keycap_Sequence}", "\\p{RGI_Emoji_Flag_Sequence}",
  "\\p{RGI_Emoji_Modifier_Sequence}", "\\p{RGI_Emoji_ZWJ_Sequence}", "\\p{RGI_Emoji_Tag_Sequence}", "\\P{RGI_Emoji}",
  "\\p{L}+", "\\p{Foo}", "\\p{lu}", "\\p{L", "\\p{}", "\\p{gc=}", "\\p{=L}", "\\p{gc=L=x}", "\\p{Lowercase}",
  "\\p{Uppercase}", "\\p{White_Space}", "\\p{Alphabetic}", "\\p{ID_Start}\\p{ID_Continue}*", "\\p{Nd}+",
  "\\p{Cased}", "\\p{CWCF}", "\\p{Cn}", "\\p{Cs}", "\\p{Co}", "\\p{Zs}", "\\p{Sc}", "\\P{Any}", "\\p{RI}",
  "\\b", "\\B", "\\bfoo\\b", "\\Ba\\B", "a\\b", "\\w\\b\\W", "\\b\\w+\\b", "\\B\\w", "\\b+", "\\_", "\\%", "\\ ",
  // groups and references
  "(a)", "(a)(b)", "((a)b)", "(?:a)", "(?<n>a)", "(?<n>a)|(?<m>b)", "(?<n>a)\\k<n>", "\\k<n>(?<n>a)",
  "(?<n>a)|(?<n>b)", "(?:(?<n>a)|(?<n>b))\\k<n>", "(?:(?<n>a)|(?<n>b))+", "(?:(?<n>a)|(?<n>b))+\\k<n>", "(?<$>a)",
  "(?<_1>a)", "(?<é>a)", "(?<𝒜>a)", "(?<\\u0061>a)", "(?<a\\u{62}>b)", "(?<\\ud835\\udc9c>a)", "(?<1>a)",
  "(?<a>a)(?<a>b)", "(?<a>", "(?<a", "(?<>a)", "(?<a-b>c)", "(a)\\1", "(a)\\2", "\\1(a)", "(a\\1)", "(a)|\\1b",
  "(a)\\10", "(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\\10", "(a)\\01", "\\2(a)(b)", "(?:)", "()", "(|a)", "(a|)", "((((a))))",
  "(?<n>.)\\k<n>", "(?<n>.)\\k", "\\k(?<n>.)", "(?<n>.)\\k<m>", "(?<n>a)(?<m>b)?", "(?<m>b)?(?<n>a)",
  "(a)|(b)", "(?:(a)|b)\\1", "(\\w)\\1", "(.)(.)\\2\\1", "(a)(?:\\1)*", "((a)\\2)*", "(?<a>x)(?:|(?<a>y))",
  "((?<a>x))|(?<a>y)", "(?<a>a)|(?<b>b)(?<a>c)",
  // lookaround
  "(?=a)", "(?!a)", "(?=a)a", "a(?=b)", "a(?!b)", "(?=(a))", "(?=(a))\\1", "(?!(a))\\1", "(?<=a)b", "(?<!a)b",
  "(?<=(a))b", "(?<=\\1(a))b", "(?<=(a)\\1)b", "(?<=a+)b", "(?<=^a*)b", "(?<=(\\w)+)x", "(?<=\\b)a", "(?=a)*",
  "(?=a)+", "(?=a){2}", "(?!a)?", "(?<=a)*", "(?<=a){0}", "(?=(a))?", "(?=(a)){2}\\1", "(?:(?=(\\w))\\w)+",
  "a(?=b(?=c))", "(?<=(?<=a)b)c", "(?<!^)a", "(?<=$)", "(?<=.)", "(?<=\\ud800)\\udc00", "(?<=(.)(.))\\1\\2",
  "(?<=([ab])+)c", "(?<=(?:ab)+)c", "(?<=a(?=b)b)c", "(?<!(a))\\1", "(?=.*b)a", "^(?!.*b).*$", "(?<=(a+?))b",
  "(?<=(a+))b", "(?<=([a-z]){2})\\d", "(?<=\\w{2,3})x",
  // modifiers
  "(?i:a)b", "(?i:a)", "(?-i:a)b", "(?m:^a)", "(?s:.)", "(?i:\\w)", "(?i:[a-z])", "(?i:(a)\\1)", "(?i-m:^a$)",
  "(?ims:.)", "(?i:K)", "(?-s:.)", "(?-m:$)", "(?i:\\b)", "(?ii:a)", "(?i-i:a)", "(?-:a)", "(?x:a)", "(?i)", "(?i",
  "(?i-:a)", "(?is-:a)", "(?i-ms:a)", "(?i:a(?-i:b))", "(?i:[^a])", "(?i:\\p{Lu})",
  // unicode and surrogates
  "😀", "😀+", "^.$", "^..$", "\\ud83d", "\\udc00", "[\\ud800-\\udbff]", "[\\udc00-\\udfff]", "\\u{10000}",
  "^[^x]$", "𝒜", "[𝒜-𝒵]", "\\ud83d\\ude00+", ".\\udc00", "\\ud800.", "[^a]", "\\S", "\\W", "\\D", ".",
  // case-insensitive pairs
  "k", "K", "s", "ſ", "ß", "ẞ", "σ", "Σ", "ς", "i", "I", "İ", "ı", "\\u212a", "Ω", "ω", "θ", "ϑ", "ꭰ", "Ꭰ", "ა", "Ა",
  "Ǆ", "ǅ", "ǆ", "\\u1c89", "\\u1fd3", "[k-m]", "[\\u0100-\\u017f]+", "(ss)\\1", "\\u{10400}", "\\u{10428}",
  "[\\u{10400}]", "[^k]", "[^\\u212a]", "\\w", "[\\w]", "[^\\w]", "[\\W]", "ss", "SS", "[ſs]", "[a-zß]+",
  // v flag
  "[a&&b]", "[\\w&&\\d]", "[\\w--\\d]", "[[a-z]--[aeiou]]", "[\\q{abc|d|}]", "[\\q{}]", "[\\p{RGI_Emoji}]",
  "[\\p{RGI_Emoji}--\\q{😀}]", "[^\\q{a|b}]", "[[^a]&&[b-z]]", "[\\p{L}&&\\p{Script=Greek}]", "[\\p{Lu}&&k]",
  "[a-z&&[^aeiou]]", "[\\d\\q{12|345}]+", "[&&a]", "[a&&]", "[a&&&b]", "[a--]", "[a-z--b]", "[a--b&&c]", "[ab--c]",
  "[a&&b--c]", "[[a]&&[b]]", "[a!!b]", "[a!b]", "[^\\q{a|bc}]", "\\q{a}", "[\\q{a]", "[\\q]", "[^\\q{}]",
  "[^[\\q{ab}]]", "[(]", "[)]", "[{]", "[}]", "[|]", "[\\(]", "[\\&]", "[\\!]", "[\\#]", "[\\z]", "[[]]", "[a-b-c]",
  "[\\q{abc}a-c]", "[\\q{KK|ſ}]", "[\\w--k]", "[^\\P{Ll}]", "\\P{Ll}", "[^[a-c]--b]", "[[a-c]--[b]]+",
  "[\\p{ASCII}--\\p{L}]", "[\\q{😀|👍🏽}\\p{Emoji}]", "[[\\q{ab}a]&&[\\q{ab}]]", "[\\q{ab|a}]b",
  "[[^\\q{}]]", "[[^\\q{}a]]", "[[^!\\q{}]]", "[^[^\\q{}]]", "[^\\p{RGI_Emoji}&&a]", "[^a&&\\p{RGI_Emoji}]", "[^\\q{}&&a]",
  "[^a--\\q{ab}]", "[^\\q{ab}--a]", "[^[\\q{ab}--\\q{ab}]]", "[[^\\q{}][^\\q{}]]", "[a-\\q{b}]", "[a-\\q{", "[\\d-\\q{}]",
  // Annex B
  "]", "}", "{", "a{", "a{1", "a{,1}", "a{1,", "x{1}{2}", "{1}", "{1,2}", "a{2,1}", "a{1}?", "a{1}??", "^*", "$*",
  "(?:a){2}?", "\\c\\d", "a\\", "\\", "\\]",
  // line terminators and slashes
  "^a$", "a\\r?\\n", "\u2028", "a/b", "\\n$", "^\\n", "[^\\n]+", "\n", "\\\n", "a\nb", "[/]a/",
  // errors
  "(", ")", "[", "*", "+", "?", "(?", "(?a)", "(?<", "[b-a]", "(?<=a)+", "a)", "((a)", "(?:", "(?=", "(?<=",
]

const subjects: string[] = [
  "", "a", "aa", "aaa", "ab", "abc", "abcd", "aBc", "ABC", "abab", "baaab", "a\nb", "a\r\nb", "\n", "x\u2028y",
  "foo bar", "foo_bar-baz", "123 456", "a1b2c3", "hello world", " ", "\t", "ss", "SS", "ß", "ẞ", "ſ", "K", "k",
  "\u212a", "ςσΣ", "İıiI", "ꭰᎠ", "აᲐ", "ǄǅǆDž", "😀", "a😀b", "😀😁", "𝒜b", "\ud800", "\udc00", "\udc00\ud800",
  "a\ud800b", "\ud83d\ude00\ud83d", "𐐀𐐨", "👍🏽", "🇩🇪🇫🇷", "👨‍👩‍👧", "#️⃣", "é", "e\u0301", "αβγ", "Привет", "日本語",
  "$1", "a.b*c", "aaa\naaa", "<a href>", "x=1;y=2", "abcabc", "kKk", "\u0000", "\u{10FFFF}", "abba", "aab", "bc",
  "\u1c89\u1c8a", "\u1fd3\u0390", "{1}", "]}", "\\", "/", "ab12", "zzz", "aAaA", "a-b", "12", "345", "😀a", "abcd😀",
]

const flagSets = [
  "", "g", "i", "m", "s", "u", "v", "y", "gi", "gu", "gv", "iu", "iv", "gy", "my", "su", "gimsu", "gimsv", "d",
  "ui", "yu",
]

const pickSubjects = (n: number) => {
  const chosen = new Set<string>(["", "a", "abc"])
  while (chosen.size < n) chosen.add(pick(subjects))
  return [...chosen]
}

const curatedSubjects = Math.max(4, Math.round(16 * scale))
for (const pattern of curated) {
  for (const flags of flagSets) regex(pattern, flags, pickSubjects(curatedSubjects))
}

// ---- generated patterns ---------------------------------------------------------------------------------------
const atoms = [
  "a", "b", "c", "A", "B", " ", "-", ".", "\\w", "\\W", "\\d", "\\s", "\\S", "[ab]", "[^a]", "[a-c]", "[A-Z]", "\\n",
  "ſ", "K", "k", "ß", "😀", "\\u{1F600}", "\\ud83d", "\\p{L}", "[\\w-]", "\\b", "\\B", "^", "$", "\\1", "\\2",
  "\\k<x>", "\\k<y>", "[^\\s]", "\\x61", "\\u0042", "\\0", "[\\b]", "\\.", "\\-", "[\\q{ab|c}]", "[\\w&&[a-c]]",
]
const quantifiers = ["*", "+", "?", "{0,2}", "{1}", "{2,}", "{1,3}", "*?", "+?", "??", "{0,2}?", "{2}"]
const names = ["x", "y", "x"]
const node = (depth: number): string => {
  const roll = random()
  if (depth <= 0 || roll < 0.3) return pick(atoms)
  if (roll < 0.5) {
    const n = 2 + int(3)
    let s = ""
    for (let k = 0; k < n; k++) s += node(depth - 1)
    return s
  }
  if (roll < 0.6) return node(depth - 1) + "|" + node(depth - 1) + (random() < 0.3 ? "|" + node(depth - 1) : "")
  if (roll < 0.75) {
    const open = pick(["(", "(", "(?:", `(?<${pick(names)}>`, "(?=", "(?!", "(?<=", "(?<!", "(?i:", "(?-i:", "(?m:", "(?s:"])
    return open + node(depth - 1) + ")"
  }
  return node(depth - 1) + pick(quantifiers)
}
const randomSubject = () => {
  const alphabet = ["a", "a", "b", "b", "c", "A", "B", " ", "-", "\n", "ſ", "K", "k", "ß", "😀", "1", "\ud83d", "_"]
  const n = int(10)
  let s = ""
  for (let k = 0; k < n; k++) s += pick(alphabet)
  return s
}
const randomFlags = ["", "g", "i", "m", "s", "u", "v", "y", "gi", "iu", "iv", "gm", "su", "gy", "uy", "gis", "gimsv"]
const generated = Math.round(40000 * scale)
for (let k = 0; k < generated; k++) {
  const pattern = node(2 + int(3))
  regex(pattern, pick(randomFlags), [randomSubject(), randomSubject(), randomSubject()], false)
}

// ---- mutations ------------------------------------------------------------------------------------------------
const syntax = "()[]{}|*+?\\^$.-:=!<>,0123456789abckpquxPBdw&/ "
const mutate = (pattern: string) => {
  let s = pattern
  const edits = 1 + int(3)
  for (let k = 0; k < edits; k++) {
    const at = int(s.length + 1)
    const roll = random()
    if (roll < 0.4 || s.length === 0) s = s.slice(0, at) + pick([...syntax]) + s.slice(at)
    else if (roll < 0.7) s = s.slice(0, at) + s.slice(at + 1)
    else s = s.slice(0, at) + pick([...syntax]) + s.slice(at + 1)
  }
  return s
}
const mutations = Math.round(60000 * scale)
for (let k = 0; k < mutations; k++) {
  const base = random() < 0.5 ? pick(curated) : node(2 + int(2))
  regex(mutate(base), pick(randomFlags), [randomSubject()], false)
}
const badFlags = ["gg", "uv", "x", "dgimsuvy", "G", "ii", "gu ", " ", "yy", "vu", "-", "gimsuyd", "dgimsvy"]
for (const flags of badFlags) regex("a", flags, ["a"])

// ---- a richer grammar: Unicode, properties, v-flag sets, modifiers, references ---------------------------------
const richAtoms = [
  ...atoms, "é", "É", "\\u00e9", "😁", "𝒜", "\\ude00", "ẞ", "Σ", "σ", "ς", "İ", "ı", "\\u212a", "Ꭰ", "ꭰ", "𐐀", "𐐨",
  "ǅ", "\\p{Lu}", "\\p{Ll}", "\\P{L}", "\\p{N}", "\\p{Script=Greek}", "\\p{ASCII}", "\\p{Emoji}", "\\p{RGI_Emoji}",
  "[\\p{L}--[a-z]]", "[a-zß]", "[^\\p{L}]", "[\\u0100-\\u017f]", "[^\\w\\s]", "[\\q{ab|ß|😀}]", "[[a-z]&&[^aeiou]]",
  "[\\w--\\d]", "[\\p{Lu}&&\\p{Script=Latin}]", "[😀-😂]", "[\\ud800-\\udfff]", "\\x41", "\\101", "\\cA", "\\c", "\\8",
  "\\3", "(?<x>a|b)", "(?<y>.)", "[\\s\\S]", "[^]", "[]", "\\u{10400}", "[\\u{10400}-\\u{1044f}]", ".", "\\D", "\\s",
]
const richSubject = () => {
  const alphabet = ["a", "b", "A", "B", "é", "É", "Σ", "σ", "ς", "İ", "ı", "i", "I", "K", "k", "K", "Ꭰ", "ꭰ", "𐐀",
    "𐐨", "😀", "😁", "\ude00", "\ud83d", "\n", "\r", " ", " ", "1", "_", "ß", "ẞ", "ſ", "s", "S", "𝒜", "-", "ǅ", "ǆ"]
  const n = int(12)
  let s = ""
  for (let k = 0; k < n; k++) s += pick(alphabet)
  return s
}
const richNode = (depth: number): string => {
  const roll = random()
  if (depth <= 0 || roll < 0.3) return pick(richAtoms)
  if (roll < 0.5) {
    let s = ""
    for (let k = 2 + int(3); k > 0; k--) s += richNode(depth - 1)
    return s
  }
  if (roll < 0.6) return richNode(depth - 1) + "|" + richNode(depth - 1)
  if (roll < 0.75) {
    const open = pick(["(", "(?:", `(?<${pick(names)}>`, "(?=", "(?!", "(?<=", "(?<!", "(?i:", "(?-i:", "(?m:", "(?s:", "(?is-m:"])
    return open + richNode(depth - 1) + ")"
  }
  return richNode(depth - 1) + pick(quantifiers)
}
const richFlags = ["", "i", "u", "v", "iu", "iv", "g", "gi", "giu", "giv", "y", "iy", "uy", "m", "s", "ms", "gimsu", "gimsv"]
for (let k = 0; k < Math.round(40000 * scale); k++) {
  regex(richNode(2 + int(3)), pick(richFlags), [richSubject(), richSubject(), richSubject()], false)
}

// ---- long subjects ----------------------------------------------------------------------------------------------
const longPatterns = [
  "a*", "a+?b", "(a|b)*c", "\\w+", ".*?x", "(?:ab)+", "\\b\\w", "(?<=a)b", "[^x]*x", "(a)\\1*", "^.*$", "(?:a|b|c)+$",
  "(\\w)(\\w)?", "\\s+", "x*", "(?=(a+))a*b\\1", "[a-c]{2,5}?", "(?<!b)a", "😀+", ".", "(?:)", "\\d{3}", "(a*)+?b",
  "(?:a{0,3}b)*", "a(?!b)", "[^\\n]*\\n", "(?<w>\\w+)\\s\\k<w>",
]
const longFlags = ["", "g", "gi", "gu", "gm", "y", "gy", "gs", "giv"]
const longSubject = () => {
  const unit = richSubject() + pick(["a", "ab", "abc", "x", " ", "\n", "aaa", "😀"])
  const times = 20 + int(200)
  let s = ""
  for (let k = 0; k < times; k++) s += random() < 0.7 ? unit : pick(["a", "b", "c", "x", "😀", " ", "\n", "1"])
  return s
}
for (let k = 0; k < Math.round(1500 * scale); k++) regex(pick(longPatterns), pick(longFlags), [longSubject()], false)

// ---- case folding sweeps ----------------------------------------------------------------------------------------
const caseRanges: Array<[number, number]> = [
  [0x80, 0x2000], [0x2100, 0x2200], [0x2c00, 0x2d30], [0xa640, 0xa800], [0xab70, 0xabc0], [0xff00, 0xff60],
  [0x10400, 0x10450], [0x104b0, 0x10500], [0x10c80, 0x10d00], [0x118a0, 0x118e0], [0x16e40, 0x16e80], [0x1e900, 0x1e944],
]
const codePoint = () => {
  const [a, b] = pick(caseRanges)
  return a + int(b - a)
}
const hexEscape = (c: number) => "\\u{" + c.toString(16) + "}"
for (let k = 0; k < Math.round(15000 * scale); k++) {
  const c = codePoint()
  const ch = String.fromCodePoint(c)
  const related = [ch, ch.toUpperCase(), ch.toLowerCase(), String.fromCodePoint(c + 1), String.fromCodePoint(Math.max(0, c - 1))]
  const subject = related.join("") + ch.toUpperCase().toLowerCase()
  const d = c + int(40)
  const pattern = pick([
    ch, `[${ch}]`, `[^${ch}]`, `(${ch})\\1`, `(.)\\1`, `[${ch}-${String.fromCodePoint(d)}]`, `${ch}+`,
    `(?<n>${ch})\\k<n>`, `[${ch}${String.fromCodePoint(d)}]+`, `\\b${ch}`, `[\\w${ch}]`,
  ])
  const flags = pick(["i", "iu", "iv", "gi", "giu", "", "u"])
  const escaped = pattern.replace(/[\u{80}-\u{10ffff}]/gu, (x) => (flags.includes("u") || flags.includes("v") ? hexEscape(x.codePointAt(0)!) : x))
  regex(random() < 0.5 ? pattern : escaped, flags, [subject, related[1] + related[2], ch], false)
}

// ---- syntax fuzzing ---------------------------------------------------------------------------------------------
const fuzz = (tokens: string[], length: number) => {
  let s = ""
  for (let k = 1 + int(length); k > 0; k--) s += pick(tokens)
  return s
}
const classSetTokens = ["[", "]", "&&", "--", "-", "a", "b", "z", "\\d", "\\q{ab|c}", "\\q{", "}", "|", "[a]", "^", "\\p{L}",
  "!", "&", "(", "\\", "\\-", "\\b", "\\w", "\\W", "\\x41", "\\u{1F600}", "😀", "!!", "\\q{}", "\\p{RGI_Emoji}", "&&&", "---"]
const classTokens = ["[", "]", "-", "a", "b", "z", "\\d", "\\w", "\\W", "^", "\\b", "\\B", "\\c", "\\c1", "\\cA", "\\1", "\\0",
  "\\8", "\\x4", "\\u12", "\\u{41}", "\\k", "\\-", "\\]", "😀", "\\ud83d", "\\p{L}", "\\P{L}", "{", "}", "(", ")"]
const escapeTokens = ["\\", "a", "b", "c", "d", "k", "p", "u", "x", "q", "0", "1", "8", "{", "}", "<", ">", "-", "/", " ", "é",
  "😀", "A", "C", "1F600", "[", "]", "(", ")", "z", "_", "$", "^"]
const quantifierTokens = ["a", "{", "}", ",", "0", "1", "2", "9", "4294967295", "4294967296", "?", "*", "+", "(?=a)", "(?<=a)",
  "^", "\\b", "(", ")", "|", "x"]
const modifierTokens = ["(?", "i", "m", "s", "-", ":", ")", "a", "x", "(", "|", "P", "d", "ii"]
const nameTokens = ["(?<", "\\k<", ">", "a", "b", "$", "_", "1", "é", "𝒜", "\\u0061", "\\u{62}", "\\ud835\\udc9c", "\\ud835",
  "\\u{110000}", "\\u004", "\\", "-", ")", "(", "!", "=", "\\d", "‌"]
for (let k = 0; k < Math.round(20000 * scale); k++) {
  const roll = random()
  const [pattern, flags] =
    roll < 0.25 ? ["[" + fuzz(classSetTokens, 6) + pick(["]", "", "]]"]), pick(["v", "iv", "gv"])]
      : roll < 0.4 ? ["[" + fuzz(classTokens, 5) + pick(["]", ""]), pick(["", "i", "u", "iu"])]
      : roll < 0.55 ? ["\\" + fuzz(escapeTokens, 3), pick(["", "u", "v", "i"])]
      : roll < 0.7 ? [fuzz(quantifierTokens, 5), pick(["", "u"])]
      : roll < 0.85 ? [fuzz(modifierTokens, 6), pick(["", "i", "u"])]
      : [fuzz(nameTokens, 6), pick(["", "u"])]
  regex(pattern, flags, [richSubject()], false)
}

// ---- every short v-flag class over a token alphabet (JSC's class-set errors depend on the token order) ----------
{
  const tokens = ["a", "b", "\\d", "[a]", "\\q{ab}", "&&", "--", "-", "&", "!", "!!", "(", "]"]
  const walk = (prefix: string, depth: number) => {
    if (prefix.length) {
      regex("[" + prefix + "]", "v", ["ab!&-"], false)
      regex("[" + prefix, "v", [], false)
    }
    if (depth > 0) for (const t of tokens) walk(prefix + t, depth - 1)
  }
  walk("", 4)
  // A nested class starts in the state its parent was in, so nested classes get their own walk.
  const nestedTokens = ["a", "\\d", "[a]", "\\q{ab}", "&&", "--", "-", "&", "!", "(", "]", "\\-"]
  const parents = ["", "a", "\\d", "[a]", "\\q{ab}", "ab", "a&&b", "a&&\\d", "a-b", "\\-", "a&&"]
  for (const parent of parents) {
    const nested = (s: string, depth: number) => {
      for (const close of ["", "]", "]]"]) regex("[" + parent + "[" + s + close, "v", ["ab-"], false)
      if (depth > 0) for (const t of nestedTokens) nested(s + t, depth - 1)
    }
    nested("", 3)
  }
}
// ---- more exhaustive syntax: non-v classes, every escape, groups, references --------------------------------
{
  const enumerate = (tokens: string[], depth: number, visit: (s: string) => void) => {
    const walk = (s: string, d: number) => {
      if (s.length) visit(s)
      if (d > 0) for (const t of tokens) walk(s + t, d - 1)
    }
    walk("", depth)
  }
  const classTokens = ["a", "z", "-", "\\d", "\\w", "]", "^", "\\b", "\\c", "\\c1", "\\1", "\\-", "😀", "\\u{41}", "\\k"]
  enumerate(classTokens, 3, (s) => {
    for (const flags of ["", "u", "i"]) {
      regex("[" + s + "]", flags, ["az-1"], false)
      regex("[" + s, flags, [], false)
    }
  })
  const extras = ["\u0080", "é", "😀", "‌", " ", "\ud800", "\udc00"]
  for (let c = 0x20; c < 0x7f; c++) extras.push(String.fromCharCode(c))
  for (const ch of extras) {
    for (const pattern of [`\\${ch}`, `\\${ch}1`, `\\${ch}{1}`, `\\${ch}41`, `[\\${ch}]`, `[\\${ch}-z]`, `\\${ch}{41}`, `\\${ch}<a>(?<a>.)`])
      for (const flags of ["", "u", "v", "i"]) regex(pattern, flags, ["a1" + ch], false)
  }
  const groupTokens = ["(", "?", "<", ">", "=", "!", ":", "a", ")", "i", "-", "*"]
  enumerate(groupTokens, 4, (s) => {
    for (const flags of ["", "u"]) regex(s, flags, ["a"], false)
  })
  const referenceTokens = ["(a)", "(?<a>x)", "(?<b>y)", "\\1", "\\2", "\\k<a>", "\\k<b>", "\\k", "(", ")", "[\\k]", "|", "\\10"]
  enumerate(referenceTokens, 3, (s) => {
    for (const flags of ["", "u", "i"]) regex(s, flags, ["axyaa"], false)
  })
}

// ---- JSC's size limits (nesting deeper than about 37,000 groups is a RangeError from a native stack check, which
// the port does not reproduce) ------------------------------------------------------------------------------------
regex("a".repeat(1 << 20), "", ["a"], false)
regex("a".repeat((1 << 20) + 1), "", [], false)
regex("*" + "a".repeat(1 << 20), "", [], false)
regex("(a)".repeat(32768), "", [], false)
regex("(a)".repeat(32768) + "(?<n>b)", "", [], false)
regex("(a)".repeat(32769), "", [], false)
regex("(a)".repeat(32769) + "(", "", [], false)
regex("(?:".repeat(20000) + "a" + ")".repeat(20000), "", ["a", "b"], false)
regex("(?=".repeat(5000) + "a" + ")".repeat(5000), "", ["a"], false)
regex("[".repeat(20000) + "a" + "]".repeat(20000), "v", ["a"], false)

for (const digits of ["4294967295", "4294967296", "9007199254740993", "9223372036854775807", "9223372036854775808",
  "18446744073709551615", "18446744073709551616", "99999999999999999999999"]) {
  for (const pattern of [`a{${digits}}`, `{${digits}}`, `a{${digits},}`, `a{1,${digits}}`, `a{${digits}`, `\\${digits}`, `(a)\\${digits}`])
    for (const flags of ["", "u"]) regex(pattern, flags, ["aa"], false)
}

// ---- RegExp.escape and string-pattern replace -----------------------------------------------------------------
const escapeInputs = ["", "abc", "1a", "a1", "_x", "$^\\.*+?()[]{}|/", ",-=<>#&!%:;@~'`\"", " \t\n\v\f\r", "\u00a0\ufeff",
  "\u2028\u2029", "\ud800", "a\udc00", "😀", "é", "\u3000", "\u1680", "z9", "-1", "\u0085"]
for (let k = 0; k < Math.round(2000 * scale); k++) escapeInputs.push(randomSubject() + pick(["", "/", "#", "1", "\u2000"]))
for (let k = 0; k < Math.round(2000 * scale); k++) {
  let s = ""
  for (let n = int(6); n >= 0; n--) s += String.fromCharCode(random() < 0.7 ? 0x20 + int(0x60) : int(0x3000))
  escapeInputs.push(s)
}
for (const s of escapeInputs) {
  // @ts-ignore RegExp.escape is ES2025
  emit(`X\t${enc(s)}\t${str(RegExp.escape(s))}`)
  cases++
}
const texts = ["", "a", "aaa", "abcabc", "a-b-c", "😀a😀", "\ud800x\ud800"]
const searches = ["", "a", "aa", "b", "abc", "-", "😀", "\ud800", "zz"]
const templates = ["", "x", "[$&]", "$$", "$`|$'", "$1", "$<a>", "$", "$0"]
for (const t of texts)
  for (const s of searches)
    for (const r of templates) {
      const positions: number[] = []
      t.replaceAll(s, (_m: string, offset: number) => {
        positions.push(offset)
        return ""
      })
      emit(`T\t${enc(t)}\t${enc(s)}\t${enc(r)}\t${str(t.replace(s, r))}\t${str(t.replaceAll(s, r))}\t[${positions.join(",")}]`)
      cases++
    }

flush()
console.error(`oracle: ${cases} cases`)
