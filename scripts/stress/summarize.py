#!/usr/bin/env python3
"""Tabulates a stress run (scripts/stress/run-mac.sh, run-ios.sh): the scenarios'
metrics from metrics.jsonl, and from each Time Profiler trace where the CPU time went:
by component (our crates, wgpu, the Metal driver, SwiftUI, the system) and the
hottest functions, on the main thread and off it. Writes summary.md beside them.

    python3 -I scripts/stress/summarize.py target/stress/results/mac-…
"""

import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path

def demangle(name):
    """Rust's v0 symbols (_RNv…) as a readable path: the identifiers in order, with
    the crate hashes and generic noise left out. Close enough to tell functions apart."""
    if not name.startswith("_R"):
        return name
    parts, i = [], 2
    while i < len(name):
        m = re.match(r"(\d+)(_?)", name[i:])
        if not m:
            i += 1
            continue
        n = int(m.group(1))
        start = i + len(m.group(0))
        ident = name[start:start + n]
        if n and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", ident) and not re.fullmatch(r"[0-9a-f]{8,}", ident):
            parts.append(ident)
            i = start + n
        else:
            i += 1
    # Drop consecutive repeats (trait impl paths repeat the type).
    out = [p for k, p in enumerate(parts) if k == 0 or p != parts[k - 1]]
    return "::".join(out) or name


# Which component a frame belongs to, by its symbol (first match wins) or binary.
COMPONENTS = [
    ("libviewer", ["libviewer"]),
    ("libgui", ["libgui"]),
    ("text shaping (harfrust/swash/fonts)", ["harfrust", "swash", "skrifa", "read_fonts", "ttf_parser", "fontdue", "rustybuzz", "hb_"]),
    ("rendererx", ["rendererx"]),
    ("wgpu/naga", ["wgpu", "naga::"]),
    ("pdf (pdfkit/mupdf)", ["libfileparser_pdfkit", "libfileparser_mupdf", "fz_", "pdf_", "CGPDF", "PDFKit", "template_span", "cf2_", "FT_", "ft_", "sort_active", "advance_active"]),
    ("zlib/deflate", ["inflate", "adler32", "crc32", "deflate"]),
    ("image decode", ["image::", "png::", "zune", "jpeg", "fdeflate", "miniz", "flate2", "ImageIO", "AppleJPEG", "PNG", "vImage"]),
    ("3d readers", ["libfileparser_3d", "gltf_lite", "gltf::", "usd_lite", "blend_lite", "scene_data", "draco"]),
    ("document readers", ["libfileparser_docs", "docx_lite", "office_oxide", "markup_lite", "pulldown", "comrak", "quick_xml", "roxmltree"]),
    ("data/text readers", ["libfileparser_data", "libfileparser_text", "libfileparser_sheets", "serde_json", "calamine", "csv"]),
    ("viewer-core/model/surface/ffi", ["viewer_core", "viewer_model", "viewer_surface", "viewer_ffi", "uniffi"]),
    ("Rust std/core", ["core::", "alloc::", "std::", "hashbrown"]),
    ("Swift runtime", ["swift_", "Swift"]),
    ("allocator/memcpy", ["malloc", "free", "_platform_mem", "memmove", "memcpy", "memset", "realloc", "nanov2", "szone", "xzm"]),
    ("Metal driver (AGX)", ["AGX", "IOGPU", "MTL", "Metal"]),
    ("SwiftUI/AppKit/UIKit", ["SwiftUI", "AppKit", "UIKit", "AttributeGraph", "CoreAnimation", "CA::", "QuartzCore"]),
]


def component(name, binary):
    for label, keys in COMPONENTS:
        if any(k in name for k in keys):
            return label
    if binary in ("Viewer",):
        return "app (other)"
    return f"system ({binary})" if binary else "unknown"


def load_metrics(path):
    lines = []
    for raw in path.read_text().splitlines() if path.exists() else []:
        if raw.startswith("viewer-stress: "):
            lines.append(json.loads(raw[len("viewer-stress: "):]))
    return lines


