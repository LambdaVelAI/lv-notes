# Test corpus and the look-and-feel loop

Real Jupyter Book and MyST projects for testing how lv-notes imports and
renders documents with math, figures, cross-references and citations, plus
a plan for an agent loop that improves the rendering.

## Files

| File | What it is |
| --- | --- |
| `corpus.toml` | The 14 projects: repo, ref, book folder, format (`myst` or `jb1`), tier, what each tests |
| `fetch.py` | Downloads them and reports their features (Python 3.11+, standard library, git) |
| `PLAN.md` | How to drive, observe and judge the UI automatically, and the order to build it in |
| `checkouts/`, `runs/` | Created by the scripts; git-ignored |

## Fetching

```sh
python3 tools/corpus/fetch.py                  # everything, ~700 MB, under a minute
python3 tools/corpus/fetch.py --tier 1         # two small books
python3 tools/corpus/fetch.py pbdl mystmd-docs # by name
python3 tools/corpus/fetch.py --report-only    # rescan what's on disk
```

`fetch.py` makes shallow, sparse checkouts of just each book's folder in
`checkouts/<name>/`. It writes two files:

- `checkouts/lock.json`: the commit fetched for each project;
- `checkouts/REPORT.md`: counts of display and inline math, labels, figures,
  tables, references, citations, admonitions, proofs, exercises and other
  directives.

The counts are regex estimates. Use them to pick test cases, not as truth.

Tiers:

- **1**, smoke tests: `mystmd-quickstart`, `quantecon-mini`;
- **2**, feature coverage: `mystmd-docs`, `jupyter-book-docs`,
  `scipy-proceedings-2026`, `sphinx-proof-docs`, `pbdl`,
  `climate-laboratory`, `teachbooks-manual`, `pythia-foundations`;
- **3**, scale: `quantecon-intro`, `quantecon-python` (about 6,000 display
  equations and 3,000 references), `inferential-thinking`, `turing-way`.

Formats:

- `jb1`: Jupyter Book 1 (`_config.yml` + `_toc.yml`);
- `myst`: Jupyter Book 2 / mystmd (`myst.yml`).

The importer needs to handle both.

To add a project, append a `[[project]]` entry to `corpus.toml` and rerun.

## For a coding session

`PLAN.md` describes the loop:

1. Import a corpus project.
2. Open every document in the browser build with Playwright.
3. Record screenshots, the ARIA snapshot and deterministic checks.
4. Compare against the project's own reference build.
5. Fix the worst issue, rerun, keep or revert.

Start with its "Order of work":

1. Add `data-node-id` / `data-kind` to block rendering.
2. Write a dev-only HTTP bridge over `Session`, plus an `httpBackend`, so the
   browser build uses the real core and its import.
3. Write `ui-run`: screenshots, ARIA snapshot and checks per document, and a
   ranked `summary.md`.
4. Add reference builds and the side-by-side comparison.
5. Add screenshot baselines, and a small macOS WebdriverIO suite for
   shell-only behavior.
