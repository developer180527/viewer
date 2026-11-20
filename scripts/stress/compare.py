#!/usr/bin/env python3
"""Two stress runs side by side: each scenario's main figures, before → after, with
the change. For checking an optimization against the run before it.

    python3 -I scripts/stress/compare.py target/stress/results/mac-A target/stress/results/mac-B
"""

import json
import sys
from pathlib import Path

# (column, label, lower is better)
FIGURES = [
    ("mem_peak_mb", "peak MB", True),
    ("mem_end_mb", "end MB", True),
    ("cpu_pct", "CPU %", True),
    ("power_mw", "mW", True),
    ("draw_p50_ms", "draw p50", True),
    ("draw_p95_ms", "draw p95", True),
    ("draws_over_16ms", "over 16 ms", True),
    ("fps", "fps", False),
]


def load(run):
    out = {}
    path = Path(run) / "metrics.jsonl"
    for raw in path.read_text().splitlines() if path.exists() else []:
        if raw.startswith("viewer-stress: "):
            d = json.loads(raw[len("viewer-stress: "):])
            if "scenario" in d:
                out[d["scenario"]] = d
    return out


def cell(a, b, lower_better):
    if a is None and b is None:
        return ""
    if a is None or b is None:
        return f"{a if a is not None else '–'} → {b if b is not None else '–'}"
    if a == b:
        return f"{b}"
    better = (b < a) == lower_better
    pct = f" ({(b - a) / a * 100:+.0f}%)" if a else ""
    mark = "✓" if better else "✗"
    return f"{a} → {b}{pct} {mark}"


def main():
    before, after = load(sys.argv[1]), load(sys.argv[2])
    names = list(dict.fromkeys([*before, *after]))
    print("| scenario | " + " | ".join(l for _, l, _ in FIGURES) + " |")
    print("|---|" + "---|" * len(FIGURES))
    for n in names:
        a, b = before.get(n, {}), after.get(n, {})
        if "error" in a or "error" in b or not a or not b:
            print(f"| {n} | " + ("missing or failed in one run" if not (a and b) else "error") + " |" + " |" * (len(FIGURES) - 1))
            continue
        print(f"| {n} | " + " | ".join(cell(a.get(k), b.get(k), low) for k, _, low in FIGURES) + " |")


if __name__ == "__main__":
    main()
