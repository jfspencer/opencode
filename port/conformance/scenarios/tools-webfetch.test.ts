// webfetch: every fetched URL is answered by a responder (no network). Covers the three output formats, the
// HTML-to-markdown conversion, content types, redirects, size and time limits, and permission rules.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type ProviderRequest, type Responder, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

/** Answers GET requests to `site` from a route table; anything else under `site` is a 404. */
function site(origin: string, routes: Record<string, Turn | ((request: ProviderRequest) => Turn)>): Responder {
  return (request) => {
    if (request.kind !== "other" || !request.url.startsWith(origin)) return
    const route = routes[request.url.slice(origin.length)]
    if (route === undefined) return { raw: "not found", status: 404, headers: { "content-type": "text/plain" } }
    return typeof route === "function" ? route(request) : route
  }
}

const page = (body: string, contentType = "text/html; charset=utf-8", status = 200): Turn => ({
  raw: body,
  status,
  headers: { "content-type": contentType },
})

const fetches = (...urls: (string | Record<string, unknown>)[]): Turn[] => [
  ...urls.map((url) => call("webfetch", typeof url === "string" ? { url } : url)),
  say("Fetched."),
]

async function run(ctx: Ctx, text: string) {
  const id = await ctx.session()
  await ctx.prompt(id, text)
  return id
}

const article = `<!DOCTYPE html>
<html lang="en">
<head>
  <title>Ignored title</title>
  <meta charset="utf-8">
  <link rel="stylesheet" href="/style.css">
  <style>body { color: red }</style>
  <script>window.tracking = true</script>
</head>
<body>
  <nav aria-hidden="true"><a href="/hidden">Hidden nav</a></nav>
  <header><h1>Release   notes &amp; more</h1></header>
  <main>
    <h2>What's <em>new</em></h2>
    <p>Version <strong>2.0</strong> ships <b>bold</b>, <i>italic</i>, <s>struck</s>, <del>deleted</del> and <strike>old</strike> text.
       Special characters: 5 * 3 = 15, snake_case, [brackets], a|b, x &lt; y &gt; z, ~tilde~, back\\slash, \`tick\`.</p>
    <p># not a heading<br>+ not a list<br>- not a list<br>1. not ordered</p>
    <p>Entities: &copy; &nbsp;non-breaking&nbsp; &#x1F600; &#169; &eacute;</p>
    <p>Links: <a href="https://example.com/docs" title="The &quot;docs&quot;">documentation</a>,
       <a href="/relative path(1)">relative</a>, <a>no href</a>, <a href="https://example.com/x"><code>code link</code></a>,
       <a href="https://example.com/outer">outer <a href="https://example.com/inner">inner</a> tail</a>.</p>
    <p>Images: <img src="/logo.png" alt="Logo [v2]" title="Our logo"> and <img src="/blank.gif"></p>
    <p>Inline code: <code>const x = 1</code>, <code>a \`tick\` inside</code>, <code>\`edge\`</code>, <code></code>, <code>  </code>.</p>
    <p>Empty markers: <b></b><em> </em>done. Marker across block: <strong>start<p>inside</p>end</strong></p>
    <pre><code class="language-ts">function add(a: number, b: number) {
  return a + b
}
</code></pre>
    <pre>line one<br>line two with \`\`\` fences</pre>
    <pre><code class="lang-sh">echo ~~~ \`\`\`</code></pre>
    <pre><a href="https://example.com/in-pre">link in pre</a> text</pre>
    <p><a href="https://example.com/around">before <pre>preformatted inside link</pre> after</a></p>
    <blockquote>
      <p>Quoted <em>text</em>.</p>
      <blockquote><p>Nested quote.</p><pre>code in quote</pre></blockquote>
      <ul><li>quoted item</li></ul>
    </blockquote>
    <hr>
    <ul>
      <li>First item</li>
      <li>Second item
        <ul><li>Nested <a href="https://example.com/n">link</a></li><li>Nested two
          <ol start="5"><li>five</li><li value="9">nine</li><li>ten</li></ol></li></ul>
      </li>
      <li><p>Paragraph item</p></li>
    </ul>
    <ol start="x"><li>bad start</li></ol>
    <ol><li>one</li><li>two</li></ol>
    <table>
      <caption>Prices <em>2026</em></caption>
      <thead><tr><th>Plan</th><th>Price | USD</th></tr></thead>
      <tbody><tr><td>Free</td><td>$0</td></tr><tr><td><a href="/pro">Pro</a></td><td><code>$10</code></td></tr></tbody>
    </table>
    <table><tr><td colspan="2">Spanning cell</td></tr><tr><td>a</td><td>b</td></tr></table>
    <table><tr><td>ragged</td></tr><tr><td>x</td><td>y</td></tr></table>
    <table><tr><td>outer <table><tr><td>inner cell</td></tr></table></td><td>second</td></tr></table>
    <table></table>
    <table>loose text<tr><td>cell</td></tr></table>
    <details><summary>Closed summary</summary><p>Hidden details body.</p></details>
    <details open><summary>Open summary</summary><p>Visible details body.</p></details>
    <dl><dt>Term</dt><dd>Definition one</dd><dt>Other</dt><dd>Definition two</dd></dl>
    <div hidden>Hidden div</div>
    <span aria-hidden="TRUE">aria hidden</span>
    <noscript>Enable JavaScript</noscript>
    <iframe src="/frame">frame</iframe>
    <template><p>template</p></template>
    <object data="x"></object><embed src="y">
    <section><article><aside>Aside text</aside><figure><figcaption>Figure caption</figcaption></figure></article></section>
    <address>Address line</address><form><fieldset>Form text</fieldset></form><dialog>Dialog text</dialog>
    <footer>Footer &mdash; end</footer>
  </main>
</body>
</html>`

