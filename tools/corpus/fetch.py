#!/usr/bin/env python3
"""Fetch the test corpus listed in corpus.toml and report what each project contains.

Each project is a shallow, sparse git checkout of just its book folder, under
checkouts/<name>/ next to this script (git-ignored). Re-running updates
existing checkouts. Afterwards it writes:

  checkouts/lock.json   the commit fetched for each project, so a run is reproducible
  checkouts/REPORT.md   per-project counts of the features the app has to render

Standard library only (Python 3.11+ for tomllib); needs git.

  python3 tools/corpus/fetch.py                 # everything
  python3 tools/corpus/fetch.py --tier 1        # tiers up to 1
  python3 tools/corpus/fetch.py pbdl mystmd-docs
  python3 tools/corpus/fetch.py --report-only   # rescan without fetching
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
MANIFEST = HERE / "corpus.toml"
OUT = HERE / "checkouts"

IMAGE_EXT = {".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".pdf"}

# Feature patterns, counted over .md files and the markdown cells of notebooks.
FEATURES: dict[str, re.Pattern[str]] = {
    "display_math": re.compile(r"^\$\$|^```\{math\}|^:::\{math\}|\\begin\{(equation|align|gather)", re.M),
    "inline_math": re.compile(r"(?<![\\$])\$[^$\n]+\$(?!\$)|\{math\}`"),
    "labels": re.compile(r"^\(([\w:.-]+)\)=\s*$|^:label:\s*\S+", re.M),
    "figures": re.compile(r"^(```|:::)\{(figure|image)\}|!\[[^\]]*\]\(", re.M),
    "tables": re.compile(r"^(```|:::)\{(list-table|table|csv-table)\}|^\|.*\|\s*$", re.M),
    "refs": re.compile(r"\{(ref|numref|eq|doc|prf:ref|term)\}`|\]\(#[^)]+\)"),
    "citations": re.compile(r"\{cite[:\w]*\}`|\[@[\w:-]+|(?<![\w@])@[A-Za-z][\w:-]*\d{2,}"),
    "admonitions": re.compile(r"^(```|:::)\{(note|tip|warning|important|caution|danger|attention|hint|seealso|admonition|dropdown)\}", re.M),
    "proofs": re.compile(r"^(```|:::)\{prf:\w+\}", re.M),
    "exercises": re.compile(r"^(```|:::)\{(exercise|solution)", re.M),
    "code_cells": re.compile(r"^(```|:::)\{code-cell\}", re.M),
    "other_directives": re.compile(r"^(```|:::)\{(?!math|figure|image|list-table|table|csv-table|note|tip|warning|important|caution|danger|attention|hint|seealso|admonition|dropdown|prf:|exercise|solution|code-cell)[\w:-]+\}", re.M),
}


@dataclass
class Project:
    name: str
    repo: str
    format: str
    tier: int
    tests: str = ""
    path: str = ""
    ref: str | None = None

    @property
    def dir(self) -> Path:
        return OUT / self.name

    @property
    def book(self) -> Path:
        return self.dir / self.path if self.path else self.dir


@dataclass
class Scan:
    md: int = 0
    ipynb: int = 0
    images: int = 0
    notebook_outputs: int = 0
    config: list[str] = field(default_factory=list)
    counts: dict[str, int] = field(default_factory=dict)
    directive_names: dict[str, int] = field(default_factory=dict)


def load_manifest() -> list[Project]:
    with MANIFEST.open("rb") as f:
        data = tomllib.load(f)
    return [Project(**p) for p in data["project"]]


def git(*args: str, cwd: Path | None = None) -> str:
    env = {**os.environ, "GIT_LFS_SKIP_SMUDGE": "1"}  # LFS assets come down as pointer stubs
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout.strip()


def fetch(p: Project) -> str:
    """Clone or update one project; returns the commit SHA."""
    url = f"https://github.com/{p.repo}"
    if not (p.dir / ".git").exists():
        p.dir.parent.mkdir(parents=True, exist_ok=True)
        args = ["clone", "--depth", "1", "--filter=blob:none", "--no-checkout"]
        if p.ref:
            args += ["--branch", p.ref]
        git(*args, url, str(p.dir))
        if p.path:
            git("sparse-checkout", "set", "--no-cone", f"/{p.path}/", "/*.yml", "/*.toml", "/*.bib", cwd=p.dir)
        git("checkout", cwd=p.dir)
    else:
        git("fetch", "--depth", "1", "origin", p.ref or "HEAD", cwd=p.dir)
        git("reset", "--hard", "FETCH_HEAD", cwd=p.dir)
    if not p.book.exists():
        raise RuntimeError(f"path '{p.path}' not found in {p.repo}")
    return git("rev-parse", "HEAD", cwd=p.dir)


def markdown_of(path: Path, scan: Scan) -> str:
    if path.suffix == ".md":
        return path.read_text(errors="replace")
    try:
        nb = json.loads(path.read_text(errors="replace"))
    except json.JSONDecodeError:
        return ""
    parts = []
    for cell in nb.get("cells", []):
        src = cell.get("source", "")
        src = "".join(src) if isinstance(src, list) else src
        if cell.get("cell_type") == "markdown":
            parts.append(src)
        elif cell.get("cell_type") == "code" and cell.get("outputs"):
            scan.notebook_outputs += 1
    return "\n\n".join(parts)


def scan(p: Project) -> Scan:
    s = Scan(counts={k: 0 for k in FEATURES})
    root = p.book
    for name in ("myst.yml", "_config.yml", "_toc.yml", "conf.py"):
        if (root / name).exists():
            s.config.append(name)
    for f in root.rglob("*"):
        if not f.is_file() or any(part.startswith((".", "_build")) for part in f.relative_to(root).parts):
            continue
        ext = f.suffix.lower()
        if ext in IMAGE_EXT:
            s.images += 1
        if ext not in (".md", ".ipynb"):
            continue
        if ext == ".md":
            s.md += 1
        else:
            s.ipynb += 1
        text = markdown_of(f, s)
        for k, rx in FEATURES.items():
            s.counts[k] += len(rx.findall(text))
        for m in re.finditer(r"^(?:```|:::)\{([\w:-]+)\}", text, re.M):
            s.directive_names[m.group(1)] = s.directive_names.get(m.group(1), 0) + 1
    return s


def report(projects: list[Project], scans: dict[str, Scan], lock: dict[str, dict]) -> str:
    cols = list(FEATURES)
    head = ["project", "tier", "fmt", "md", "ipynb", "imgs", "outputs", *cols]
    lines = [
        "# Corpus report",
        "",
        "Generated by `tools/corpus/fetch.py`. Counts are regex estimates over Markdown and notebook",
        "markdown cells: good for choosing test cases, not exact.",
        "",
        "| " + " | ".join(head) + " |",
        "|" + "---|" * len(head),
    ]
    for p in projects:
        s = scans.get(p.name)
        if not s:
            continue
        row = [p.name, str(p.tier), p.format, str(s.md), str(s.ipynb), str(s.images), str(s.notebook_outputs)]
        row += [str(s.counts[c]) for c in cols]
        lines.append("| " + " | ".join(row) + " |")
    lines += ["", "## Projects", ""]
    for p in projects:
        s = scans.get(p.name)
        if not s:
            continue
        top = sorted(s.directive_names.items(), key=lambda kv: -kv[1])[:15]
        sha = lock.get(p.name, {}).get("commit", "?")[:10]
        lines += [
            f"### {p.name}",
            f"- `{p.repo}`{'/' + p.path if p.path else ''} @ `{sha}` ({p.format}; config: {', '.join(s.config) or 'none found'})",
            f"- Exercises: {p.tests}",
            f"- Directives: {', '.join(f'{n} ({c})' for n, c in top) or 'none'}",
            "",
        ]
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("names", nargs="*", help="project names from corpus.toml (default: all)")
    ap.add_argument("--tier", type=int, help="only projects with tier <= this")
    ap.add_argument("--report-only", action="store_true", help="rescan existing checkouts without fetching")
    ap.add_argument("-j", "--jobs", type=int, default=4, help="parallel clones (default 4)")
    args = ap.parse_args()

    projects = load_manifest()
    if args.names:
        unknown = set(args.names) - {p.name for p in projects}
        if unknown:
            ap.error(f"unknown project(s): {', '.join(sorted(unknown))}")
        projects = [p for p in projects if p.name in args.names]
    if args.tier:
        projects = [p for p in projects if p.tier <= args.tier]

    OUT.mkdir(exist_ok=True)
    lock_path = OUT / "lock.json"
    lock: dict[str, dict] = json.loads(lock_path.read_text()) if lock_path.exists() else {}
    failed: list[str] = []

    if not args.report_only:
        def one(p: Project):
            try:
                return p, fetch(p), None
            except Exception as e:  # report and carry on with the rest
                return p, None, e

        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for p, sha, err in pool.map(one, projects):
                if err:
                    failed.append(p.name)
                    print(f"  FAIL {p.name}: {err}", file=sys.stderr)
                else:
                    lock[p.name] = {"repo": p.repo, "ref": p.ref, "path": p.path, "commit": sha}
                    print(f"  ok   {p.name} @ {sha[:10]}")
        lock_path.write_text(json.dumps(lock, indent=2, sort_keys=True) + "\n")

    # The report covers every checkout on disk, not just this run's selection.
    everything = load_manifest()
    scans = {p.name: scan(p) for p in everything if p.book.exists()}
    (OUT / "REPORT.md").write_text(report(everything, scans, lock))
    print(f"\nWrote {OUT / 'REPORT.md'}" + (f"; {len(failed)} failed: {', '.join(failed)}" if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
