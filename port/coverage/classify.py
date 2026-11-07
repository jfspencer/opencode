"""Sort upstream's in-scope test files by the boundary they exercise.

Only tests that reach the system from outside (HTTP client, CLI process, whole-TUI keystrokes and frames,
recorded OpenRouter traffic) can run against the Rust implementation. Everything else calls TS internals.

Usage: python3 -I port/coverage/classify.py   -> prints a summary, writes port/coverage/out/lists.json
Categories:
  A  HTTP API through the generated client        (runs against Rust)
  B  whole TUI: keys + fake server -> frames       (runs against Rust)
  D  CLI run as a process                          (runs against Rust)
  E  recorded OpenRouter provider traffic          (outbound requests comparable)
  E-other  recorded traffic for other providers    (out of scope)
  C  single TUI component render                   (internal)
  G  TUI state against a fake server               (internal)
  F  internal functions and services               (internal)
"""

import collections
import json
import os
import re
import subprocess

root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()
out_dir = os.path.join(root, "port", "coverage", "out")
pkgs = "core ai server protocol schema cli util client codemode tui theme sdk".split()
files = subprocess.run(["git", "-C", root, "ls-files"] + [f"packages/{p}" for p in pkgs], capture_output=True, text=True).stdout.split()
files = [f for f in files if re.search(r"\.(test|spec)\.tsx?$", f)]

R = {
    "http_client": re.compile(r"ServerFetch|OpenCode\.make|createFetch\(|tui-client|fetch\(\s*[`'\"]http://(127|localhost|opencode)"),
    "tui_app": re.compile(r"createAppFixture|createSimulation|SimulationRenderer"),
    "tui_render": re.compile(r"captureCharFrame|createTestRenderer|testRender\("),
    "spawn_cli": re.compile(r"Bun\.spawn|spawnSync|\$`|child_process|execFile"),
    "cassette": re.compile(r"HttpRecorder|cassette|Cassette|recordReplay|replay\("),
}


def category(path, src):
    h = {k: bool(r.search(src)) for k, r in R.items()}
    pkg = path.split("/")[1]
    if h["tui_app"]:
        return "B"
    if h["http_client"]:
        return "G" if pkg == "tui" else "A"
    if h["spawn_cli"] and pkg == "cli":
        return "D"
    if h["tui_render"]:
        return "C"
    if h["cassette"]:
        return "E" if "openrouter" in src.lower() else "E-other"
    return "F"


lists = collections.defaultdict(list)
for f in files:
    lists[category(f, open(os.path.join(root, f), encoding="utf-8", errors="ignore").read())].append(f)

os.makedirs(out_dir, exist_ok=True)
with open(os.path.join(out_dir, "lists.json"), "w") as fh:
    json.dump(lists, fh, indent=1)
for c in sorted(lists):
    print(f"{c:8} {len(lists[c]):4} files")
print(f"total    {len(files):4} files -> {os.path.relpath(os.path.join(out_dir, 'lists.json'), root)}")