scenario(
  "webfetch rich HTML as markdown",
  {
    responders: [site("https://docs.example.com", { "/release": page(article) })],
    turns: fetches("https://docs.example.com/release#section-2"),
  },
  async (ctx) => {
    const id = await run(ctx, "Summarize https://docs.example.com/release")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "webfetch HTML as text and as raw HTML",
  {
    responders: [site("https://docs.example.com", { "/release": page(article) })],
    turns: fetches(
      { url: "https://docs.example.com/release", format: "text" },
      { url: "https://docs.example.com/release", format: "html" },
    ),
  },
  async (ctx) => {
    await run(ctx, "Fetch the release notes as text and as HTML")
  },
)

const quirks = [
  // Inline code that contains a link, then a link that contains inline code.
  `<p><code>call <a href="https://a.example/x">x</a> now</code> and <a href="https://a.example/y"><code>y()</code> docs</a></p>`,
  `<p><a href="https://a.example/z"><code>z <a href="https://a.example/w">w</a></code></a> after</p>`,
  `<p><code>pre<a href="https://a.example/1">one</a>mid<a href="https://a.example/2">two</a>post</code></p>`,
  `<p><code>c <a href="https://a.example/n1">one <a href="https://a.example/n2">two</a> rest</a> end</code></p>`,
  `<p><code>d <a href="https://a.example/m1">one</code> out</a> after</p>`,
  `<p><a href="https://a.example/o1"><code>e <a href="https://a.example/o2">two</a></code> tail</a></p>`,
  `<p><code>f <a href="https://a.example/p1">one<p>para</p>two</a> y</code></p>`,
  // Links across blocks, lists and tables.
  `<a href="https://a.example/block"><div>block in link</div><ul><li>item in link</li></ul><blockquote>quote in link</blockquote><table><tr><td>t</td></tr></table></a>`,
  `<p><a href="https://a.example/em"><em>emphasis</em></a> <em><a href="https://a.example/em2">inside</a></em></p>`,
  `<p><b>bold <i>both</i></b> <em>a</em><em>b</em> x<strong> spaced </strong>y</p>`,
  `<ul><li><b></b>marker first</li><li> <em>leading</em> space</li></ul>`,
  `<blockquote><ol><li>quoted ordered</li></ol><table><tr><th>h</th></tr><tr><td>v</td></tr></table><br>after break</blockquote>`,
  `<ul><li><table><tr><th>in</th><th>list</th></tr><tr><td>1</td><td>2</td></tr></table></li></ul>`,
  `<ol><li><ol><li><ol><li><ol><li><ol><li><ol><li><ol><li>deep</li></ol></li></ol></li></ol></li></ol></li></ol></li></ol></li></ol>`,
  `<p>text<pre>   indented code
	with tab</pre>tail</p><pre></pre><code>   </code>`,
  `<table><tr><td><pre>code in cell</pre></td><td>a<br>b</td></tr></table>`,
  `<details><summary>S</summary><details open><summary>inner</summary>shown?</details></details>`,
  `<h3>Heading with <a href="https://a.example/h">link</a></h3><h6>Six</h6>`,
  `<p>Trailing spaces   <br>   after</p><p>a&#9;tab</p>`,
  `<p><img alt="a\\b]c" src="x y.png"></p><p><a href="https://a.example/t" title="  spaced
   title  ">t</a></p>`,
].join("\n")

scenario(
  "webfetch HTML conversion edge cases",
  {
    responders: [site("https://edge.example.com", { "/quirks": page(quirks, "text/html") })],
    turns: fetches("https://edge.example.com/quirks"),
  },
  async (ctx) => {
    await run(ctx, "Fetch the quirks page")
  },
)

// Pages that end inside a tag or a character reference. One is served as `Text/HTML`: media types are
// case-insensitive.
scenario(
  "webfetch markup cut off at the end of a page",
  {
    responders: [
      site("https://cut.example.com", {
        "/closing": page("<p>kept</p></div foo", "Text/HTML"),
        "/emoji": page("a</b \u{1f600}"),
        "/self-closing": page("<p>kept</p><div/"),
        "/script": page("<p>kept</p>x<s"),
        "/title": page("<p>kept</p>x<t"),
        "/reference": page("<p>hello world &#x"),
        "/list": page("<ul><li>a<b "),
        "/emphasis": page("<p>a<em "),
      }),
    ],
    turns: fetches(
      "https://cut.example.com/closing",
      { url: "https://cut.example.com/closing", format: "text" },
      { url: "https://cut.example.com/emoji", format: "text" },
      "https://cut.example.com/self-closing",
      "https://cut.example.com/script",
      "https://cut.example.com/title",
      { url: "https://cut.example.com/reference", format: "text" },
      "https://cut.example.com/list",
      "https://cut.example.com/emphasis",
    ),
  },
  async (ctx) => {
    await run(ctx, "Fetch the cut-off pages")
  },
)

scenario(
  "webfetch deeply nested HTML",
  {
    responders: [
      site("https://deep.example.com", {
        "/nested": page(
          `<p>start</p>${"<div><span>".repeat(5_001)}deep text<script>hidden()</script><em>x</em>${"</span></div>".repeat(5_001)}<p>end</p>`,
        ),
        "/suppressed": page(
          `<p>visible</p><div hidden>${"<div>".repeat(10_001)}hidden deep${"</div>".repeat(10_001)}</div><p>after hidden</p>`,
        ),
      }),
    ],
    turns: fetches("https://deep.example.com/nested", "https://deep.example.com/suppressed"),
  },
  async (ctx) => {
    await run(ctx, "Fetch nested pages")
  },
)

// The markdown output is capped just under 5 MiB: escaped text fills the budget exactly, so the inline code,
// fenced code and image that follow must be trimmed to fit (or dropped).
const budget = 5 * 1024 * 1024 - 64 * 1024
const filler = (budget - 6 - 6) / 2
const savedOutput = (request: ProviderRequest) => {
  const messages = (request.body as { messages: { role: string; content: unknown }[] }).messages
  const last = String(messages.at(-1)?.content ?? "")
  const saved = last.match(/full output saved to (\S+?)\]/)?.[1]
  if (!saved) throw new Error(`no saved output in: ${last.slice(0, 200)}`)
  return call("read", { path: saved, limit: 8 })
}

