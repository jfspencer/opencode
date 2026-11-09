# opencode-shell

Shell command analysis for the Rust server, ported from the TS reference: what a command line runs, the
permission request the shell tool makes for it, and how the tool reports what ran. Pure functions, no
dependencies; spawning processes, reading output files and asking the user are the server's.

| Module | TS (`packages/core/src/`) |
|---|---|
| `scan` | `shell/scan.ts`: the commands of a Bash, Zsh, POSIX sh or PowerShell command line, or why it is opaque |
| `parse` | `shell/parse.ts`: each command's permission resource and saved pattern, the directories `cd` moves to; `FileAccess.resolvePath` |
| `wildcard` | `util/wildcard.ts`: the permission pattern language |
| `permission` | `Permission.evaluate` and the rule decisions of `permission.ts`; `whollyDisabled` of `tool.ts` |
| `select` | the pure part of `shell/select.ts`: shell names, PowerShell detection, login shells, arguments |
| `result` | `shell/result.ts`, the output capture of `Shell.result` (`shell.ts`), the shell tool's result shapes (`tool/plugin/shell.ts`) |
| `path` | Node's `path.posix` (`normalize`, `join`, `resolve`, `dirname`, `basename`) |
| `js` | JavaScript string semantics over UTF-16 units |

The scanner is a function-for-function transliteration of `scan.ts`: the same work budget, the same order of
checks, the same failure reasons. Each regular expression of the TS source is a hand-written matcher that names
the regex it implements (several backtrack; the comments say where and why the hand-written form is equivalent).
`unicode.rs` is generated from the TS runtime itself (`bun harness/gen-unicode.ts`), so `\p{L}` and `\p{Nl}` follow
the same Unicode version as JavaScriptCore.

