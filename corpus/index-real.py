"""Writes corpus/real/README.md: every category and reader folder, how many files it
holds, how big, and which repositories they came from (each file's name starts with
its repository, `repo__path`; 3d/ keeps one folder per repository).

    python3 -I corpus/index-real.py
"""

import os

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "real")


def size(path):
    total = 0
    for dp, _, files in os.walk(path):
        for f in files:
            total += os.path.getsize(os.path.join(dp, f))
    return total


def human(n):
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


lines = ["# Real-world files", "",
         "Kept out of git; each file keeps its repository's licence. Fetched by `corpus/fetch-real.sh`",
         "(into `<category>/<reader>/`) and `corpus/fetch-real-3d.sh` (into `3d/<repository>/`).",
         "`<reader>-invalid/` folders hold test suites' broken-on-purpose files.", "",
         "```text",
         "LFP_REAL_CORPUS=$PWD/corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture",
         "LFP_REAL_3D=$PWD/corpus/real/3d cargo test --release -p libfileparser --test real_3d -- --ignored --nocapture",
         "```", ""]
for category in sorted(os.listdir(root)):
    cat = os.path.join(root, category)
    if not os.path.isdir(cat):
        continue
    lines += [f"## {category}/", "", "| Folder | Files | Size | From |", "|---|---|---|---|"]
    for folder in sorted(os.listdir(cat)):
        path = os.path.join(cat, folder)
        if not os.path.isdir(path):
            continue
        files = [f for dp, _, fs in os.walk(path) for f in fs]
        if category == "3d":
            sources = folder.replace("_", "/", 1)
        else:
            repos = sorted({f.split("__", 1)[0] for f in files if "__" in f})
            sources = ", ".join(repos[:6]) + (f" and {len(repos) - 6} more" if len(repos) > 6 else "")
        lines.append(f"| {folder} | {len(files)} | {human(size(path))} | {sources or '—'} |")
    lines.append("")
with open(os.path.join(root, "README.md"), "w") as out:
    out.write("\n".join(lines))
print("wrote", os.path.join(root, "README.md"))
