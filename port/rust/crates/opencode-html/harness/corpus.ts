// The inputs of the differential check. Every expected output comes from running the TS reference on them; nothing
// here states what an output should be.
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

export const MAX = 5 * 1024 * 1024
export const BUDGET = MAX - 64 * 1024
export const CHUNK = 64 * 1024

/** A seeded PRNG (mulberry32), so a seed names a corpus. */
export class Rng {
  constructor(private state: number) {}
  next() {
    let t = (this.state = (this.state + 0x6d2b79f5) | 0)
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(n: number) {
    return Math.floor(this.next() * n)
  }
  pick<T>(items: readonly T[]): T {
    return items[this.int(items.length)]
  }
  chance(p: number) {
    return this.next() < p
  }
}

// ---- tag soup

const TAGS = [
  "p", "div", "span", "a", "b", "strong", "i", "em", "s", "strike", "del", "code", "pre", "h1", "h2", "h3", "h4",
  "h5", "h6", "ul", "ol", "li", "table", "thead", "tbody", "tfoot", "tr", "th", "td", "caption", "blockquote", "br",
  "hr", "img", "dl", "dt", "dd", "details", "summary", "head", "body", "html", "title", "script", "style",
  "noscript", "iframe", "object", "embed", "meta", "link", "template", "section", "article", "nav", "header",
  "footer", "main", "aside", "figure", "figcaption", "address", "fieldset", "form", "dialog", "input", "select",
  "option", "optgroup", "button", "datalist", "output", "textarea", "svg", "math", "path", "desc", "foreignObject",
  "mi", "mtext", "annotation-xml", "x-custom", "product-name", "font", "wbr", "rt", "rp", "ruby", "sup", "kbd",
  "var", "q", "u", "mark", "small", "label", "area", "base", "col", "source", "track", "param", "keygen", "frame",
  "isindex", "command", "basefont", "scripts", "styles", "titles", "sc", "st", "t", "x",
] as const
// Tags the converter treats specially, weighted up.
const HOT = ["p", "a", "code", "pre", "b", "em", "li", "ul", "ol", "table", "tr", "td", "th", "blockquote", "br", "div", "img", "details", "summary", "dt", "dd", "h2", "caption", "del", "span", "script"] as const

const WORDS = [
  "word", "text", "a", "Hello", "#", "+", "-", "1.", "12.", "3.5", "1", "*", "**", "_", "`", "``", "```", "~", "~~",
  "~~~", "[", "]", "<", ">", "|", "\\", "\\|", "&", "(", ")", "\"", "'", "=", "/", "!", "?", ";", "#heading",
  "+plus", "-dash", "99.", "0.", ">quote", "a|b", "snake_case", "x*y", "é", "😀", "中文", " ", " ",
  "é", "﻿", "ß", "İ", "K", "\u0000", "­", "​", "𝔄", "👩‍💻", "Ω", "\u0085", "\u000b",
]
const SPACES = [" ", " ", " ", "\n", "\t", "\f", "\r", "\r\n", "\u000b", " ", "  ", "\n\n\n", " \t ", "   \n", "　"]
const ENTITIES = [
  "&amp;", "&lt;", "&gt;", "&quot;", "&apos;", "&nbsp;", "&copy", "&copy;", "&not", "&notin;", "&notit;", "&#65;",
  "&#x41;", "&#X1F600;", "&#x1f600", "&#0;", "&#128;", "&#150;", "&#xD800;", "&#xdfff;", "&#1114111;", "&#1114112;",
  "&#99999999999;", "&#", "&#x", "&#;", "&#x;", "&", "&&", "&amp", "&Afr;", "&NotEqualTilde;", "&ampx", "&am",
  "&unknown;", "&#12abc", "&#x1g", "&AMP;", "&Amp;", "&lt", "&gtx", "&nbspx", "&#45;", "&#35;", "&#49;&#46;",
  "&#96;", "&#124;", "&#126;", "&#10;", "&#13;", "&#9;", "&#32;", "&#160;", "&#xA0;", "&Tab;", "&NewLine;",
  "&bne;", "&fjlig;", "&acE;", "&ThickSpace;", "&#x0;", "&#x80;", "&#x9F;", "&#x10FFFF;", "&#00000065;",
]
const ATTR_NAMES = [
  "href", "title", "alt", "src", "class", "start", "value", "colspan", "rowspan", "hidden", "aria-hidden", "open",
  "id", "style", "__proto__", "HREF", "Title", "data-x", "lang", "type", "constructor", "toString", "ALT", "Class",
  "Hidden", "ARIA-HIDDEN", "x", "a-b", "on", "Key",
]
const ATTR_VALUES = [
  "", "x", "https://example.com/a b", "/path(1)", "a\\b", "a]b", "say \"hi\"", "it's", "line one\n  line two",
  "language-ts", "lang-sh x", "x language-", "language- lang-rs", "language-c++ y", "lang- x", "true", "TRUE",
  "True", "false", "3", "0", "-1", "x", "0x10", " 7", "1e3", "99999999999999999999", "-0", "0X1F", "+5", " 5",
  "１", "\t42", "007", "9007199254740993", "0x20000000000001", "-99999999999999999999999", "1".repeat(400),
  "&amp;", "&copy", "&copy;x", "a&#65;b", "&#x1F600;", "&Afr;", "&notin", "a&b", "#frag", "  spaced  ", " ",
  "😀 emoji", "tab\there", "\\", "(", ")", "a\"b", "javascript:alert(1)",
]

function caseVariant(rng: Rng, name: string) {
  if (!rng.chance(0.15)) return name
  return rng.chance(0.5)
    ? name.toUpperCase()
    : Array.from(name, (c) => (rng.chance(0.5) ? c.toUpperCase() : c)).join("")
}

function attribute(rng: Rng) {
  const name = caseVariant(rng, rng.pick(ATTR_NAMES))
  const value = rng.chance(0.2) ? soupText(rng, 1 + rng.int(4)) : rng.pick(ATTR_VALUES)
  switch (rng.int(8)) {
    case 0:
      return ` ${name}`
    case 1:
      return ` ${name}='${value.replace(/'/g, "")}'`
    case 2:
      return ` ${name}=${value.replace(/[\s>"']/g, "") || "v"}`
    case 3:
      return ` ${name} = "${value.replace(/"/g, "")}"`
    case 4:
      return `\n${name}="${value.replace(/"/g, "&quot;")}"`
    case 5:
      return ` ${name}=${rng.pick(["\"unterminated", "'", "=", "a=b", "x/", "&amp;y"])}`
    default:
      return ` ${name}="${value.replace(/"/g, "")}"`
  }
}

function soupText(rng: Rng, words: number) {
  let out = ""
  for (let i = 0; i < words; i++) {
    const r = rng.next()
    if (r < 0.45) out += rng.pick(WORDS)
    else if (r < 0.7) out += rng.pick(SPACES)
    else if (r < 0.85) out += rng.pick(ENTITIES)
    else out += String.fromCodePoint(rng.pick([0x20 + rng.int(0x5f), 0xa0 + rng.int(0x700), 0x1f300 + rng.int(0x300), 0x4e00 + rng.int(0x100)]))
  }
  return out
}

const ODDITIES = [
  "<!-- comment -->", "<!-->", "<!--->", "<!---->", "<!-- a -- b -->", "<!-- <p>in</p> -->", "<!--x--!>", "<![CDATA[x<y]]>",
  "<![CDATA[", "<![cdata[x]]>", "<!DOCTYPE html>", "<!doctype html public \"x\">", "<!>", "<!x>", "<?xml version=\"1.0\"?>",
  "<?>", "</>", "</ >", "</3>", "</ p>", "< p>", "<3", "<", "</", "<a/b>", "<//>", "<p/>", "<div/>", "<br/>", "</br>",
  "</br >", "</p>", "</P>", "</li>", "</td>", "</table>", "</a>", "</code>", "</pre>", "</b>", "<img/>", "<img src=x/>",
  "<x y>", "<!-", "<!--", "-->", "]]>", "<?", "&", "<a href=x>", "<code>", "<pre>", "<svg><path/><title>t</title></svg>",
  "<math><mi>x</mi></math>", "<svg><p/>after</svg>", "<svg><br/></svg>", "<svg><desc><p/></desc></svg>", "</svg>",
  "</math>", "</title>", "</desc>", "<title>a &amp; b</title>", "<title>&notit;</title>", "<textarea><b>x</b></textarea>",
  "<script>if (a < b) {}</script>", "<script></scriptx></script>", "<script><</script>", "<script>a</SCRIPT >b",
  "<script><!-- </script> -->", "<style>p > a { }</style>", "<style></styl></style>", "<script/>", "<style/>x",
  "<title/>x</title>", "<scrip>t</scrip>", "<scripts>x</scripts>", "<sc>x</sc>", "<st>x</st>", "<t>x</t>",
  "<head><title>t</title></head>", "<body>", "</body>", "<html>", "\u0000", "<p\u0000>", "<a\thref=x\n>",
  "<a href=\"x\"title=\"y\">t</a>", "<a href='a'href='b'>", "<p hidden>h</p>", "<div aria-hidden=TRUE>h</div>",
  "<details><summary>s</summary>body</details>", "<details open>open</details>", "<ol start=\"5\"><li>", "<li value=\"9\">",
]

/** Random tag soup of about `size` tokens. */
export function soup(rng: Rng, size: number) {
  const open: string[] = []
  let out = ""
  for (let i = 0; i < size; i++) {
    const r = rng.next()
    if (r < 0.32) {
      out += soupText(rng, 1 + rng.int(6))
    } else if (r < 0.62) {
      const name = rng.chance(0.5) ? rng.pick(HOT) : rng.pick(TAGS)
      let tag = `<${caseVariant(rng, name)}`
      const attrs = rng.chance(0.4) ? 1 + rng.int(3) : 0
      for (let a = 0; a < attrs; a++) tag += attribute(rng)
      const end = rng.next()
      tag += end < 0.85 ? ">" : end < 0.93 ? "/>" : end < 0.97 ? " >" : ""
      out += tag
      open.push(name)
    } else if (r < 0.85) {
      const name = open.length && rng.chance(0.75) ? open.splice(open.length - 1 - (rng.chance(0.8) ? 0 : rng.int(open.length)), 1)[0] : rng.pick(TAGS)
      const close = rng.next()
      out += close < 0.85 ? `</${caseVariant(rng, name)}>` : close < 0.92 ? `</${name} >` : close < 0.96 ? `</${name} x="1">` : `</${name}`
    } else {
      out += rng.pick(ODDITIES)
    }
  }
  if (rng.chance(0.5)) while (open.length) out += `</${open.pop()}>`
  return out
}

// ---- structured documents (a grammar that nests properly, so the converter's structures get exercised)

function inlineContent(rng: Rng, depth: number): string {
  let out = ""
  const n = 1 + rng.int(4)
  for (let i = 0; i < n; i++) {
    const r = rng.next()
    if (depth > 3 || r < 0.45) out += soupText(rng, 1 + rng.int(5))
    else if (r < 0.55) out += `<code>${rng.chance(0.3) ? inlineContent(rng, depth + 1) : soupText(rng, 1 + rng.int(4))}</code>`
    else if (r < 0.65) out += `<a href="${rng.pick(ATTR_VALUES)}"${rng.chance(0.3) ? ` title="${rng.pick(ATTR_VALUES)}"` : ""}>${inlineContent(rng, depth + 1)}</a>`
    else if (r < 0.75) {
      const m = rng.pick(["b", "strong", "i", "em", "s", "del", "strike"])
      out += `<${m}>${rng.chance(0.2) ? " " : ""}${inlineContent(rng, depth + 1)}${rng.chance(0.2) ? " " : ""}</${m}>`
    } else if (r < 0.8) out += `<img src="${rng.pick(ATTR_VALUES)}" alt="${rng.pick(ATTR_VALUES)}"${rng.chance(0.3) ? ` title="${rng.pick(ATTR_VALUES)}"` : ""}>`
    else if (r < 0.85) out += "<br>"
    else if (r < 0.9) out += `<span>${inlineContent(rng, depth + 1)}</span>`
    else if (r < 0.95) out += `<${rng.pick(["kbd", "var", "sup", "u", "mark", "x-custom"])}>${inlineContent(rng, depth + 1)}</${"kbd"}>`
    else out += rng.pick(ODDITIES)
  }
  return out
}

export function document(rng: Rng, depth = 0): string {
  let out = ""
  const n = 1 + rng.int(depth === 0 ? 8 : 4)
  for (let i = 0; i < n; i++) {
    const r = rng.next()
    const deeper = depth < 4
    if (r < 0.2 || !deeper) out += `<p>${inlineContent(rng, 0)}</p>`
    else if (r < 0.28) out += `<h${1 + rng.int(6)}>${inlineContent(rng, 1)}</h${1 + rng.int(6)}>`
    else if (r < 0.38) {
      const list = rng.pick(["ul", "ol"])
      const start = rng.chance(0.3) ? ` start="${rng.pick(ATTR_VALUES)}"` : ""
      let items = ""
      for (let k = 0; k < 1 + rng.int(4); k++)
        items += `<li${rng.chance(0.15) ? ` value="${rng.pick(ATTR_VALUES)}"` : ""}>${rng.chance(0.5) ? inlineContent(rng, 0) : document(rng, depth + 1)}${rng.chance(0.8) ? "</li>" : ""}`
      out += `<${list}${start}>${items}</${list}>`
    } else if (r < 0.46) {
      let rows = rng.chance(0.3) ? `<caption>${inlineContent(rng, 1)}</caption>` : ""
      const width = 1 + rng.int(4)
      for (let k = 0; k < 1 + rng.int(4); k++) {
        rows += "<tr>"
        const cells = rng.chance(0.8) ? width : 1 + rng.int(4)
        for (let c = 0; c < cells; c++) {
          const tag = k === 0 && rng.chance(0.5) ? "th" : "td"
          const span = rng.chance(0.05) ? ` ${rng.pick(["colspan", "rowspan"])}="${rng.pick(["2", "", "0"])}"` : ""
          rows += `<${tag}${span}>${rng.chance(0.15) ? document(rng, depth + 1) : inlineContent(rng, 1)}</${tag}>`
        }
        rows += rng.chance(0.9) ? "</tr>" : ""
      }
      out += `<table>${rng.chance(0.1) ? soupText(rng, 2) : ""}${rng.chance(0.3) ? `<tbody>${rows}</tbody>` : rows}</table>`
    } else if (r < 0.54) out += `<blockquote>${document(rng, depth + 1)}</blockquote>`
    else if (r < 0.62) {
      const lang = rng.chance(0.5) ? ` class="${rng.pick(ATTR_VALUES)}"` : ""
      const text = Array.from({ length: 1 + rng.int(5) }, () => soupText(rng, 1 + rng.int(8))).join(rng.pick(["\n", "<br>", "\r\n", "\n\n"]))
      out += rng.chance(0.6) ? `<pre><code${lang}>${text}</code></pre>` : `<pre>${text}${rng.chance(0.3) ? inlineContent(rng, 1) : ""}</pre>`
    } else if (r < 0.68) out += `<dl><dt>${inlineContent(rng, 1)}</dt><dd>${inlineContent(rng, 1)}</dd>${rng.chance(0.5) ? "<dt>x<dd>y" : ""}</dl>`
    else if (r < 0.73) out += `<details${rng.chance(0.5) ? " open" : ""}><summary>${inlineContent(rng, 1)}</summary>${document(rng, depth + 1)}</details>`
    else if (r < 0.78) out += "<hr>"
    else if (r < 0.84) out += `<div${rng.chance(0.1) ? " hidden" : ""}>${document(rng, depth + 1)}</div>`
    else if (r < 0.88) out += `<a href="${rng.pick(ATTR_VALUES)}">${document(rng, depth + 1)}</a>`
    else if (r < 0.92) out += `<${rng.pick(["b", "em", "del"])}>${document(rng, depth + 1)}</${rng.pick(["b", "em", "del"])}>`
    else if (r < 0.96) out += soup(rng, 3 + rng.int(10))
    else out += `<${rng.pick(["script", "style", "noscript", "template", "iframe", "object", "head", "title", "svg"])}>${inlineContent(rng, 2)}</${rng.pick(["script", "style", "title", "svg", "div"])}>`
  }
  return out
}

// ---- inputs around the 64 Ki-unit write chunks

/** A document whose interesting part straddles a chunk boundary at a random offset. */
export function chunkStraddle(rng: Rng) {
  const chunks = 1 + rng.int(rng.chance(0.8) ? 1 : 3)
  const snippet = rng.chance(0.5) ? soup(rng, 4 + rng.int(12)) : document(rng, 3)
  const before = rng.int(Math.min(snippet.length + 2, 40))
  const target = chunks * CHUNK - before
  const fillers = [
    () => "x",
    () => " ",
    () => "😀",
    () => "a😀",
    () => "é",
    () => "<b>x</b>",
    () => "&amp;",
    () => "1.",
    () => "-a ",
    () => "<p>w</p>",
    () => "<!-- -->",
    () => "<div>",
    () => "\u{1d504}",
    () => "&Afr;",
  ]
  const unit = rng.pick(fillers)()
  let prefix = rng.pick(["", "<p>", "<pre>", "<code>", "<table><tr><td>", "<blockquote>", "<a href=x>", "<title>", "<script>", "<div title=\""])
  while (prefix.length < target) prefix += unit
  prefix = prefix.slice(0, target)
  return prefix + snippet + (rng.chance(0.3) ? soup(rng, 5) : "")
}

// ---- inputs at the 5 MiB budget

export function budget(rng: Rng) {
  const tails = [
    () => `<p><code>${rng.pick(["x", "`x", "x`", " x ", "``", "😀`", "a``b`c", "   ", " ` ", soupText(rng, 3)])}</code></p>`,
    () => `<pre>${rng.pick(["x", "😀é", "a\nb", "```", "~~~\n```", soupText(rng, 6)])}</pre>`,
    () => `<blockquote><pre>${rng.pick(["x", "x\ny\n", "😀", soupText(rng, 4)])}</pre></blockquote>`,
    () => `<p><img src="${rng.pick(ATTR_VALUES)}" alt="${rng.pick(["alt été", "a]b", "😀😀😀", "x".repeat(rng.int(40))])}"></p>`,
    () => `<p><a href="/x">link ${soupText(rng, 2)}</a> after</p>`,
    () => `<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>${soupText(rng, 2)}</td></tr></table>`,
    () => `<ul><li>item<ul><li>${soupText(rng, 2)}</li></ul></li></ul>`,
    () => `<p>tail ${soupText(rng, 3)}</p>`,
    () => `<p><b>bold</b> <em> x </em><code>c</code></p>`,
    () => document(rng, 3),
  ]
  const unitBytes = rng.pick([1, 1, 1, 2, 3, 4])
  const unit = { 1: "*", 2: "é", 3: "中", 4: "😀" }[unitBytes]!
  const fill = rng.chance(0.5) ? (unit === "*" ? "x" : unit) : unit
  const escaped = fill === "*" ? 2 : 1
  const slack = rng.int(rng.chance(0.5) ? 40 : 400) - 20
  const count = Math.max(0, Math.floor((BUDGET - slack) / (unitBytes * escaped)))
  let html = `<p>${rng.chance(0.2) ? "x" : ""}${fill.repeat(count)}</p>`
  for (let i = 0; i < 1 + rng.int(3); i++) html += rng.pick(tails)()
  if (rng.chance(0.1)) html += "x".repeat(rng.int(MAX - html.length > 0 ? Math.min(200_000, MAX - html.length) : 1))
  return html
}

// ---- end of input in the middle of a construct

export const EOF_TAILS = [
  "</a ", "</a x", "</a\t😀", "</a 😀", "</a 中", "</a xy😀", "</div\n", "<br/", "<a href=x/", "<b/", "<p/", "<x/",
  "&#12", "&#x1F6", "&#x1f600", "&#x", "&#", "&#99", "&#0", "&amp", "&notin", "&no", "&", "&&", "&Afr", "&Afr;",
  "<", "</", "<a", "<a ", "<a href", "<a href=", "<a href=\"x", "<a href='x", "<a href=x", "<a href=\"&#12", "<a href=\"&amp",
  "<a title=&#x41", "<!", "<!-", "<!--", "<!-- x", "<!-- x -", "<!-- x --", "<![CDATA[x", "<![CDATA[x]", "<![CD", "<?x",
  "<!doctype", "</3", "</ 3", "<s", "<sc", "<script", "<scripts", "<script>x", "<script>x</scr", "<title>x&amp",
  "<title>x&#12", "<title>a</titl", "<style>x</sty", "<style>x</style", "<p>x", "😀", "&#xD83D;", "x ", "</title ",
]

/** A document of about `size` units ending inside a construct. */
export function eofQuirk(rng: Rng) {
  const size = rng.pick([0, 0, 10, 100, 1000, CHUNK - 3, CHUNK + 100, 3 * CHUNK + rng.int(CHUNK)])
  let body = ""
  while (body.length < size) body += rng.chance(0.7) ? document(rng, 3) : soup(rng, 20)
  if (rng.chance(0.3)) body = body.slice(0, size)
  return body + rng.pick(EOF_TAILS) + (rng.chance(0.2) ? rng.pick(["xyz", " ", "😀", "中文"]) : "")
}

/** `</a ` or `<br/` left open at the end of input after a chunk boundary, with the boundary on either side of an
 * astral character: htmlparser2 then emits text that starts at the last unit of the oldest chunk it holds. */
export function wrappedSlices() {
  const out: string[] = []
  for (const tail of ["</a ", "<br/"])
    for (const before of [0, 1, 2, 3, 5, 6, 7])
      for (const after of ["y😀zz", "😀zz", "y", "zz", "😀", "y中zz", "y😀"]) {
        const k = CHUNK - before - tail.length
        out.push("x".repeat(k) + tail + after)
        out.push("<p>" + "x".repeat(k - 3) + tail + after)
        out.push("x".repeat(k + CHUNK) + tail + after)
      }
  return out
}

/** Text that runs out of budget between the halves of a surrogate pair (an astral entity is two text events). */
export function splitAtBudget() {
  const out: string[] = []
  for (let r = 0; r < 9; r++)
    for (const tail of ["&Afr;", "&Afr;&Bopf;", "😀", "&NotEqualTilde;"]) {
      out.push(`<p>${"x".repeat(BUDGET - r)}${tail}</p>`)
      out.push(`<p>${"x".repeat(BUDGET - r - 6)}<code>${tail}</code></p>`)
    }
  return out
}

/** Ordered lists numbered from values parseInt reads in every way it can. */
export function numbering(rng: Rng) {
  const digits = (n: number, alphabet: string) => Array.from({ length: n }, () => rng.pick([...alphabet])).join("")
  const value = () =>
    rng.pick([
      () => digits(1 + rng.int(25), "0123456789"),
      () => "0x" + digits(1 + rng.int(22), "0123456789abcdefABCDEF"),
      () => "0X" + digits(250 + rng.int(20), "0123456789abcdef"),
      () => "0x" + "0".repeat(rng.int(300)) + digits(1 + rng.int(15), "123456789abcdef"),
      () => "-" + digits(1 + rng.int(25), "0123456789"),
      () => rng.pick(["", " ", " ", "﻿", " ", "\t", "᠎", "+", "-"]) + digits(1 + rng.int(5), "0123456789"),
      () => digits(300 + rng.int(20), "123456789"),
      () => rng.pick(ATTR_VALUES),
    ])()
  return `<ol start="${value()}"><li>a</li><li value="${value()}">b</li><li>c</li></ol><ol><li value="${value()}">d</li></ol>`
}

// ---- hand-written pages and their mutations

export function pages(dir: string) {
  return readdirSync(dir)
    .filter((f) => f.endsWith(".html"))
    .sort()
    .map((f) => ({ name: f, html: readFileSync(join(dir, f), "utf8") }))
}

export function mutate(rng: Rng, html: string) {
  switch (rng.int(5)) {
    case 0:
      return html.slice(0, rng.int(html.length + 1))
    case 1: {
      const at = rng.int(html.length + 1)
      return html.slice(0, at) + rng.pick([soup(rng, 3), rng.pick(ODDITIES), rng.pick(ENTITIES)]) + html.slice(at)
    }
    case 2: {
      const at = rng.int(html.length + 1)
      return html.slice(0, at) + html.slice(at + 1 + rng.int(20))
    }
    case 3:
      return html.replace(/<\/[a-z0-9]+>/g, (m) => (rng.chance(0.2) ? "" : m))
    default:
      return html.replace(/[a-z]+/g, (m) => (rng.chance(0.05) ? m.toUpperCase() : m))
  }
}

// ---- the HTML the conformance scenarios serve

/** Every body the webfetch scenarios' responders answer, found by evaluating the scenario file with stubs. */
export function scenarioBodies(file: string) {
  const js = new Bun.Transpiler({ loader: "ts" }).transformSync(readFileSync(file, "utf8"))
  const code = js.replace(/^import .*$/gm, "")
  const urls: string[] = []
  const scenarios: { name: string; options: { responders?: ((request: unknown) => unknown)[] } }[] = []
  const call = (_name: string, input: string | { url?: string }) => {
    const url = typeof input === "string" ? input : input.url
    if (url) urls.push(url)
    return { call: input }
  }
  new Function("HIDDEN_TOOLS", "call", "say", "scenario", code)([], call, () => ({}), (name: string, options: never) => scenarios.push({ name, options }))
  const bodies: { name: string; body: string; contentType: string }[] = []
  for (const { name, options } of scenarios)
    for (const responder of options.responders ?? [])
      for (const url of new Set(urls)) {
        const turn = responder({ kind: "other", url: url.split("#")[0], method: "GET", headers: {} }) as
          | { raw?: string; headers?: Record<string, string> }
          | undefined
        if (turn && typeof turn.raw === "string" && turn.raw)
          bodies.push({ name: `${name} ${url}`, body: turn.raw, contentType: turn.headers?.["content-type"] ?? "" })
      }
  return bodies
}

// ---- the upstream tests

/** tool-html-markdown-budget.test.ts runs its cases in a child process; its table, as the test builds the input. */
export function upstreamBudgetCases() {
  const budget = MAX - 64 * 1024
  const table: [number, string, boolean][] = [
    [budget, "x", false],
    [budget - 13, "x", false],
    [64, "x", false],
    [budget - 15, "x", false],
    [budget - 14, "x", false],
    [budget - 18, "😀é", false],
    [budget - 23, "x", true],
    [budget - 22, "x", true],
    [budget - 21, "x", true],
  ]
  return table.map(([count, payload, quoted]) => {
    const code = `<pre>${payload}</pre>`
    return `<p>${"x".repeat(count)}</p>${quoted ? `<blockquote>${code}</blockquote>` : code}`
  })
}

// ---- webfetch helpers

export const CONTENT_TYPES = [
  "", "text/html", "text/html; charset=utf-8", "TEXT/HTML", " text/html ", "text/plain", "text/markdown; charset=utf-8",
  "application/json", "application/ld+json", "application/xml", "application/atom+xml", "image/svg+xml", "image/png",
  "image/vnd.fastbidsheet", "IMAGE/JPEG", "application/javascript", "application/x-javascript", "application/zip",
  "application/octet-stream", "text/xhtml", "application/xhtml+xml", "multipart/form-data; boundary=x", ";",
  "text/html;", ";text/html", " text/html", "text/html ", "Text/HTML; Charset=UTF-8", "image/", "text/",
  "application/json; x", "application/jsonx", "x+json", "x+xml", "\ttext/plain", "text/plain﻿", "TEXT/PLAIN;q=1",
]