Strings are UTF-16 inside, as in TS: every offset (`wordEnds`, `redirectPrefixEnd`) and every `?` wildcard counts
UTF-16 units. The `_units` functions take and return `Vec<u16>`; the `&str` ones convert at the boundary, replacing
a lone surrogate (only a PowerShell `` `u{d800} `` escape can make one) with U+FFFD.

## How the server uses it

The shell tool (`tool/plugin/shell.ts`, `prepare`), for a call with `command` and `workdir`:

```rust
let host = opencode_shell::SystemHost::new(); // once per process: $HOME, /etc/passwd, the environment
// cwd: workdir resolved against the location (FileAccess.resolve, the server's), shell: the compatible shell
let request = opencode_shell::analyze(&command, &shell, &cwd, &host)?; // Err(message): fail the call with it
for directory in &request.directories {
    let absolute = opencode_shell::resolve_path(&cwd, directory, &home); // then FileAccess.resolve + external_directory
}
if !request.resources.is_empty() {
    // Permission.assert { action: "shell", resources: request.resources, save: request.save, ...invocation }
}
```

`Err` carries TS's message, `Portable shell scanner cannot analyze command: <reason>`; the tool turns it into the
`Unable to execute command: <command>` failure (`result::failure_message`). The order of the TS steps matters and
stays the server's: resolve the workdir, analyze, authorize the external directories, assert the `shell`
permission, check the workdir again, spawn.

The permission layer evaluates with `permission::evaluate(action, resource, &rules)` (the last matching rule, else
`ask`), and `permission::decide(action, &resources, &configured, &saved)` gives what `Permission.ask`/`assert` reach
before plugin hooks: deny when the agent and session rules deny any resource, else ask when any resource asks under
those rules followed by the saved ones, else allow. `wildcard::matches` is `Wildcard.match` wherever else TS uses it
(tool visibility, managed policies).

The shell tool's output: `result::capture` builds the bounded tail from the output file's last bytes (as
`Shell.result` does), `result::output` / `completed` / `background` / `result_messages` / `tool_metadata` the tool
result, `result::notification` and `user_notification` the synthetic messages of background and user commands,
`result::tool_description` the tool's description (with `select::name` of the compatible shell). `select::args`
are the arguments a shell runs a command with.

## Verification

```sh
port/rust/crates/opencode-shell/harness/check.sh                        # the standard run (about 360,000 cases)
port/rust/crates/opencode-shell/harness/check.sh --cases 30000 --seed 7 # a deeper one
port/rust/crates/opencode-shell/harness/check.sh --no-upstream --only parse --show 20
```

`harness/driver.ts` (bun) runs every case through the TS modules of the reference (`port/.reference/packages/core/src`)
and through the Rust crate (`harness/rust`, the `shell-harness` example), and the two JSON answers must be
byte-identical; differences go to `port/rust/target/shell-harness/differences.jsonl`. The corpus:

1. every input upstream's own tests hand to `ShellScan`, `ShellParse` and `Wildcard` (all `test/shell-scan/*`,
   `test/shell*`, `test/permission.test.ts`, `test/tool-shell.test.ts`, `test/session-shell.test.ts`), recorded
   by running those tests with `harness/record.ts` as a preload; each command is also scanned in every dialect;
2. every shell command the conformance goldens record, and every string literal of the `shell-scan`,
   `shell-classify`, `shell-tool` and `permissions` scenarios, in every dialect and as a PowerShell command;
3. generated command lines (`harness/generate.ts`, seeded): a grammar of words, quoting, escapes, variables and
   parameter operators, command and process substitution, arithmetic, heredocs, redirects, pipes and lists,
   compound commands, functions, comments, Unicode and odd whitespace, half of them mutated (truncated, spliced,
   sprinkled with special characters) into unterminated and malformed constructs; PowerShell lines the same way;
   `cd`-family commands with tilde, environment and path forms; wildcard patterns, paths, rulesets, shell results
   and byte sequences.

The TS side runs with this process's home, user and environment; the harness passes them to Rust.

## Known gaps

- Windows: case-insensitive wildcards, `path.win32`, Git Bash and `cmd` lookup, `~\` are not ported (the port
  targets Linux and macOS). macOS is ported (`/Users/<name>` for `~name`) but only Linux runs the harness.
- `Host::user_home` stands in for `os.userInfo()` plus `/etc/passwd`; `SystemHost` finds the current user through
  `/proc/self` on Linux and has no current user on macOS (where `/etc/passwd` rarely lists people anyway).
- `select::name` lowercases with Rust's Unicode tables, JavaScriptCore with its own; they differ only for
  characters newer than one of them, and only the ASCII result is ever compared.
- `result::capture` decodes the tail with `String::from_utf8_lossy`; the harness checks that against
  `Buffer#toString("utf8")` byte sequence by byte sequence, but `capture` itself has no exported TS counterpart, so
  its arithmetic is checked by the conformance scenarios that truncate output (`shell-tool`), not here.

## TS defects reproduced (to be fixed in the reference by a deviation patch)

The port matches TS in each case; each needs the deviation patch, then the port follows the fixed behavior.

- **Quoted wrapper names escape the wrapper rule** (`bashPrefix`, parse.ts). The wrapper table is looked up by
  the *raw* word, so `'sudo' ls` (or `\sudo ls`, `"sudo" ls`, `s''udo ls`) saves `'sudo' *`, which matches
  `'sudo' rm -rf /`. Unquoted, `sudo ls` saves `sudo ls *`, as the comment on `WRAPPERS` intends.
- **Quoted command names escape the arity table** (`prefix`, same cause): `'git' status` saves `'git' *`, which
  matches `'git' push --force`; `git status` saves `git status *`.
- **A quoted `cd` escapes directory tracking** (`unwrapDirectoryCommand`, same cause): `'cd' /tmp && touch x`
  reports no directories, so no `external_directory` access is asked, while `cd /tmp && touch x` reports `/tmp`.
  (`c\d /tmp` likewise.)
- **`$env:<Object.prototype member>`** (`environment`, parse.ts): PowerShell `$env:NAME` reads `process.env[NAME]`
  with a plain property access, so `cd $env:toString` under pwsh reports the directory
  `function toString() { [native code] }` (`$env:__proto__` gives `[object Object]`, `$env:constructor`
  `function Object() { [native code] }`). Reproduced in `parse.rs` (`environment`).
- Minor: `Wildcard.match`'s `?` matches one UTF-16 unit (the regex has no `u` flag), so `a?` does not match `a😀`.
