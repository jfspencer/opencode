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

const replacements = ["[$&]", "$1|$2", "$<n>$<m>", "$$-$`-$'", "$0$00$01$10$11$99$", "x"]
const limits: Array<number | undefined> = [undefined, 0, 1, 2, 3]

// Every operation on one regex and subject, in the order jsregex_check.rs performs them.
const operations = (pattern: string, flags: string, s: string, full: boolean) => {
  const fresh = () => new RegExp(pattern, flags)
  const results: string[] = []
  const lastIndexes = full ? [0, 1, 2, s.length, s.length + 1, -1, 2.5] : [0, 1, s.length]
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

// ---- RegExp.escape and string-pattern replace -----------------------------------------------------------------
const escapeInputs = ["", "abc", "1a", "a1", "_x", "$^\\.*+?()[]{}|/", ",-=<>#&!%:;@~'`\"", " \t\n\v\f\r", "\u00a0\ufeff",
  "\u2028\u2029", "\ud800", "a\udc00", "😀", "é", "\u3000", "\u1680", "z9", "-1", "\u0085"]
for (let k = 0; k < Math.round(2000 * scale); k++) escapeInputs.push(randomSubject() + pick(["", "/", "#", "1", "\u2000"]))
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
