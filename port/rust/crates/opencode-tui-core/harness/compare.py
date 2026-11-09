"""Compares the TS and Rust outputs of one harness mode, record by record; exits 1 on any difference.

usage: python3 compare.py <ts output> <rust output> [shown=5]
A record starts at a line with a kind and a name (`dump NAME ...`, `frame NAME ...`, `setup NAME ...`) and runs
to the next one.
"""
import sys


def records(path):
    out = []
    with open(path, encoding="utf-8") as handle:
        for line in handle.read().splitlines():
            if not line:
                continue
            head = line.split(" ", 2)
            if head[0] in ("dump", "frame", "setup", "error"):
                out.append([line])
            elif out:
                out[-1].append(line)
            else:
                out.append([line])
    return out


def hexdiff(a, b):
    """For hex-encoded byte records: the first differing offset, with both sides around it as text."""
    try:
        x = bytes.fromhex(a.split(" ", 2)[2])
        y = bytes.fromhex(b.split(" ", 2)[2])
    except (ValueError, IndexError):
        return None
    i = next((i for i in range(min(len(x), len(y))) if x[i] != y[i]), min(len(x), len(y)))
    show = lambda s: repr(s[max(0, i - 60) : i + 80].decode("utf-8", "replace"))
    return f"  first difference at byte {i} (ts {len(x)} bytes, rust {len(y)} bytes)\n    ts:   {show(x)}\n    rust: {show(y)}"


def main():
    ts, rust = records(sys.argv[1]), records(sys.argv[2])
    shown = int(sys.argv[3]) if len(sys.argv) > 3 else 5
    differences = 0
    if len(ts) != len(rust):
        print(f"record count differs: ts {len(ts)}, rust {len(rust)}")
        differences += 1
    for a, b in zip(ts, rust):
        if a == b:
            continue
        differences += 1
        if differences > shown:
            continue
        print(f"DIFF {a[0][:80]}")
        detail = hexdiff(a[0], b[0]) if len(a) == 1 and len(b) == 1 else None
        if detail:
            print(detail)
            continue
        for row, (x, y) in enumerate(zip(a[1:], b[1:])):
            if x != y:
                xs, ys = x.split(" "), y.split(" ")
                cells = [i for i, (p, q) in enumerate(zip(xs, ys)) if p != q]
                print(f"  row {row}: cells {cells[:8]}")
                for i in cells[:3]:
                    print(f"    ts   {xs[i]}\n    rust {ys[i]}")
    print(f"{len(ts)} records, {differences} differences")
    sys.exit(1 if differences else 0)


main()
