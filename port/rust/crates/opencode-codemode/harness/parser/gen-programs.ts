// A seeded random program generator for the parser corpus: mostly valid programs of every expression, statement,
// pattern and class form, nested a few levels deep, with random whitespace, comments and line breaks between tokens.
export function randomPrograms(seed: number, count: number) {
  let state = seed >>> 0
  const rand = () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  const pick = <T>(list: readonly T[]): T => list[Math.floor(rand() * list.length)]
  const chance = (p: number) => rand() < p
  const times = (max: number, f: () => string, sep = ", ") => Array.from({ length: Math.floor(rand() * (max + 1)) }, f).join(sep)

  const space = () => (chance(0.85) ? " " : pick(["", "  ", "\n", "\t", " /* c */ ", "\r\n", " // c\n", " "]))
  const names = ["a", "b", "c", "x", "y", "$", "_v", "foo", "of", "get", "set", "async", "let", "static", "yield", "await", "as", "from", "été", "\u{1d49c}"]
  const name = () => pick(names)
  const safeName = () => pick(["a", "b", "c", "x", "y", "$", "_v", "foo", "été", "\u{1d49c}"])
  const number = () =>
    pick(["0", "1", "42", "3.14", ".5", "1e3", "1e-7", "0x1f", "0o17", "0b101", "1_000", "10n", "0xffn", "077", "08", "2e308", "1.5e+300"])
  const string = () => pick(['"a"', "'b'", '"\\n\\t"', "'\\x41\\u0042\\u{43}'", '"\\\'"', "'é'", '"use strict"', "''", '"\\0"'])
  const regex = () => pick(["/a/", "/a+b*/g", "/[a-z]/i", "/(?<n>x)\\k<n>/", "/\\p{L}/u", "/[\\p{L}--a]/v", "/(?i:a)/", "/\\//"])
  const template = (depth: number): string =>
    "`" + times(2, () => pick(["a", "\\n", "\n", "$", "{"]) + "${" + expr(depth + 1) + "}", "") + pick(["", "z", "\\`"]) + "`"

  function primary(depth: number): string {
    if (depth > 3) return pick([safeName(), number(), string(), "this", "null", "true"])
    switch (Math.floor(rand() * 24)) {
      case 0: return safeName()
      case 1: return number()
      case 2: return string()
      case 3: return regex()
      case 4: return template(depth)
      case 5: return "[" + trailing(times(3, () => pick(["", "..."]) + expr(depth + 1))) + "]"
      case 6: return "{" + trailing(times(3, () => property(depth + 1))) + "}"
      case 7: return "((" + params(depth) + ")" + space() + "=>" + space() + arrowBody(depth) + ")"
      case 8: return "(" + safeName() + space() + "=>" + space() + arrowBody(depth) + ")"
      case 9: return "(async (" + params(depth) + ") => " + arrowBody(depth) + ")"
      case 10: return pick(["function", "function*", "async function", "async function*"]) + pick(["", " f"]) + "(" + params(depth) + ") " + block(depth)
      case 11: return "class " + pick(["", "K "]) + pick(["", "extends B "]) + classBody(depth)
      case 12: return "new " + safeName() + pick(["", "()", "(" + expr(depth + 1) + ")", ".x"])
      case 13: return "(" + expr(depth + 1) + ")"
      case 14: return "(" + expr(depth + 1) + ", " + expr(depth + 1) + ")"
      case 15: return safeName() + "`" + "a${" + expr(depth + 1) + "}b" + "`"
      case 16: return "this"
      case 17: return pick(["null", "true", "false"])
      case 18: return "import(" + string() + ")"
      case 19: return "[...a, ...b]"
      case 20: return "{ ...a, b }"
      case 21: return "(" + pick(["a", "[a, b]", "{ a }"]) + " = " + expr(depth + 1) + ")"
      case 22: return "super_"
      default: return safeName()
    }
  }
  function postfix(depth: number): string {
    let e = primary(depth)
    for (let i = 0; i < 3 && depth < 4 && chance(0.4); i++)
      e += pick([
        "." + pick(["a", "b", "if", "class", "of", "length"]),
        "?." + safeName(),
        "[" + expr(depth + 1) + "]",
        "?.[" + expr(depth + 1) + "]",
        "(" + times(2, () => expr(depth + 1)) + ")",
        "?.(" + expr(depth + 1) + ")",
        "`t`",
      ])
    if (chance(0.05)) e = pick(["++", "--"]) + safeName()
    if (chance(0.05)) e = safeName() + pick(["++", "--"])
    return e
  }
  function unary(depth: number): string {
    if (depth > 6) return safeName()
    if (chance(0.15)) return pick(["!", "-", "+", "~", "typeof ", "void ", "delete "]) + unary(depth + 1)
    if (chance(0.05)) return "await " + unary(depth + 1)
    return postfix(depth)
  }
  function expr(depth: number): string {
    let e = unary(depth)
    for (let i = 0; i < 3 && depth < 4 && chance(0.35); i++)
      e += space() + pick(["+", "-", "*", "/", "%", "**", "==", "===", "!=", "<", ">=", "<<", ">>>", "&", "|", "^", "&&", "||", "??", "in", "instanceof"]) + space() + unary(depth + 1)
    if (depth < 4 && chance(0.08)) e += " ? " + expr(depth + 1) + " : " + expr(depth + 1)
    if (depth < 4 && chance(0.08)) e = safeName() + space() + pick(["=", "+=", "-=", "**=", "&&=", "||=", "??=", ">>>="]) + space() + e
    if (depth < 3 && chance(0.05)) e = pick(["[a, , b = 1, ...c]", "{ a, b: [c] = [], ...d }", "[x.y, z[0]]"]) + " = " + e
    return e
  }
  function property(depth: number): string {
    return pick([
      () => safeName(),
      () => safeName() + ": " + expr(depth),
      () => string() + ": " + expr(depth),
      () => number() + ": " + expr(depth),
      () => "[" + expr(depth) + "]: " + expr(depth),
      () => safeName() + "(" + params(depth) + ") " + block(depth),
      () => "get " + safeName() + "() " + block(depth),
      () => "set " + safeName() + "(v) " + block(depth),
      () => "async " + safeName() + "() " + block(depth),
      () => "*" + safeName() + "() " + block(depth),
      () => "async *" + safeName() + "() " + block(depth),
      () => "..." + expr(depth),
      () => pick(["get", "set", "async", "static", "of"]) + ": 1",
    ])()
  }
  function pattern(depth: number): string {
    if (depth > 3) return safeName()
    return pick([
      () => safeName(),
      () => "[" + times(3, () => pick(["", pattern(depth + 1), pattern(depth + 1) + " = " + expr(depth + 1)])) + pick(["", ", ...r"]) + "]",
      () => "{" + times(3, () => pick([safeName(), safeName() + ": " + pattern(depth + 1), safeName() + " = " + expr(depth + 1), "[" + expr(depth + 1) + "]: " + pattern(depth + 1)])) + pick(["", ", ...r"]) + "}",
    ])()
  }
  const trailing = (list: string) => (list && chance(0.3) ? list + "," : list)
  const params = (depth: number) => {
    const list = times(3, () => pattern(depth + 1) + (chance(0.2) ? " = " + expr(depth + 1) : ""))
    return chance(0.2) ? (list ? list + ", " : "") + "...rest" : list
  }
  const arrowBody = (depth: number) => (chance(0.5) ? expr(depth + 1) : chance(0.2) ? "({ a: 1 })" : block(depth))
  function classBody(depth: number): string {
    const element = () =>
      pick([
        () => safeName() + "() " + block(depth + 1),
        () => "static " + safeName() + "() " + block(depth + 1),
        () => "#p = " + expr(depth + 1) + ";",
        () => "static #q;",
        () => safeName() + " = " + expr(depth + 1) + ";",
        () => "get " + safeName() + "() { return this.#p }",
        () => "set " + safeName() + "(v) {}",
        () => "static { " + statement(depth + 1) + " }",
        () => "async *" + safeName() + "() { yield await 1 }",
        () => "[" + expr(depth + 1) + "]() {}",
        () => "constructor(" + params(depth + 1) + ") { " + pick(["", "super()"]) + " }",
        () => "'quoted'() {}",
        () => "static async x() {}",
        () => "#m() {} m2() { #m in this; this.#m() }",
      ])()
    return "{ #p; " + times(4, element, " ") + " }"
  }
  function block(depth: number): string {
    if (depth > 3) return "{}"
    return "{" + space() + times(3, () => statement(depth + 1), space() + pick([";", "\n", "; "]) + space()) + space() + "}"
  }
  function statement(depth: number): string {
    if (depth > 4) return expr(depth) + ";"
    return pick([
      () => expr(depth) + ";",
      () => pick(["var", "let", "const"]) + " " + pattern(depth) + " = " + expr(depth) + pick([";", "", ", z = 1;"]),
      () => "if (" + expr(depth) + ") " + statement(depth + 1) + pick(["", " else " + statement(depth + 1)]),
      () => "for (" + pick(["let i = 0", "var i = 0", "i = 0", ""]) + "; " + pick(["i < 3", ""]) + "; " + pick(["i++", ""]) + ") " + statement(depth + 1),
      () => "for (" + pick(["const", "let", "var", ""]) + " " + pattern(depth) + " of " + expr(depth) + ") " + statement(depth + 1),
      () => "for (" + pick(["const", "let", "var"]) + " k in " + expr(depth) + ") " + statement(depth + 1),
      () => "while (" + expr(depth) + ") " + statement(depth + 1),
      () => "do " + statement(depth + 1) + " while (" + expr(depth) + ")",
      () => block(depth),
      () => "try " + block(depth) + pick([" catch (e) " + block(depth), " catch " + block(depth), " finally " + block(depth), " catch ({ message }) " + block(depth) + " finally {}"]),
      () => "switch (" + expr(depth) + ") { case 1: " + statement(depth + 1) + " break; default: " + statement(depth + 1) + " }",
      () => "lbl: for (;;) { break lbl }",
      () => "return " + expr(depth) + ";",
      () => "throw " + expr(depth) + ";",
      () => "function f" + Math.floor(rand() * 1000) + "(" + params(depth) + ") " + block(depth),
      () => "async function g" + Math.floor(rand() * 1000) + "() { await " + expr(depth) + " }",
      () => "function* h" + Math.floor(rand() * 1000) + "() { yield " + expr(depth) + "; yield* [] }",
      () => "class C" + Math.floor(rand() * 1000) + " " + classBody(depth),
      () => "{ using u = " + expr(depth) + " }",
      () => "if (a) { await using au = b }",
      () => ";",
      () => "debugger;",
      () => '"use strict";',
      () => "with (o) " + statement(depth + 1),
      () => "import " + pick(["d", "* as ns", "{ a as b, c }", "d, { e }"]) + " from " + string() + pick(["", ' with { type: "json" }']),
      () => "export " + pick(["const ex = 1", "function exf() {}", "{ a as default }", "* from 'm'", "default " + expr(depth)]),
    ])()
  }
  const out: string[] = []
  for (let i = 0; i < count; i++) out.push(times(5, () => statement(0), pick(["\n", ";", " ", "\r\n"])) || ";")
  return out
}
