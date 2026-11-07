// The permission requests the shell tool makes for a command: which commands the scanner finds in it
// (`resources`, matched against `shell` rules) and the patterns an "always" reply would save (`save`). Every command
// asks; the scenario records the request and declines it with feedback, so nothing runs and the model moves on to
// the next command. To keep recordings small, each call is a short script of a few related command lines; its
// request lists the resources of every line in order. Commands that leave the project ask for external directory access first, which is granted
// once so the shell request that follows is recorded too. Commands the scanner refuses, and commands that hold no
// command at all, are covered by shell-scan.test.ts.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
const only = [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")]
// A one-line system prompt keeps the recorded provider requests small; the default prompt is covered elsewhere.
const agents = { build: { system: "Conformance agent." } }

const files = { "README.md": "# Demo\n", "a.txt": "alpha\n", "b.txt": "beta\n", "src/main.ts": "export {}\n" }

async function classify(ctx: Ctx, id: string, text: string) {
  let from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  let state = await ctx.settle(id, from)
  while (state === "permission.asked") {
    const asked = ctx.events.filter((event: any) => event?.type === "permission.asked" && event.data.sessionID === id).at(-1) as any
    from = ctx.events.length
    const decision = asked.data.action === "shell" ? { decision: "reject", message: "Recorded." } : { decision: "once" }
    await ctx.http("POST", `/api/session/${id}/permission/${asked.data.id}/reply`, decision)
    state = await ctx.settle(id, from)
  }
}

/** One shell call per group of up to four command lines. */
function probe(name: string, lines: readonly string[], shell?: string) {
  const commands = Array.from({ length: Math.ceil(lines.length / 4) }, (_, index) => lines.slice(index * 4, index * 4 + 4).join("\n"))
  scenario(
    name,
    {
      files,
      config: {
        agents,
        ...(shell ? { shell } : {}),
        permissions: [...only, rule("shell", "*", "ask"), rule("external_directory", "*", "ask")],
      },
      turns: [...commands.map((command) => call("shell", { command })), say("Done.")],
    },
    async (ctx) => {
      const id = await ctx.session({ title: name })
      await classify(ctx, id, `Classify the ${name} commands.`)
    },
  )
}

probe("lists and pipelines", [
  "git status && git diff --stat",
  "make build || echo failed",
  "cat README.md | grep Demo | wc -l",
  "git log --oneline |& head -n 3",
  "sleep 1 & echo started",
  "echo one\necho two\n\necho three",
  "! grep -q missing README.md",
  "git status &&\n  git diff",
  "echo a\\\n  b",
  "time -p git status",
])

probe("quoting and escaping", [
  "echo 'single quoted; rm -rf /'",
  "printf '%s\\n' 'a b' \"c d\" e\\ f",
  "echo $'ansi\\tquoted' $\"locale\"",
  "echo \"mixed\"'quotes'unquoted",
  "grep -r 'TODO\\|FIXME' src",
  "echo \\$HOME \\`date\\`",
  "git  commit   -m  'spaced   message'",
  "\tgit\tstatus\t",
])

probe("substitutions and expansions", [
  'echo "$(git rev-parse HEAD)"',
  "echo `git rev-parse HEAD`",
  "echo $(basename $(pwd))",
  "diff <(sort a.txt) <(sort b.txt)",
  "echo $((1 + 2 * 3)) ${FILE%.txt} ${x:=default}",
  "echo $(cat <<EOF\ninside\nEOF\n)",
  "$(which git) status",
  "echo \"${arr[@]}\" $HOME",
])

probe("assignments and declarations", [
  "FOO=bar env",
  "FOO=$(git rev-parse HEAD) make build",
  "export A=1 B=$(hostname)",
  'arr=(a b "$(whoami)")',
  "read -r line < a.txt",
  "printf -v out '%s' x",
  'eval "echo hi"',
  "local v=$(date +%s)",
])

probe("redirections", [
  "echo hi > out.txt",
  "make 2>&1 | tee build.log",
  "sort < a.txt > sorted.txt",
  "> empty.txt",
  ">> log.txt echo appended",
  "2>\terr.txt",
  "echo a 2>/dev/null b",
  "{ echo a; echo b; } > both.txt",
  "echo > $(echo name).txt",
  'grep x <<< "$text"',
])

probe("heredocs", [
  "cat <<EOF\nhello $USER\nEOF",
  "cat <<-EOF\n\tindented\n\tEOF",
  "cat <<'EOF'\nliteral $(not run)\nEOF",
  "cat <<EOF\n$(git status)\nEOF",
  "cat <<A <<B\na\nA\nb\nB",
  "cat <<EOF | grep x\nx\nEOF",
  "cat <<EOF; echo after\nbody\nEOF",
  "cat << EOF > out.txt\nspaced\nEOF",
  "<<EOF cat\nbody\nEOF",
])

probe("compound commands", [
  "if [ -f a.txt ]; then cat a.txt; else echo none; fi",
  'for f in *.txt; do wc -l "$f"; done',
  'while read -r line; do echo "$line"; done < a.txt',
  'case "$1" in start) npm start;; *) echo usage;; esac',
  "( cd src && ls )",
  "greet() { echo hi; }; greet",
  '[[ -f a.txt && -n "$x" ]] && echo yes',
  "(( count > 1 )) && echo many",
  "coproc worker { cat; }",
  "time { sleep 1; }",
])

probe("wrappers", [
  "builtin echo x",
  "command -v git",
  "exec ls",
  "env FOO=1 npm test",
  "sudo rm -rf /var/cache",
  "sh -c 'rm -rf build'",
  "nohup npm start",
  "xargs rm < files.txt",
])

probe("directory changes", [
  "cd src && ls",
  "cd -",
  "cd",
  "pushd src && popd",
  "cd ~+/src && ls",
  "cd {a,b}",
  "cd $(git rev-parse --show-toplevel)",
  "cd .. && ls",
  "cd / && ls -la",
  "builtin cd -L src && command cd -- src && ls",
  "command -v cd",
])

probe("command prefixes", [
  "ls *.txt",
  "rm -rf build/*",
  "npm run build -- --watch",
  "npm install lodash",
  "git config user.name x",
  "git -C src status",
  "docker compose up -d",
  "kubectl get pods -A",
  "bun x tsc --noEmit",
  "python -m http.server",
  "aws s3 ls s3://bucket",
  "gh pr list",
  "unknown-tool arg1 arg2",
  "./script.sh --flag",
])

probe("command names shared with object properties", ["toString x", "constructor y", "hasOwnProperty z"])

probe("wildcard characters in commands", ["r? notes.txt", "sh -c 'rm -f *.log'", "FOO=1 ls *.txt", "git '*'"])

probe("posix sh", ["make &> build.log", "echo $[1 + 2]", "git status && git diff | head -5", "cat <<EOF\n$(git status)\nEOF"], "sh")

// PowerShell is not installed where the reference runs, but the scanner picks its dialect from the configured
// shell's name; a stand-in executable named pwsh makes the configuration valid. Nothing runs: every command asks
// and is declined.
const fakeShells = path.join(process.env.CONFORMANCE_ROOT!, "fake-shells")
mkdirSync(fakeShells, { recursive: true })
writeFileSync(path.join(fakeShells, "pwsh"), "#!/bin/sh\n# Stand-in for a shell that is not installed.\nexit 0\n", { mode: 0o755 })
const pwsh = path.join(fakeShells, "pwsh")

probe(
  "powershell commands",
  [
    "Get-ChildItem -Recurse | Where-Object { $_.Length -gt 1kb } | Select-Object Name",
    "git status; git diff",
    "npm test && npm run build",
    "Write-Output 'hello' > out.txt",
    "& 'C:\\Program Files\\tool.exe' --flag",
    ". ./script.ps1",
    'git commit -m "msg with `"quotes`""',
    "git log --oneline --% -n 5 | Select-Object -First 2",
    "Get-Item x -ErrorAction:SilentlyContinue",
    "Copy-Item -Path:a.txt -Destination b.txt",
    "Get-Item\tx",
    "Write-Output(1)",
    "Get-Item `\n  x",
    "npm  run   build",
  ],
  pwsh,
)

probe(
  "powershell expressions and blocks",
  [
    "$x = Get-Date; Write-Output $x",
    "[System.IO.File]::Delete('x')",
    "$p = Get-Process -Id 1; $p.Kill()",
    'Write-Output "value: $(git rev-parse HEAD)"',
    "foreach ($f in Get-ChildItem) { Write-Output $f.Name }",
    "try { git pull } catch { Write-Error $_ } finally { git status }",
    "function Get-Thing { param($n) Write-Output $n }; Get-Thing 1",
    "Set-Location src; Get-ChildItem",
    "cd ..; ls",
    "@\"\nhere $(Get-Date)\n\"@ | Out-File a.txt",
  ],
  pwsh,
)