/** Answers chat requests in order from steps that may depend on the request. */
function chat(...steps: (Turn | ((request: ProviderRequest) => Turn))[]): Responder {
  let index = 0
  return (request) => {
    if (request.kind !== "chat") return
    const step = steps[index++]
    if (step === undefined) return { status: 500, body: { error: { message: "conformance: script exhausted" } } }
    return typeof step === "function" ? step(request) : step
  }
}

scenario(
  "webfetch HTML that exceeds the markdown budget",
  {
    responders: [
      chat(
        call("webfetch", { url: "https://huge.example.com/page" }),
        savedOutput,
        call("webfetch", { url: "https://huge.example.com/multibyte" }),
        savedOutput,
        call("webfetch", { url: "https://huge.example.com/tight" }),
        savedOutput,
        say("Fetched."),
      ),
      site("https://huge.example.com", {
        "/page": page(
          `<p>${"*".repeat(filler)}</p><p><code> a </code></p><pre><code>fenced code after the limit</code></pre><p><img alt="alt \u00e9t\u00e9" src="x.png"></p><p>tail \u00e9</p>`,
        ),
        "/multibyte": page(`<p>${"\u00e9".repeat(budget / 2 + 10)}</p>`),
        // One byte of budget left for inline code that needs a two-backtick fence and padding: nothing fits.
        "/tight": page(`<p>x${"*".repeat((budget - 12) / 2)}</p><p><code>\`a</code></p>`),
      }),
    ],
    // Each result is too large for the model and is saved to a file; read the saved conversion back (long lines
    // are clipped by read, the short trailing lines show what was trimmed).
  },
  async (ctx) => {
    await run(ctx, "Fetch the huge page")
  },
)

