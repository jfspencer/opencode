// Runs before any test module loads (see bunfig.toml). opencode resolves its global directories from the
// environment once, at import time, so isolation has to happen here: the oracle must never read the user's
// real config or write into their real data directories.
import { afterAll } from "bun:test"
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

for (const key of Object.keys(process.env)) {
  if (/^(OPENCODE_|OPENROUTER_|ANTHROPIC_|OPENAI_|GEMINI_|GOOGLE_|AWS_|AZURE_|GITHUB_TOKEN|GH_TOKEN|XDG_|AI_AGENT|CLAUDE|COLORTERM|TERM_PROGRAM|VTE_|KITTY_|WEZTERM_|TMUX)/.test(key))
    delete process.env[key]
}

const realHome = process.env.HOME
const root = realpathSync(mkdtempSync(path.join(tmpdir(), "oc-conformance-")))
const dir = (name: string) => {
  const target = path.join(root, name)
  mkdirSync(target, { recursive: true })
  return target
}

process.env.CONFORMANCE_ROOT = root
// The sandbox holds every scenario's project, git repositories and caches: remove it when the test process exits,
// or /tmp (often a tmpfs with a fixed inode count) fills up. CONFORMANCE_KEEP_SANDBOX=1 keeps it for debugging.
// (A global afterAll registered here runs once after every test file; bun's test runner skips "exit" listeners.)
if (!process.env.CONFORMANCE_KEEP_SANDBOX)
  afterAll(() => {
    try {
      process.chdir(tmpdir())
      rmSync(root, { recursive: true, force: true })
    } catch {}
  })
// File search needs ripgrep. CI has it on PATH; a macOS development machine may only have the copy opencode
// already cached under the real home. The pinned 15.1.0 binary is the same one a download would install, and the
// fake network cannot reach GitHub, so expose the host's copy when PATH has none.
if (!Bun.which("rg") && realHome) {
  const cached = path.join(realHome, ".cache/opencode/bin/rg")
  if (existsSync(cached)) process.env.PATH = `${path.dirname(cached)}:${process.env.PATH}`
}
process.env.HOME = dir("home")
process.env.OPENCODE_TEST_HOME = process.env.HOME
process.env.XDG_CONFIG_HOME = dir("config")
process.env.XDG_DATA_HOME = dir("data")
process.env.XDG_STATE_HOME = dir("state")
process.env.XDG_CACHE_HOME = dir("cache")
process.env.TMPDIR = dir("tmp")
process.env.OPENCODE_DB = ":memory:"
process.env.OPENROUTER_API_KEY = "sk-or-conformance"
process.env.TZ = "UTC"
// Fixed terminal and shell identity (they reach tool output and built-in skill text).
process.env.TERM = "xterm-256color"
process.env.SHELL = "/bin/bash"
process.env.LANG = "C.UTF-8"
process.env.GIT_CONFIG_NOSYSTEM = "1"
process.env.GIT_AUTHOR_NAME = process.env.GIT_COMMITTER_NAME = "Conformance"
process.env.GIT_AUTHOR_EMAIL = process.env.GIT_COMMITTER_EMAIL = "conformance@example.invalid"
process.env.GIT_AUTHOR_DATE = process.env.GIT_COMMITTER_DATE = "2026-01-01T00:00:00Z"
// Persistent terminals run in the `opencode-pty` daemon, which a server starts on first use and owns. Every
// implementation runs the reference's own build of it (OPENCODE_PTY_BIN, which the reference reads first), with its
// runtime directory in the sandbox state (XDG_STATE_HOME) and its socket in the sandbox, not the user's
// /tmp/opencode-pty-<uid> where their real service's daemons listen. macOS's sandbox root is longer than a Unix
// socket path allows (`sun_path` is 104 bytes), so there the daemon's control socket directory is a short one under
// /tmp: the runtime directory (the records) stays in the sandbox.
const pty = await import(Bun.resolveSync("@opencode/pty", path.join(import.meta.dir, "../ref/packages/core")))
const ptySockets = process.platform === "darwin" ? mkdtempSync(path.join("/tmp", "opencode-pty-")) : dir("pty")
process.env.OPENCODE_PTY_BIN = pty.binaryPath as string
process.env.OPENCODE_PTY_SOCKET_DIR = ptySockets
if (!process.env.CONFORMANCE_KEEP_SANDBOX && ptySockets !== path.join(root, "pty"))
  afterAll(() => {
    try {
      rmSync(ptySockets, { recursive: true, force: true })
    } catch {}
  })

// The server also boots an instance at its own working directory. Keep it inside the sandbox: from the repo
// it would load the repo's own .opencode config (whose references trigger git clones over the network).
process.chdir(dir("cwd"))

// Some code paths call globalThis.fetch directly instead of the server's injected fetch (e.g. code-mode
// `fetch`, core/src/codemode/fetch.ts). Loopback stays allowed (the TUI and CLI drivers talk to local servers);
// everything else goes to the running scenario's fake network, or fails. Nothing may reach the real network.
const realFetch = globalThis.fetch
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]", "::1"])
globalThis.fetch = Object.assign(
  (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input))
    // data: and blob: URLs never touch the network.
    if (url.protocol === "data:" || url.protocol === "blob:" || LOOPBACK.has(url.hostname)) return realFetch(input, init)
    const route = (globalThis as { __conformanceFetch?: typeof realFetch }).__conformanceFetch
    if (route) return route(input, init)
    return Promise.reject(new Error(`conformance sandbox: network access to ${url.host} is blocked`))
  },
  { preconnect: realFetch.preconnect },
) as typeof fetch
