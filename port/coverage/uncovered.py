"""Show which lines of in-scope source files a coverage run did not reach.

usage: python3 -I port/coverage/uncovered.py <label> <path-regex> [--source]
  label       a run label (e.g. oracle, or a union like bb+oracle)
  path-regex  which source files, e.g. 'core/src/session/compaction' or 'core/src/(shell|permission)'
  --source    print the uncovered source lines, not just line ranges
Files are matched among in-scope files (port/coverage/report.py's scope) at the label's commit.
"""
import os
import re
import sys

sys.argv = [sys.argv[0]] + sys.argv[1:]
label, pattern = sys.argv[1], re.compile(sys.argv[2])
show_source = "--source" in sys.argv
here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, here)
argv, sys.argv = sys.argv, [sys.argv[0], label]  # report.py reads labels from argv at import
import io, contextlib  # noqa: E401
with contextlib.redirect_stdout(io.StringIO()):
    import report  # noqa: E402  (computes coverage for the label on import)
sys.argv = argv

lines, _ = report.covs[label]
for f in sorted(f for f in report.scope if pattern.search(f)):
    measured = lines.get(f, {})
    missing = sorted(n for n, h in measured.items() if h == 0)
    if not measured:
        print(f"{f}: never loaded")
        continue
    pct = 100 * (len(measured) - len(missing)) / len(measured)
    ranges, start, prev = [], None, None
    for n in missing:
        if start is None:
            start = prev = n
        elif n == prev + 1:
            prev = n
        else:
            ranges.append((start, prev))
            start = prev = n
    if start is not None:
        ranges.append((start, prev))
    print(f"{f}: {pct:.1f}% of {len(measured)} lines; uncovered: " + ", ".join(f"{a}" if a == b else f"{a}-{b}" for a, b in ranges))
    if show_source and missing:
        source = report.git("show", f"{report.ref}:{f}").splitlines()
        for a, b in ranges:
            for n in range(a, b + 1):
                print(f"  {n:5}  {source[n - 1] if n - 1 < len(source) else ''}")