scenario(
  "webfetch non-HTML content types",
  {
    responders: [
      site("https://data.example.com", {
        "/notes.txt": page("Plain <b>text</b> stays as is.\n", "text/plain"),
        "/readme.md": page("# Title\n\n- item\n", "text/markdown; charset=utf-8"),
        "/data.json": page('{"ok":true,"items":[1,2,3]}', "application/json"),
        "/feed.xml": page("<feed><title>t</title></feed>", "application/atom+xml"),
        "/script.js": page("console.log(1)\n", "application/javascript"),
        "/logo.svg": page('<svg xmlns="http://www.w3.org/2000/svg"></svg>', "image/svg+xml"),
        "/image.png": page("\x89PNG", "image/png"),
        "/archive.zip": page("PK", "application/zip"),
        "/untyped": { raw: "no content type", status: 200, headers: { "content-type": "" } },
        "/empty": page("", "text/plain"),
      }),
    ],
    turns: fetches(
      "https://data.example.com/notes.txt",
      { url: "https://data.example.com/notes.txt", format: "text" },
      "https://data.example.com/readme.md",
      "https://data.example.com/data.json",
      "https://data.example.com/feed.xml",
      "https://data.example.com/script.js",
      "https://data.example.com/logo.svg",
      "https://data.example.com/image.png",
      "https://data.example.com/archive.zip",
      "https://data.example.com/untyped",
      "https://data.example.com/empty",
    ),
  },
  async (ctx) => {
    await run(ctx, "Fetch the data files")
  },
)

scenario(
  "webfetch HTTP errors and invalid URLs",
  {
    responders: [
      site("https://errors.example.com", {
        "/gone": page("<h1>Not here</h1>", "text/html", 404),
        "/boom": page("internal", "text/plain", 500),
        "/teapot": page("short and stout", "text/plain", 418),
      }),
    ],
    turns: fetches(
      "https://errors.example.com/gone",
      "https://errors.example.com/boom",
      "https://errors.example.com/teapot",
      "not a url",
      "ftp://files.example.com/file.txt",
      "file:///etc/passwd",
      "https://user:secret@errors.example.com/private",
      "http://unanswered.example.com/",
      { url: "https://errors.example.com/gone", format: "pdf" },
      { url: "https://errors.example.com/gone", timeout: 500 },
    ),
  },
  async (ctx) => {
    await run(ctx, "Fetch broken URLs")
  },
)

const redirect = (location: string, status = 302): Turn => ({ raw: "", status, headers: { location } })

