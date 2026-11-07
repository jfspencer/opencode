# Deviation patches

The Rust port does not ship upstream bugs it knows about. Each patch here fixes one upstream bug in the TS
reference that the conformance oracle records. Rust has to reproduce the fixed behavior, and the goldens
still come from running code rather than being hand-written. `port/scripts/reference.sh` applies the patches
in file-name order to a sparse worktree of upstream (`port/.reference`) that the conformance drivers load.

A patch's commit message is its deviation record:

```
D101 permissions: "always" on a wrapped command approves only that command

Upstream: approving `sudo rm -rf /var/cache` with "always" saves `sudo *`, which then allows every sudo
command (same for sh, env, xargs, `$(which git)`).
Fixed: the saved pattern names the wrapped command (`sudo rm *`).
Scenarios: shell-permissions.test.ts "always on a wrapped command"
Upstream tests: test/permission/arity.test.ts updated (asserted the wrapper-only pattern)
```

ID ranges: D1xx permissions and model requests, D2xx HTTP API, D3xx TUI, D4xx CLI, D5xx code mode.

## When to add one

- When the oracle records behavior that is wrong: a crash, a wrong status code, a security hole, data
  loss, an ignored setting, or an output that contradicts upstream's own docs or types. If it is a design
  choice, it is not a bug, even when another choice seems better: that belongs in a proposal, not here.
- When the Rust implementation surfaces a bug in the reference while porting.

Make the smallest fix upstream would accept as a PR, in upstream's style. If an upstream test asserts the
buggy behavior, change it in the same patch, so the reference still passes upstream's own suite. Then
re-record the goldens the fix changes, and read the diff: every change must follow from the fix.

```sh
port/scripts/reference.sh build                      # once, and after every upstream merge
$EDITOR port/.reference/packages/...                 # fix the bug
(cd port/.reference/packages/<pkg> && bun test <affected tests>)
port/scripts/reference.sh save "D123 area: what is fixed" "$(cat record.txt)"
cd port/conformance && UPDATE_GOLDENS=1 bun test scenarios/<file> && git diff --stat goldens
```

## After an upstream merge

`reference.sh build` re-applies every patch to the new upstream. A patch that no longer applies means
upstream changed that code. Check the bug against the new code:
- If upstream fixed it, delete the patch and re-record. The goldens should come out unchanged.
- If it is still there, re-derive the fix on the new code (`reference.sh save` with the same ID).

The goldens then reveal behavior changes from both sides: upstream's changes and any change in how a
patch interacts with them.
