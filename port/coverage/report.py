"""Compare coverage of the in-scope TS source between test runs, and write the scenario backlog.

Usage: python3 -I port/coverage/report.py [labels]        (default labels: bb,all)
Reads port/coverage/out/<label>-*/lcov.info from port/coverage/run.sh. The file list and source come from
the commit recorded in out/<label>.ref, so results stay consistent after pulling upstream.

The scenario backlog (out/scenario-backlog.csv) lists, per file, the lines the full upstream suite reaches
but the Rust-compatible tests ("bb") don't: behavior upstream tests only through TS internals.
"""

import collections
import csv
import glob
import os
import re
import subprocess
import sys

# --- Scope: the TS source whose observable behavior the Rust implementation must reproduce. -------------
KEEP_PROVIDER = {"openrouter.ts", "configured.ts", "credential-settings.ts", "dynamic.ts", "refresh.ts",
                 "sdk-factory.ts", "prompt-cache-key.ts"}


def in_scope(path):
    m = re.match(r"packages/(core|ai|server|protocol|schema|cli|util|client|codemode|tui|theme)/src/(.*)$", path)
    if not m or not re.search(r"\.tsx?$", path) or path.endswith(".d.ts") or re.search(r"\.(test|spec)\.", path):
        return False
    pkg, rest = m.groups()
    # Out of scope by decision (2026-11-07):
    #   no web app/desktop/GUI (whole packages not listed above), no plugins, OpenRouter only,
    #   no `opencode mini`, no v1 storage migration, no AI SDK bridge, hardcoded OpenRouter access point
    #   (no model catalog/resolver, no pricing), no MCP, no legacy v1 config/schema formats.
    if pkg == "tui" and re.match(r"(mini/|feature-plugins/sidebar/mcp\.tsx|config/v1/|theme/v1\.ts)", rest):
        return False
    if pkg == "cli" and re.match(r"(mini(-host)?\.ts|commands/handlers/(mini|mcp/))", rest):
        return False
    if pkg == "core" and re.match(r"(database/v1-migration|aisdk(-native)?\.ts|models-dev|model-resolver\.ts|mcp/|"
                                  r"tool/mcp\.ts|tool/plugin/mcp|config/plugin/mcp|v1/|github-copilot/)", rest):
        return False
    if pkg == "core" and rest.startswith("plugin/"):
        return rest.startswith("plugin/provider/") and rest.count("/") == 2 and rest.split("/")[-1] in KEEP_PROVIDER
    if pkg in ("server", "protocol") and re.search(r"(^|/)mcp\.ts$", rest):
        return False
    if pkg == "schema" and re.match(r"(mcp(-event)?\.ts|config/mcp\.ts|v1/)", rest):
        return False
    if pkg == "ai":
        if rest.startswith("providers/"):
            return bool(re.match(r"providers/(openrouter(\.ts|/)|settings\.ts|index\.ts)", rest))
        if rest.startswith("protocols/"):
            return bool(re.match(r"protocols/(openai-chat|shared|index)\.ts|protocols/utils/", rest))
        if re.match(r"(image|media|speech|transcription|video)", rest) or rest.startswith("route/media"):
            return False
    return True


# --- Report ------------------------------------------------------------------------------------------------
root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()
out = os.path.join(root, "port", "coverage", "out")
labels = (sys.argv[1] if len(sys.argv) > 1 else "bb,all").split(",")
refs = {l: open(os.path.join(out, f"{l}.ref")).read().strip() for l in labels}
if len(set(refs.values())) > 1:
    sys.exit(f"runs were measured on different commits: {refs}; rerun them on one commit")
ref = refs[labels[0]]


def git(*args):
    return subprocess.run(["git", "-C", root, *args], capture_output=True, text=True, errors="ignore").stdout


def load(label):
    lines, funcs = collections.defaultdict(dict), {}
    for f in glob.glob(os.path.join(out, f"{label}-*", "lcov.info")):
        base = os.path.join(root, "packages", f.split(f"{label}-")[-1].split("/")[0])
        sf = fnf = None
        for line in open(f):
            if line.startswith("SF:"):
                sf = os.path.relpath(os.path.normpath(os.path.join(base, line[3:].strip())), root)
            elif line.startswith("FNF:"):
                fnf = int(line[4:])
            elif line.startswith("FNH:") and sf:
                prev = funcs.get(sf, (0, 0))
                funcs[sf] = (max(prev[0], fnf or 0), max(prev[1], int(line[4:])))
            elif line.startswith("DA:") and sf:
                n, h = line[3:].split(",")[:2]
                lines[sf][int(n)] = lines[sf].get(int(n), 0) + int(h)
    return lines, funcs


covs = {l: load(l) for l in labels}
scope = [f for f in git("ls-tree", "-r", "--name-only", ref, "packages").split() if in_scope(f)]
rows = collections.defaultdict(collections.Counter)
backlog = []
for f in scope:
    pkg = f.split("/")[1]
    measured = set().union(*(c[0].get(f, {}).keys() for c in covs.values()))
    if not measured:
        rows[pkg]["unloaded_files"] += 1
        rows[pkg]["unloaded_lines"] += sum(1 for l in git("show", f"{ref}:{f}").splitlines()
                                           if l.strip() and not l.strip().startswith(("//", "*", "/*", "import ")))
        continue
    hit = {l: {n for n in measured if c[0].get(f, {}).get(n, 0) > 0} for l, c in covs.items()}
    fnf = max(c[1].get(f, (0, 0))[0] for c in covs.values())
    rows[pkg]["lines"] += len(measured)
    rows[pkg]["fnf"] += fnf
    for l, c in covs.items():
        rows[pkg][l] += len(hit[l])
        rows[pkg]["fn:" + l] += min(c[1].get(f, (0, 0))[1], fnf)
    if "bb" in hit and "all" in hit:
        backlog.append((f, len(measured), len(hit["bb"]), len(hit["all"]), len(hit["all"] - hit["bb"])))

pct = lambda a, b: f"{100 * a / b:6.1f}%" if b else "     -"
print(f"coverage of in-scope source at {ref[:10]} (L = lines, F = functions; import-time execution inflates L)")
print(f"{'package':10} {'lines':>7} " + " ".join(f"{'L:' + l:>7}" for l in labels) + f" {'funcs':>6} "
      + " ".join(f"{'F:' + l:>7}" for l in labels) + "  unloaded files/lines")
total = collections.Counter()
for pkg in sorted(rows):
    r = rows[pkg]
    total.update(r)
    print(f"{pkg:10} {r['lines']:7} " + " ".join(pct(r[l], r["lines"]) for l in labels) + f" {r['fnf']:6} "
          + " ".join(pct(r["fn:" + l], r["fnf"]) for l in labels) + f"  {r['unloaded_files']}/{r['unloaded_lines']}")
print(f"{'TOTAL':10} {total['lines']:7} " + " ".join(pct(total[l], total["lines"]) for l in labels)
      + f" {total['fnf']:6} " + " ".join(pct(total["fn:" + l], total["fnf"]) for l in labels)
      + f"  {total['unloaded_files']}/{total['unloaded_lines']}")
print(f"in-scope source files: {len(scope)}")
if backlog:
    backlog.sort(key=lambda r: -r[4])
    path = os.path.join(out, "scenario-backlog.csv")
    with open(path, "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["file", "lines", "blackbox_covered", "full_suite_covered", "gap_lines"])
        w.writerows(backlog)
    print(f"\nscenario backlog: {sum(r[4] for r in backlog)} lines -> {os.path.relpath(path, root)}")
    for r in backlog[:10]:
        print(f"{r[4]:6}  {r[0]}")