scenario(
  "webfetch redirects",
  {
    responders: [
      site("https://old.example.com", {
        "/moved": redirect("https://new.example.com/landing#frag", 301),
        "/relative": redirect("/final", 307),
        "/final": page("<p>Final page</p>"),
        "/see-other": redirect("https://new.example.com/landing", 303),
        "/permanent": redirect("https://new.example.com/landing", 308),
        "/no-location": { raw: "", status: 302, headers: {} },
        "/to-ftp": redirect("ftp://old.example.com/file"),
        "/to-credentials": redirect("https://user:pw@new.example.com/landing"),
        ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`/loop/${i}`, redirect(`/loop/${i + 1}`)])),
      }),
      site("https://new.example.com", { "/landing": page("<h1>Landing</h1><p>Welcome.</p>") }),
    ],
    turns: fetches(
      "https://old.example.com/moved",
      "https://old.example.com/relative",
      "https://old.example.com/see-other",
      "https://old.example.com/permanent",
      "https://old.example.com/no-location",
      "https://old.example.com/to-ftp",
      "https://old.example.com/to-credentials",
      "https://old.example.com/loop/0",
    ),
  },
  async (ctx) => {
    await run(ctx, "Follow the redirects")
  },
)

scenario(
  "webfetch Cloudflare challenge retried with another user agent",
  {
    responders: [
      site("https://cf.example.com", {
        "/page": (() => {
          let attempts = 0
          return () =>
            attempts++ === 0
              ? { raw: "challenge", status: 403, headers: { "content-type": "text/html", "cf-mitigated": "challenge" } }
              : page("<p>Passed the challenge.</p>")
        })(),
        "/blocked": { raw: "forbidden", status: 403, headers: { "content-type": "text/plain" } },
      }),
    ],
    turns: fetches("https://cf.example.com/page", "https://cf.example.com/blocked"),
  },
  async (ctx) => {
    await run(ctx, "Fetch behind Cloudflare")
  },
)

scenario(
  "webfetch response size limits",
  {
    responders: [
      site("https://big.example.com", {
        "/declared": { raw: "small body", status: 200, headers: { "content-type": "text/plain", "content-length": "99999999" } },
        "/streamed": page("x".repeat(5 * 1024 * 1024 + 10), "text/plain"),
        "/long": page(Array.from({ length: 3000 }, (_, i) => `line ${i + 1}`).join("\n"), "text/plain"),
      }),
    ],
    turns: fetches("https://big.example.com/declared", "https://big.example.com/streamed", "https://big.example.com/long"),
  },
  async (ctx) => {
    await run(ctx, "Fetch big responses")
  },
)

{
  let release!: () => void
  const stalled = new Promise<void>((resolve) => (release = resolve))
  scenario(
    "webfetch timeout",
    {
      responders: [
        site("https://slow.example.com", {
          "/stall": { events: [{ text: "partial" }, { until: stalled }, { text: "rest" }] },
        }),
      ],
      turns: fetches({ url: "https://slow.example.com/stall", timeout: 0.2 }),
    },
    async (ctx) => {
      await run(ctx, "Fetch the slow page")
      release()
    },
  )
}

const permissions = (...rules: Record<string, string>[]) => ({
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), ...rules],
})

scenario(
  "webfetch denied by permission rules",
  {
    config: permissions(
      { action: "webfetch", resource: "https://blocked.example.com/*", effect: "deny" },
      { action: "webfetch", resource: "https://evil.example.com/*", effect: "deny" },
    ),
    responders: [
      site("https://ok.example.com", { "/hop": redirect("https://evil.example.com/payload"), "/fine": page("fine", "text/plain") }),
      site("https://evil.example.com", { "/payload": page("evil", "text/plain") }),
    ],
    turns: fetches("https://blocked.example.com/page", "https://ok.example.com/hop", "https://ok.example.com/fine"),
  },
  async (ctx) => {
    await run(ctx, "Fetch blocked pages")
  },
)

scenario(
  "webfetch asked by permission and answered",
  {
    config: permissions({ action: "webfetch", resource: "*", effect: "ask" }),
    responders: [site("https://ask.example.com", { "/a": page("first", "text/plain"), "/b": page("second", "text/plain") })],
    turns: fetches("https://ask.example.com/a", "https://ask.example.com/b", "https://ask.example.com/a"),
  },
  async (ctx) => {
    const id = await ctx.session()
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Fetch with approval" })
    const decisions = ["always", "reject"]
    for (let i = 0; i < 4; i++) {
      const state = await ctx.settle(id, from)
      if (state !== "permission.asked") break
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      from = ctx.events.length
      await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, {
        decision: decisions.shift() ?? "once",
      })
    }
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
