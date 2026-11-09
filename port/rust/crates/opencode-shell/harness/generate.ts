// Random inputs for the differential harness: shell command lines built from a grammar of the constructs the
// scanner reads (words, quoting, escapes, expansions, substitutions, arithmetic, heredocs, redirects, lists,
// compound commands, functions, comments), then mutated (truncated, spliced, sprinkled with odd characters) so
// unterminated and malformed constructs are as common as well-formed ones. Deterministic for a seed.

export type Random = () => number

export function random(seed: number): Random {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

export const pick = <T>(rand: Random, items: readonly T[]): T => items[Math.floor(rand() * items.length)]!
const chance = (rand: Random, p: number) => rand() < p
const int = (rand: Random, max: number) => Math.floor(rand() * max)

// Characters that change how a shell or the scanner reads text, plus Unicode the regexes treat specially
// (astral letters, the folding letters U+017F and U+212A, U+0345, line and paragraph separators, JS whitespace).
const ODD = [
  "\\", "\\\n", "$", "`", "'", '"', "(", ")", "{", "}", "[", "]", ";", "&", "|", "<", ">", "#", "~", "=", "*", "?",
  "!", "@", "%", "^", "+", "-", ",", ".", ":", "/", " ", "\t", "\n", "\r", "\r\n", "\v", "\f", "\u00a0", "\u3000",
  "\u2028", "\u0085", "\ufeff", "\u2013", "\u201c", "é", "ſ", "\u212a", "\u0345", "𝐀", "😀", "中", "\0", "\x1b",
]

const NAMES = [
  "git", "ls", "cat", "echo", "printf", "print", "rm", "cp", "mv", "grep", "make", "npm", "bun", "cargo", "docker",
  "kubectl", "gh", "aws", "python", "node", "sed", "awk", "find", "xargs", "sudo", "doas", "env", "nice", "nohup",
  "timeout", "time", "exec", "command", "builtin", "coproc", "noglob", "nocorrect", "eval", "source", ".", "trap",
  "export", "declare", "typeset", "local", "readonly", "private", "unset", "unsetenv", "read", "mapfile", "readarray",
  "getopts", "vared", "zstyle", "zparseopts", "compgen", "set", "setopt", "unsetopt", "emulate", "alias", "let",
  "integer", "float", "shift", "return", "exit", "wait", "test", "[", "cd", "chdir", "pushd", "popd", "bash", "sh",
  "zsh", "dash", "toString", "constructor", "hasOwnProperty", "__proto__", "/usr/bin/sudo", "./run.sh", "/bin/rm",
  "npx", "pnpm", "yarn", "terraform", "openssl", "ip", "go", "tmux", "systemctl", "redis-cli",
]
const SUBCOMMANDS = [
  "status", "diff", "log", "run", "x", "add", "install", "test", "build", "compose", "container", "image", "config",
  "remote", "stash", "exec", "init", "view", "dlx", "kv", "auth", "rollout", "workspace", "req", "addr", "route",
  "push", "origin", "main", "--", "-C", "-n", "--watch", "-rf", "-p", "-v", "-u", "root",
]
const VARIABLES = ["x", "HOME", "PATH", "PWD", "USER", "i", "arr", "PS4", "NULLCMD", "aliases", "functions", "options", "_", "a1"]
const KEYWORDS = ["if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case", "esac", "select", "function", "repeat", "{", "}", "!", "[[", "]]", "((", "))"]
const USERS = ["root", "daemon", "nobody", "thinkpad", "nosuchuser", "_x", "a.b-c"]

export function shellWord(rand: Random, depth = 0): string {
  const n = rand()
  if (n < 0.25) return pick(rand, SUBCOMMANDS)
  if (n < 0.32) return pick(rand, ["foo", "bar", "a.txt", "src/main.ts", "*.txt", ".*", "a?b", "[ab]c", "{a,b}", "{1..3}", ".git/*", "a,b", "x..y"])
  if (n < 0.38) return pick(rand, ["~", "~/", "~/src", "~+", "~+/x", "~-", "~2", "~+3/a", `~${pick(rand, USERS)}`, `~${pick(rand, USERS)}/dir`, "..", "../a", "/tmp", "/", "-", "--", "-P", "-L", "./x y"])
  if (n < 0.46) return `${pick(rand, VARIABLES)}${pick(rand, ["=", "+=", "[1]=", "[$i]=", "=("])}${chance(rand, 0.7) ? shellWord(rand, depth + 1) : ""}${chance(rand, 0.1) ? ")" : ""}`
  if (n < 0.54) return `'${shellText(rand, depth + 1).replaceAll("'", "")}'`
  if (n < 0.62) return `"${shellText(rand, depth + 1)}"`
  if (n < 0.66) return `$'${pick(rand, ["a\\tb", "\\x41", "\\101", "\\u00e9", "\\U0001F600", "\\'", "\\e", "x\\", "\\u", "\\x", "\\0", "\\777", "\\ud800"])}'`
  if (n < 0.68) return `$"${shellText(rand, depth + 1)}"`
  if (n < 0.8) return expansion(rand, depth + 1)
  if (n < 0.86) return `${shellWord(rand, depth + 1)}${shellWord(rand, depth + 1)}`
  if (n < 0.9) return `\\${pick(rand, ODD)}${pick(rand, ["a", "", "b"])}`
  return pick(rand, ODD) + (chance(rand, 0.5) ? pick(rand, SUBCOMMANDS) : "")
}

function shellText(rand: Random, depth: number): string {
  const parts: string[] = []
  const count = int(rand, 4)
  for (let i = 0; i < count; i++) {
    parts.push(chance(rand, 0.4) && depth < 4 ? expansion(rand, depth + 1) : chance(rand, 0.3) ? pick(rand, ODD) : pick(rand, SUBCOMMANDS))
    if (chance(rand, 0.5)) parts.push(" ")
  }
  return parts.join("")
}

function expansion(rand: Random, depth: number): string {
  if (depth > 5) return `$${pick(rand, VARIABLES)}`
  const v = pick(rand, VARIABLES)
  switch (int(rand, 22)) {
    case 0: return `$${v}`
    case 1: return `\${${v}}`
    case 2: return `\${${v}${pick(rand, [":-", ":=", "=", ":?", ":+", "#", "##", "%", "%%", "/", "//", "^^", ",,", ":0:2", ": -3", "@P", "@Q", "[@]", "[$i]", "[1]"])}${chance(rand, 0.6) ? shellWord(rand, depth) : ""}}`
    case 3: return `\${#${v}}`
    case 4: return `\${!${v}${pick(rand, ["", "*", "@", "[@]"])}}`
    case 5: return `$(${list(rand, depth)})`
    case 6: return `\`${list(rand, depth).replaceAll("`", "")}\``
    case 7: return `$((${arith(rand, depth)}))`
    case 8: return `$[${arith(rand, depth)}]`
    case 9: return `${pick(rand, ["<", ">"])}(${list(rand, depth)})`
    case 10: return `\${ ${list(rand, depth)}; }`
    case 11: return `\${| ${list(rand, depth)}; }`
    case 12: return `$${pick(rand, ["$", "?", "#", "@", "*", "!", "-", "0", "1", "9"])}`
    case 13: return `\${(${pick(rand, ["P", "e", "f", "s:x:", "j:,:", "q+", "qq", "%", "@", "Pq", "s))", "z"])})${v}}`
    case 14: return `$${pick(rand, ["^", "=", "~", "^~", "+", "#", "##", "=~"])}${v}`
    case 15: return `$${v}[${pick(rand, ["1", "$i", "$(id)", "x", "1,2", "-1"])}]`
    case 16: return `$$'${pick(rand, ["x", "a\\nb", ""])}'`
    case 17: return `\${${v}:${arith(rand, depth)}}`
    case 18: return `$(<${shellWord(rand, depth)})`
    case 19: return `\${${pick(rand, ["(f)", "(P)", "^", "=", "~"])}${v}}`
    case 20: return `$(\\\n${list(rand, depth)})`
    default: return `\${${v}\\\n}`
  }
}

function arith(rand: Random, depth: number): string {
  const atoms = ["1", "x", "$x", "${x}", "x[1]", "x[$i]", "a+b", "2#101", "16#ff", "$#", "$$", "$$[1]", "'1'", '"$x"', "$(echo 1)", "i++", "--j", "x?1:2", "(1)", "1 << 2", "0x10"]
  const parts = [pick(rand, atoms)]
  for (let i = int(rand, 3); i > 0; i--) parts.push(pick(rand, [" + ", "*", " - ", " == ", ",", " && ", ";", "#"]), depth < 4 && chance(rand, 0.2) ? expansion(rand, depth + 1) : pick(rand, atoms))
  return parts.join("")
}

function simpleCommand(rand: Random, depth: number): string {
  const words: string[] = []
  if (chance(rand, 0.15)) words.push(`${pick(rand, VARIABLES)}=${shellWord(rand, depth)}`)
  words.push(chance(rand, 0.85) ? pick(rand, NAMES) : shellWord(rand, depth))
  for (let i = int(rand, 5); i > 0; i--) {
    if (chance(rand, 0.12)) words.push(redirect(rand, depth))
    else words.push(shellWord(rand, depth))
  }
  return words.join(pick(rand, [" ", " ", " ", "  ", "\t", " \\\n "]))
}

function redirect(rand: Random, depth: number): string {
  const op = pick(rand, [">", ">>", "<", "2>&1", ">&2", "&>", "&>>", "<<<", "<>", ">|", "2>", "3<&-", "{fd}>", "1>&-", ">&", "<&", "<(", "<<", "<<-"])
  if (op === "<<" || op === "<<-") {
    const delimiter = pick(rand, ["EOF", "'EOF'", '"EOF"', "\\EOF", "E\\OF", "$'EOF'", "-", "''", "$\"EOF\"", "EOF\\\n"])
    const bare = delimiter.replace(/['"\\$]/g, "")
    const body = chance(rand, 0.7) ? shellText(rand, depth + 1) : "$(id)"
    return `${op}${pick(rand, ["", " "])}${delimiter}\n${body}\n${pick(rand, ["", "\t"])}${chance(rand, 0.85) ? bare : "EOX"}\n`
  }
  if (op === "<(") return `<(${list(rand, depth + 1)})`
  return `${op}${pick(rand, ["", " "])}${chance(rand, 0.9) ? shellWord(rand, depth + 1) : ""}`
}

function compound(rand: Random, depth: number): string {
  const body = () => list(rand, depth + 1)
  switch (int(rand, 14)) {
    case 0: return `if ${body()}; then ${body()}; ${chance(rand, 0.3) ? `else ${body()}; ` : ""}fi`
    case 1: return `for ${pick(rand, [...VARIABLES, "in", "do", ""])}${pick(rand, [" ", "\n", "; ", " \\\n", "\n\n"])}${pick(rand, ["in", "in", "do", ""])} ${shellWord(rand, depth)} ${shellWord(rand, depth)}${pick(rand, ["; ", "\n"])}do ${body()}; done`
    case 2: return `while ${body()}; do ${body()}; done`
    case 3: return `until ${body()}; do ${body()}; done`
    case 4: return `case ${shellWord(rand, depth)} in ${pick(rand, ["a", "(a|b)", "*", "a)"])}) ${body()};; ${pick(rand, ["", "b) x;&", "*) y;;&"])} esac`
    case 5: return `{ ${body()}; }`
    case 6: return `(${body()})`
    case 7: return `[[ ${shellWord(rand, depth)} ${pick(rand, ["==", "=~", "-eq", "-lt", "!=", "-v", "-t", "<", "&&", "||"])} ${shellWord(rand, depth)} ]]`
    case 8: return `(( ${arith(rand, depth)} ))`
    case 9: return `${pick(rand, ["function f", "f()", "function f()", "f ()", "function g {", "_x-y.z()"])} { ${body()}; }`
    case 10: return `for (( i = 0; i < 3; i++ )); do ${body()}; done`
    case 11: return `select ${pick(rand, VARIABLES)} in a b; do ${body()}; done`
    case 12: return `repeat ${pick(rand, ["3", "$n", "x"])} do ${body()}; done`
    default: return `${pick(rand, ["time ", "time -p ", "coproc ", "coproc NAME ", "! ", "! ! "])}${pick(rand, ["{ x; }", "(y)", "if true; then z; fi", "[[ a ]]", "while a; do b; done"])}`
  }
}

function list(rand: Random, depth: number): string {
  const items = [depth < 3 && chance(rand, 0.2) ? compound(rand, depth) : simpleCommand(rand, depth)]
  for (let i = int(rand, depth > 1 ? 2 : 4); i > 0; i--) {
    items.push(pick(rand, [" ; ", "; ", " && ", " || ", " | ", " |& ", " & ", "\n", "&&", "\n\n", " &! ", " &| ", ";;", " # note\n"]))
    items.push(depth < 3 && chance(rand, 0.15) ? compound(rand, depth) : simpleCommand(rand, depth))
  }
  if (chance(rand, 0.1)) items.push(pick(rand, [";", " &", " #comment", "\n", " \\", " &&", " |"]))
  return items.join("")
}

export function mutate(rand: Random, text: string): string {
  let out = text
  for (let i = int(rand, 3); i > 0; i--) {
    const at = int(rand, out.length + 1)
    switch (int(rand, 5)) {
      case 0: out = out.slice(0, at); break
      case 1: out = out.slice(0, at) + pick(rand, ODD) + out.slice(at); break
      case 2: out = out.slice(0, at) + out.slice(at + 1 + int(rand, 3)); break
      case 3: out = out.slice(0, at) + out.slice(int(rand, out.length + 1)); break
      default: out = out.slice(0, at) + pick(rand, ODD) + pick(rand, ODD) + out.slice(at); break
    }
  }
  return out
}

function soup(rand: Random, alphabet: readonly string[]): string {
  let out = ""
  for (let i = 1 + int(rand, 14); i > 0; i--) out += pick(rand, alphabet)
  return out
}

const SOUP = [...ODD, "a", "b", "x", "1", "0", " ", "$", "(", ")", "{", "}", "'", '"', "\\", "in", "do", "done", "if", "fi", "case", "esac", "<<", "EOF", "\n"]

/** A random POSIX-family command line. */
export function shellCommand(rand: Random): string {
  const r = rand()
  if (r < 0.12) return soup(rand, SOUP)
  const text = list(rand, 0)
  return r < 0.55 ? text : mutate(rand, text)
}

const CMDLETS = [
  "Get-ChildItem", "Write-Output", "Get-Content", "Set-Location", "Push-Location", "Pop-Location", "cd", "chdir",
  "Remove-Item", "git", "npm", "ForEach-Object", "foreach", "Where-Object", "Invoke-Expression", "iex", "&", ".",
  "Start-Process", "Select-String", "Out-File", "Write-Host", "pushd", "popd", "toString",
]
const PS_PARAMS = ["-Path", "-LiteralPath", "-Recurse", "-Force", "-Filter", "-lp", "-PSPath", "-StackName", "-st", "-p", "-li", "-Path:", "-LiteralPath:x", "--", "--%", "-not", "-split", "-join", "-isplit", "-ſplit"]

export function powerShellWord(rand: Random, depth = 0): string {
  const n = rand()
  if (n < 0.15) return pick(rand, PS_PARAMS)
  if (n < 0.3) return pick(rand, ["foo", "a.txt", "src", "..", "~", "~/x", "*.txt", ".*", ".\\x", "C:\\x", "$HOME", "$PWD", "$PSHOME", "$PSHOME/x", "${HOME}", "$env:HOME", "$env:toString", "${env:PATH}", "$env:__proto__", "$env:NOPE", "+", "-", "1..3", "0x1F", "1kb", "1.5e3d", "0b101", "5ul", "3ſ", "2\u212ab", "-1"])
  if (n < 0.4) return `'${pick(rand, ["a", "it''s", "$x", "a b", "", "`n"])}'`
  if (n < 0.5) return `"${pick(rand, ["a", "$x", "`$x", "a`tb", '""', "$(Get-Date)", "${x}", "`u{1F600}", "`u{d800}", "`u{110000}", "`", "$($x.y)", "a`\nb"])}"`
  if (n < 0.58) return pick(rand, ["$x", "$x.Prop", "$x.Method()", "$x[0]", "$_", "$?", "$$", "$^", "$x::y", "${x}.y", "$x?.y", "$x..y", "$:x"])
  if (n < 0.66 && depth < 3) return `${pick(rand, ["$(", "@(", "(", "{", "@{", "["])}${powerShellList(rand, depth + 1)}${pick(rand, [")", ")", ")", "}", "}", "]"])}`
  if (n < 0.7)
    return `@${pick(rand, ["'", '"'])}${pick(rand, ["", "", " ", "x", "\t", " x"])}${pick(rand, ["\n", "\r\n", "\r", ""])}${pick(rand, ["body", "$x", "$(id)", "a'@b", "`$x", "${x}"])}${pick(rand, ["\n", "\r\n", "\r"])}${pick(rand, ["'", '"'])}@`
  if (n < 0.76) return pick(rand, ["[int]", "[System.IO.Path]::GetTempPath()", "[string]$x", "[Type]::new()", "[x]::y"])
  if (n < 0.82) return `\`${pick(rand, ODD)}`
  if (n < 0.86) return pick(rand, ["<# c #>", "# c", "#", "<#"])
  return pick(rand, ODD) + (chance(rand, 0.5) ? pick(rand, CMDLETS) : "")
}

function powerShellCommand(rand: Random, depth: number): string {
  if (depth < 2 && chance(rand, 0.15)) {
    const block = () => `{ ${powerShellList(rand, depth + 1)} }`
    switch (int(rand, 9)) {
      case 0: return `if (${powerShellList(rand, depth + 1)}) ${block()}${chance(rand, 0.4) ? ` else ${block()}` : ""}`
      case 1: return `foreach ($x in ${powerShellWord(rand, depth)}) ${block()}`
      case 2: return `while (${powerShellWord(rand, depth)}) ${block()}`
      case 3: return `do ${block()} ${pick(rand, ["while", "until"])} (${powerShellWord(rand, depth)})`
      case 4: return `try ${block()} ${pick(rand, ["catch", "finally"])} ${block()}`
      case 5: return `function f ${block()}`
      case 6: return `switch (${powerShellWord(rand, depth)}) { a ${block()} }`
      case 7: return `${pick(rand, [":label ", ":l"])}${pick(rand, ["while", "for", "foreach"])} (1) ${block()}`
      default: return `${pick(rand, ["param", "class", "enum", "using", "return", "throw", "exit", "break", "trap", "begin", "data"])} ${powerShellWord(rand, depth)}`
    }
  }
  const words = [chance(rand, 0.8) ? pick(rand, CMDLETS) : powerShellWord(rand, depth)]
  for (let i = int(rand, 5); i > 0; i--) {
    if (chance(rand, 0.1)) words.push(pick(rand, [">", ">>", "2>&1", "*>&1", "3>", "2>>", "*>", "7>", "1>&2"]) + pick(rand, ["", " out.txt", "out"]))
    else words.push(powerShellWord(rand, depth))
  }
  return words.join(pick(rand, [" ", " ", "  ", "\t", " `\n "]))
}

function powerShellList(rand: Random, depth: number): string {
  const items = [powerShellCommand(rand, depth)]
  for (let i = int(rand, depth > 0 ? 2 : 4); i > 0; i--) {
    items.push(pick(rand, ["; ", " | ", " && ", " || ", "\n", "\r\n", " & ", ";", "\r", ", "]))
    items.push(powerShellCommand(rand, depth))
  }
  return items.join("")
}

const PS_SOUP = [...ODD, "`", "@", "$", "-", "a", "x", "1", ".", "(", ")", "{", "}", "[", "]", "'", '"', "#", "<#", "#>", "--%", "in", "if", "foreach", "::", "..", "?."]

/** A random PowerShell command line. */
export function powerShellCommandLine(rand: Random): string {
  const r = rand()
  if (r < 0.12) return soup(rand, PS_SOUP)
  const text = powerShellList(rand, 0)
  return r < 0.55 ? text : mutate(rand, text)
}

/** A command line about changing directories, to exercise the directory resolution of parse.ts. */
export function directoryCommand(rand: Random, powershell: boolean): string {
  const target = () =>
    powershell
      ? powerShellWord(rand)
      : pick(rand, [shellWord(rand), "~", "~/src", "~+", "~+/x", "~-", "~1", `~${pick(rand, USERS)}`, `~${pick(rand, USERS)}/a/../b`, "..", "-", "--", "-P", "$HOME", "'~'", '"~/x"', "\\~", "a b", "/a//b/./c/", "sub/../..", "$'x'", "{a,b}", ".*", "x*"])
  const head = powershell
    ? pick(rand, ["cd", "Set-Location", "set-location", "Push-Location", "pushd", "chdir", "Pop-Location", "popd", "CD", "sl"])
    : pick(rand, ["cd", "pushd", "popd", "chdir", "builtin cd", "command cd", "command -p cd", "command -v cd", "builtin -- cd", "command -- builtin cd", "CD", "cd -L", "cd -P --"])
  const args: string[] = []
  for (let i = int(rand, 3); i >= 0; i--) {
    if (powershell && chance(rand, 0.35)) args.push(pick(rand, PS_PARAMS))
    args.push(target())
  }
  const command = `${head} ${args.join(" ")}`
  const joined = chance(rand, 0.3) ? `${command}${pick(rand, ["; ", " && ", "\n"])}${powershell ? powerShellCommand(rand, 1) : simpleCommand(rand, 1)}` : command
  return chance(rand, 0.2) ? mutate(rand, joined) : joined
}

// Constructs a random grammar reaches too rarely: nesting around the 32-level limit, Zsh glob qualifiers and flag
// groups, escapes that reach arithmetic sinks, PowerShell labels the label regex shortens, inputs at the size limit.
const NESTERS = [
  ["$(", ")"], ['"$(', ')"'], ["${x:-", "}"], ["$((", "))"], ["((", "))"], ["(", ")"], ["{ ", "; }"], ["<(", ")"],
  ["if a; then ", "; fi"], ["arr=(", ")"], ["`", "`"], ["$[", "]"], ["[[ ( ", " ) ]]"], ["${x[", "]}"], ["case a in b) ", ";; esac"],
  ["f() { ", "; }"], ["${ ", "; }"], ["x=$(", ")"], ["echo ${#", "}"], ["@(", ")"],
] as const
const PS_NESTERS = [["(", ")"], ["{ ", " }"], ["$(", ")"], ["@(", ")"], ["[", "]"], ["@{", "}"], ['"$(', ')"'], ["if (1) { ", " }"], ['@"\n$(', ')\n"@']] as const

export function nested(rand: Random, powershell: boolean): string {
  const depth = 26 + Math.floor(rand() * 12)
  const nesters = powershell ? PS_NESTERS : NESTERS
  const chosen = Array.from({ length: depth }, () => (rand() < 0.7 ? nesters[0] : pick(rand, nesters)))
  const core = powershell ? pick(rand, ["Get-Item x", "1", "$x", "a"]) : pick(rand, ["echo hi", "1", "x", "a b"])
  const text = chosen.map(([open]) => open).join("") + core + chosen.reverse().map(([, close]) => close).join("")
  return rand() < 0.3 ? mutate(rand, text) : text
}

const QUALIFIER_TOKENS = ["/", "F", ".", "@", "=", "*", "r", "w", "x", "A", "s", "S", "t", "U", "^", "M", "T", "N", "D", "n", "-", ",", "om", "oL", "On", "Lk+3", "Lm-1", "L5", "a-2", "mh5", "cM1", "l3", "Y1", "[1]", "[-1,3]", "[1,]", "e:cmd:", "+x", ":", "q", "(", "oz", "m", "L", "[x]"]

export function zshTargeted(rand: Random): string {
  const word = pick(rand, ["*", "a", "x*", "*.txt", "dir/", "~/a", "$x", "a=", "b"])
  const qualifier = Array.from({ length: Math.floor(rand() * 4) }, () => pick(rand, QUALIFIER_TOKENS)).join("")
  const after = pick(rand, ["", " ", ";", " | cat", ")", "x", "\\\n ", "&", "<", "(", "\n"])
  const flags = Array.from({ length: Math.floor(rand() * 4) }, () => pick(rand, ["P", "q", "q+", "e", "f", "s:a:", "j:,:", "s))", "s(x(", "@", "%", "z", "j", "s", "(", "jxPx", "s:P:"])).join("")
  return pick(rand, [
    `ls ${word}(${qualifier})${after}`,
    `arr=(${word}(${qualifier})${after})`,
    `echo \${(${flags})${pick(rand, ["x", "~x", "^x", "=x", "x[1]", "!x", "(x)"])}}`,
    `echo $${pick(rand, ["^", "=", "~", "^~", "#", "##", "+", "\\\n", "^\\\n~"])}${pick(rand, ["x", "#", "1", "@", "{x}"])}${pick(rand, ["", "[1]", "[$i]", "[$(id)]", "\\\n["])}`,
    `setopt ${pick(rand, ["globsubst", "KSH_GLOB", "no_aliases", "-m", "+o", "prompt_subst", "o"])}; x`,
    `${pick(rand, ["set -o", "set +o", "set -k", "emulate", "unsetopt"])} ${pick(rand, ["kshglob", "nokshglob", "x", "keyword", "-m"])}`,
  ])
}

export function escapeTargeted(rand: Random): string {
  const value = pick(rand, ["'\\U41'", "'\\x5b'", "'\\101'", "'\\u005b'", '"\\U5b"', "'\\x'", "'\\8'", "$'\\x5b'", "'a[1]'", "'$(id)'", "'`x`'", "'{x}'"])
  const name = pick(rand, ["x", "a", "PS4", "i"])
  const sink = pick(rand, [`echo $((${name}))`, `echo \${arr[$${name}]}`, `printf -v ${name} '%s' y`, `eval "$${name}"`, `let ${name}`, `[[ $${name} -eq 1 ]]`, `declare -n r=${name}`, `read ${name}`, `echo \${!${name}}`, `test -v ${name}`, `echo ${name}`, `(( ${name} ))`])
  const assign = pick(rand, [`${name}=${value}`, `export ${name}=${value}`, `declare ${name}=${value}`, `read ${name} <<< ${value}`, `for ${name} in ${value}; do :; done`, `${name}=(${value})`, `local ${name}=$(printf ${value})`])
  return `${assign}${pick(rand, ["; ", "\n", " && "])}${sink}`
}

// Bound values that reach a sink only through a construct no word records: parameters in heredoc bodies and
// conditionals, Zsh flag groups, line continuations inside arrays and conditionals.
export function sinkTargeted(rand: Random): string {
  const bound = pick(rand, ["x='$(a)[1]'", "x='`a`'", "y=$(printf '\\x5b')", "a=('$(b)')", "read v <<< '{a}[1]'", ""])
  const use = pick(rand, [
    "cat <<E\n${PS4}\nE\n",
    "cat <<E\n${PS4:-x}\nE\n",
    "[[ ${PS4} ]]",
    "(( ${PS4} ))",
    "echo ${(s:P:)y}",
    "echo ${(j:P:)y}",
    "echo ${(sxPx)y}",
    "echo ${(P)y}",
    "echo ${(s:a:P)y}",
    "arr=(a \\\nb)",
    "arr=(a\\\n b)",
    "[[ a \\\n== b ]]",
    "[[ a ==\\\n b ]]",
    "[[ a \\\n]]",
    "[[ a\\\n ]]",
    "[[ -v a \\\n]] && b",
    "[[ a \\b\n]]",
    "[[ ( a \\\n) ]]",
    "[[ a \\\n# c\n]]",
    "ls @(a\\\nb)",
    "case x in (a\\\n)) y;; esac",
  ])
  return `${bound}${bound ? pick(rand, ["; ", "\n"]) : ""}${use}`
}

// Shapes a second mutation sweep showed missing: keywords after redirects, odd function bodies, ANSI quotes after
// globs, wrapper option ends before cd.
export function oddTargeted(rand: Random): string {
  const shapes = [
    "f() a[ b", "f() x[[ a ]]", "function f x[ y", "f() x[[\\\n a ]]", "if a; then b; >x fi", "while a; do b; >x done",
    "{ a; >x }", "if a; >x then b; fi", "case a in b) c;; >x esac", "*$'\\n' x", "echo a?$'\\x41'b", "ls [x]$'\\t'",
    "echo .*$'a'", "builtin -- -x cd /tmp", "command -- --x cd /tmp", "builtin -- cd /tmp", "command -p -- cd /tmp",
  ]
  const text = pick(rand, shapes)
  return rand() < 0.5 ? text : `${text}${pick(rand, ["; ", "\n", " && "])}${pick(rand, shapes)}`
}

export function memberTargeted(rand: Random): string {
  return `${pick(rand, ["Write-Output ", "", "$r = ", "& "])}${pick(rand, [
    "[int][string]$x", "$a[0][1]", "[T][U]::x", "$x[0][1].y", "[a]x[b]", "$a[1] [2]", "$x.a:b", "$x.a:b()", "$x.a.b:c()",
    "$x.y-:z", "$x.a::b()", "$x::a.b()", "[T]::a.b()", "$x?.a()", "$x.a?.b()", "$x.a..b", "$x. a()", "${x}.a()",
    "$x.a:b:c", "@{a=1}.a()", "(1).a()", "$x.'a'()", '$x."a"()', "$x.a`b()", "0x1Fz", "0xdd", "0x1d", "0x1dkb",
    "0x1dz", "0x1d.x", "0x12kbz", "[1#2]", "[a#b]", "[Int32#x]", "[System.IO#a]::b", '"`u{01F600}"', '"`u{10FFFF}"',
    '"`u{000041}"', "-1: --% x", "a -x: --% y",
  ])}${pick(rand, ["", " | Out-Null", "; y", " # c"])}`
}

export function labelTargeted(rand: Random): string {
  const keywords = ["while", "for", "foreach", "do", "switch", "WHILE", "For"]
  const label = `:${pick(rand, ["a", "x_", "lbl"])}${rand() < 0.6 ? pick(rand, keywords) : ""}${pick(rand, ["", "1", "x"])}`
  return `${label}${pick(rand, [" ", "", "\t", "  "])}${pick(rand, keywords)}${pick(rand, [" (1) { a }", "(1){a}", " ($x in $y) { b }", " { c } while (1)", "", ";x"])}`
}

/** Inputs near the 64 KiB limit and the work budget. */
export function large(rand: Random): string {
  const unit = pick(rand, ["echo a; ", "$(", "${x:-", "a[", "x\\\n", "'", "((", "{ ", "cat <<E\n", "\\", "$((", "`", "a ", "x=$(y) "])
  const target = pick(rand, [65535, 65536, 65537, 40000, 20000])
  let text = unit.repeat(Math.ceil(target / unit.length)).slice(0, target)
  if (rand() < 0.5) text = text.slice(0, text.length - Math.floor(rand() * 4)) + pick(rand, ["", ")", "}", "\nE\n", "'"])
  return text
}

/** Random pattern and input for Wildcard.match. */
export function wildcardPair(rand: Random): [string, string] {
  const alphabet = ["a", "b", " ", "*", "?", "[*]", "[?]", "[", "]", "\\", "/", ".", "+", "(", ")", "$", "^", "|", "{", "}", "😀", "é", "\n", "-", "ab", " *"]
  const pattern = soup(rand, alphabet)
  const input = chance(rand, 0.5)
    ? pattern.replace(/\[([*?])\]/g, "$1").replace(/\*/g, () => pick(rand, ["", "x", "ab c", "😀"])).replace(/\?/g, () => pick(rand, ["q", "😀", "é", ""]))
    : soup(rand, ["a", "b", " ", "*", "?", "/", "\\", ".", "😀", "\n", "x"])
  return [input, pattern]
}

/** A random path for the path.posix functions. */
export function pathText(rand: Random): string {
  return soup(rand, ["/", "/", ".", "..", "a", "b", "~", "//", "./", "../", "é", " "])
}
