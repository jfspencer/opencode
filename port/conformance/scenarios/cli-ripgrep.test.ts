// Without ripgrep on PATH or in opencode's cache (`<XDG_CACHE_HOME>/opencode/bin/rg`), the server downloads the pinned
// ripgrep release from GitHub (through the fake network) and runs it only when the archive matches its pinned SHA-256.
// The scenarios run the CLI with a PATH that has no ripgrep, and a model turn that needs it (glob). The fake network
// cannot serve the real release, so they cover what happens when the download is unanswered or fails verification:
// the request, the tool's error, and an empty cache.
import { cliScenario } from "../lib/cli"
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"

const allowed = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" }))],
}
// The system directories only: no ~/.cargo/bin or other place a developer keeps rg.
const noRipgrep = { PATH: "/usr/bin:/bin" }
const release = (url: string) => url.startsWith("https://github.com/BurntSushi/ripgrep/releases/download/")

cliScenario(
  "an unanswered ripgrep download fails the search",
  {
    env: noRipgrep,
    config: allowed,
    files: { "a.txt": "a\n" },
    turns: [call("glob", { pattern: "*.txt" }), say("No ripgrep."), call("glob", { pattern: "*.md" }), say("Still none.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "List the text files"])
    // The failed download is remembered: the next search does not retry it.
    await ctx.cli(["run", "List the markdown files"])
    ctx.file("cache/opencode/bin/rg")
  },
)

cliScenario(
  "a ripgrep download that fails verification is not used",
  {
    env: noRipgrep,
    config: allowed,
    files: { "a.txt": "a\n" },
    turns: [call("glob", { pattern: "*.txt" }), say("No ripgrep.")],
    responders: [(request) => (release(request.url) ? { raw: "not a ripgrep release\n" } : undefined)],
  },
  async (ctx) => {
    await ctx.cli(["run", "List the text files"])
    ctx.file("cache/opencode/bin/rg")
  },
)