def parse_trace(trace):
    """(per-sample (thread, frames[(name, binary)], weight)) from a trace's time profile."""
    try:
        toc = subprocess.run(["xcrun", "xctrace", "export", "--input", str(trace), "--toc"], capture_output=True, text=True, timeout=300).stdout
        schema = "time-profile" if 'schema="time-profile"' in toc else "time-sample"
        xml = subprocess.run(
            ["xcrun", "xctrace", "export", "--input", str(trace), "--xpath", f'/trace-toc/run[@number="1"]/data/table[@schema="{schema}"]'],
            capture_output=True, timeout=600,
        ).stdout
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return []
    if not xml:
        return []
    root = ET.fromstring(xml)
    ids = {}

    def resolve(el):
        if el is None:
            return None
        ref = el.get("ref")
        return ids.get(ref, el) if ref else el

    for el in root.iter():
        if el.get("id"):
            ids[el.get("id")] = el
    samples = []
    for row in root.iter("row"):
        thread = resolve(row.find("thread"))
        tname = thread.get("fmt", "") if thread is not None else ""
        weight_el = resolve(row.find("weight"))
        weight = int(weight_el.text) if weight_el is not None and weight_el.text and weight_el.text.isdigit() else 1_000_000
        state = resolve(row.find("thread-state"))
        if state is not None and state.get("fmt", "Running") != "Running":
            continue
        bt = resolve(row.find("tagged-backtrace"))
        if bt is None:
            bt = resolve(row.find("backtrace"))
        frames = []
        if bt is not None:
            for fr in bt.findall("frame"):
                fr = resolve(fr)
                b = resolve(fr.find("binary"))
                frames.append((demangle(fr.get("name", "?")), b.get("name", "") if b is not None else ""))
        samples.append((tname, frames, weight))
    return samples


def profile_tables(samples, top=12):
    total = sum(w for _, _, w in samples) or 1
    comp_self = Counter()
    self_main, self_other = Counter(), Counter()
    incl = Counter()
    own = Counter()
    main_total = 0
    for tname, frames, w in samples:
        if not frames:
            continue
        name, binary = frames[0]
        comp_self[component(name, binary)] += w
        is_main = "Main Thread" in tname or "main" == tname.split(" ")[0].lower()
        if is_main:
            main_total += w
            self_main[name] += w
        else:
            self_other[name] += w
        for n in {f[0] for f in frames}:
            incl[n] += w
        # Charged to the first frame of the app's own code (Swift and every Rust crate
        # and C library linked into it), past the system's: who asked for that time.
        mine = next(((n, b) for n, b in frames if b == "Viewer" and not n.startswith("<") and "$main" not in n and n not in ("main", "start")), None)
        own[(mine[0] if mine else f"(system only) {frames[0][1]}", "main" if is_main else "other")] += w
    pct = lambda w: f"{100 * w / total:.1f}%"
    out = [f"CPU samples: {total / 1e6:.0f} ms ({100 * main_total / total:.0f}% on the main thread)\n"]
    out.append("| Component (self time) | Share |\n|---|---|")
    out += [f"| {c} | {pct(w)} |" for c, w in comp_self.most_common(10)]
    out.append("\nCharged to the app's own code (first own frame, self time below it included):\n\n| Function | Thread | Share of all |\n|---|---|---|")
    out += [f"| `{n[:110]}` | {t} | {pct(w)} |" for (n, t), w in own.most_common(top + 6)]
    for title, counter in (("Main thread, hottest functions (self)", self_main), ("Other threads, hottest functions (self)", self_other)):
        out.append(f"\n{title}:\n\n| Function | Share of all |\n|---|---|")
        out += [f"| `{n[:110]}` | {pct(w)} |" for n, w in counter.most_common(top)]
    return "\n".join(out)


COLUMNS = ["scenario", "interface", "open_ms", "fps", "draw_p50_ms", "draw_p95_ms", "draw_max_ms", "draws_over_16ms",
           "cpu_pct", "power_mw", "gpu_ms", "wakeups_per_s", "mem_peak_mb", "mem_end_mb"]


def metrics_table(lines):
    rows = [l for l in lines if "scenario" in l and "error" not in l]
    out = ["| " + " | ".join(COLUMNS) + " |", "|" + "---|" * len(COLUMNS)]
    for r in rows:
        out.append("| " + " | ".join(str(r.get(c, "")) for c in COLUMNS) + " |")
    errors = [l for l in lines if "error" in l]
    if errors:
        out.append("\nErrors: " + "; ".join(f"{e['scenario']}: {e['error']}" for e in errors))
    extras = [r for r in rows if any(k in r for k in ("fetches", "mem_every_10_rounds_mb", "mem_after_each_round_mb", "open_ms_worst"))]
    for r in extras:
        keep = {k: v for k, v in r.items() if k in ("fetches", "fetch_us_avg", "fetch_max_ms", "kind", "mem_every_10_rounds_mb", "mem_after_each_round_mb", "open_ms_worst")}
        out.append(f"\n{r['scenario']}: `{json.dumps(keep)}`")
    return "\n".join(out)


def main():
    run = Path(sys.argv[1])
    lines = load_metrics(run / "metrics.jsonl")
    doc = [f"# Stress run {run.name}\n", "## Metrics (clean run)\n", metrics_table(lines)]
    traces = sorted((run / "traces").glob("*.trace"))
    if traces:
        doc.append("\n## Where the CPU time went (Time Profiler, per scenario)\n")
    for t in traces:
        samples = parse_trace(t)
        doc.append(f"\n### {t.stem}\n")
        doc.append(profile_tables(samples) if samples else "(no samples)")
    (run / "summary.md").write_text("\n".join(doc) + "\n")
    print(f"wrote {run / 'summary.md'}")


if __name__ == "__main__":
    main()
