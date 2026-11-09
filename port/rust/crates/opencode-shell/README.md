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

Strings are UTF-16 inside, as in TS: every offset (`wordEnds`, `redirectPrefixEnd`) counts UTF-16 units. A `?`
wildcard matches one code point (the TS regex has the `u` flag), so an astral character is one and a lone surrogate
is one of its own. The `_units` functions take and return `Vec<u16>`; the `&str` ones convert at the boundary,
replacing a lone surrogate (only a PowerShell `` `u{d800} `` escape can make one) with U+FFFD.

The saved prefix of a Bash command is looked up by the words' values, as the scanner unquotes them, and kept as
written. So `'sudo' ls`, `\sudo ls` and `/usr/bin/sudo ls` are sudo running `ls` (`'sudo' ls *`), and
`/usr/bin/git status` is git's `status` (`/usr/bin/git status *`), because the arity table is keyed by the command's
basename. A quoted option (`git '-C' x status`) still hides the subcommand, so that command is saved exactly.
Directory commands are recognized by value too: `'cd' /tmp` and `c\d /tmp` report `/tmp`, and they stay a `shell`
resource (`'cd' *`), where a plain `cd` is dropped.

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
port/rust/crates/opencode-shell/harness/check.sh                        # the standard run: ~416,000 cases, ~45 s
port/rust/crates/opencode-shell/harness/check.sh --cases 30000 --seed 7 # a deeper one: ~1,110,000 cases, ~70 s
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
   and byte sequences. Targeted families add what a random grammar reaches too rarely: nesting across the
   32-level limit, inputs at the 64 KiB limit, Zsh glob qualifiers and flag groups, escapes and bound values that
   reach arithmetic sinks (also through heredocs and `[[ ]]`), PowerShell labels, member chains and here-strings,
   command names and options written quoted, escaped or with a path (wrappers, `eval`, arity commands, `cd` and
   its `builtin`/`command` forms), and wildcard pairs around astral characters and lone surrogates;
4. `Shell.result`'s capture and `Shell.output` pages (no pure TS function): the TS `Shell` service itself, built
   with upstream's test fixtures, runs `cat` on generated output files under several `tool_output` limits
   (`harness/capture.ts`);
5. the shell tool's module-private texts (its description per shell, background and timeout results, the
   background instruction): the strings the conformance goldens recorded from TS.

The TS side runs with this process's home, user and environment; the harness passes them to Rust.

The harness is itself checked by mutation (`bun harness/mutants.ts [count] [seed]`: random operator mutants of
the scanner, parser and helpers, `<`/`<=`, `+ 1`/`+ 2`, `&&`/`||`, `==`/`!=`, `true`/`false`, `break`/`continue`,
each built and run against a quick corpus; it restores every file it touches; `all` with `MUTANTS_ONLY=file:from-to,...`
runs every site of the lines a change touched). Three sweeps (60, 60 and 50 mutants) plus hand-picked ones; each
survivor either got a generator family that catches it or was shown equivalent. The D116–D119 changes had every site
of their lines swept (44 mutants), plus 16 hand-picked ones that each revert one piece of the new logic. Every mutant
that changes behavior is now caught. The equivalent ones: an assignment word ending in
`=` is never one character; a `<<` at the end of input fails before its prefix is used; the character after `/`
always resets the segment state; an escaped character in double quotes cannot be the first to break a static word;
a sticky `u`-flag regex never starts mid-pair after an ASCII `@` or `$`; a PowerShell command reaches the comment
branch's `finishCommand` without words only as a bare invocation, invalid either way; `function_head` is only
called at a real character; an escaped CR before LF inside PowerShell double quotes gives the same word whether
the LF goes with the escape or not. Three more are equivalent:
- `unwrap_directory_command` meets index 0 only for a head it already found outside `CWD`.
- `bash_prefix` reads no value past the words it was given, so the raw words' values serve as well as the
  redirect-cut words' values.
- A PowerShell head in `CWD` is a bare name whose value is itself, so the `powershell ||` of the plain-`cd` test
  changes nothing.

One is not caught: the work budget's off-by-one (`budget < 0` against
`<= 0`), observable only by an input that exhausts exactly 2,097,152 steps.

## Known gaps

- Windows: case-insensitive wildcards, `path.win32`, Git Bash and `cmd` lookup, `~\` are not ported (the port
  targets Linux and macOS). macOS is ported (`/Users/<name>` for `~name`) but only Linux runs the harness.
- `Host::user_home` stands in for `os.userInfo()` plus `/etc/passwd`; `SystemHost` finds the current user through
  `/proc/self` on Linux and has no current user on macOS (where `/etc/passwd` rarely lists people anyway).
- `select::name` lowercases with Rust's Unicode tables, JavaScriptCore with its own; they differ only for
  characters newer than one of them, and only the ASCII result is ever compared.
- `permission::decide`, `denied`, `relevant`, `allowed_unattended` and `wholly_disabled` are closures inside TS
  services (`Permission`'s layer, `tool.ts`); only `Permission.evaluate`, which they compose, is checked
  differentially. `result::output`, `notice`, `notification`, `user_notification`, `capture` and `page` are
  checked; `result_messages`, `tool_metadata`, `job_text` and `tool_timeout` only through the scenarios.
- `SystemHost` is not run by the harness (it passes TS's own home, user and environment instead).

## TS defects this crate found

The harness found these in upstream. Each is fixed in the reference by a deviation patch (`port/patches/`), and the
crate follows the fixed behavior:

- **D116**: wrappers, `eval`, options and the arity table were looked up by the raw word. So `'sudo' ls` (or
  `\sudo ls`) saved `'sudo' *`, which approves `'sudo' rm -rf /`. Likewise `'git' status` saved `'git' *`,
  `/usr/bin/git status` saved `/usr/bin/git *`, and `git '-C' x status` slipped past the option rule (D106).
- **D117**: a quoted or escaped `cd` (`'cd' /tmp && touch x`, `c\d /tmp`) reported no directory, so no
  `external_directory` access was asked.
- **D118**: PowerShell `$env:NAME` read `process.env[NAME]` with a plain property access. So `cd $env:toString`
  reported the directory `function toString() { [native code] }`, and `$env:__proto__` and `$env:constructor`
  did the same. Only the environment's own keys resolve now. `SystemHost::env` also refuses a name with `=`,
  which `getenv` would read as a prefix.
- **D119**: `Wildcard.match`'s `?` matched one UTF-16 unit (the regex had no `u` flag), so `a?` missed `a😀`.
