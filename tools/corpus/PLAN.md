# UI automation and the look-and-feel loop

Status: proposal. Not built yet, except the corpus in this directory.

## Goal

An agent (a Claude Code session) improves how the app renders real
documents, unattended, in a loop:

1. Import a real Jupyter Book.
2. Open every document and look at it.
3. Find what is wrong or ugly.
4. Change the code, then rerun and compare.

This needs three things: a way to **drive** the UI, a way to **observe** it,
and a way to **judge** what was observed.

## Corpus

`corpus.toml` lists about 14 public Jupyter Book / MyST projects
in three tiers:

- tier 1: smoke tests;
- tier 2: feature coverage;
- tier 3: scale.

`python3 tools/corpus/fetch.py` makes shallow, sparse checkouts under
`tools/corpus/checkouts/` (git-ignored, about 700 MB in total) and records the commits in
`checkouts/lock.json`. It also writes `checkouts/REPORT.md`, a regex estimate of
each project's features (math, labels, figures, references, citations,
directives) for picking test cases.

Some of the projects are Jupyter Book 1 (`_config.yml` + `_toc.yml`) and some
are mystmd (`myst.yml`). The importer has to handle both, so they are two
test paths.

## Drive: the browser build, with the real core behind it

The UI is plain web (ProseMirror, no framework), and `npm run dev` already
serves it in a browser. That makes **Playwright against the browser build**
the main harness:

- It runs headless on Linux, so it works in CI and in cloud agent sessions.
  The Tauri app doesn't run there.
- It is fast and parallel, and it captures screenshots, the accessibility
  tree, console messages and traces.

The gap is the backend:

- `memoryBackend` is a UI stand-in with no ops and no import.
- Import, reconcile and search live in Rust.

Two ways to close the gap:

- **(a) Dev bridge (recommended now).** A small dev-only binary serves the
  same commands as `src-tauri/src/lib.rs` over localhost HTTP, on top of
  `Session`. In the browser, `connect()` picks a `httpBackend` when
  `?backend=http://127.0.0.1:PORT` is set.
  - The harness gets the real core with real import.
  - It's a small amount of code: the commands are already a narrow table.
  - The bridge must be debug-only and bound to loopback.
- **(b) lv-core compiled to WebAssembly.** `CLAUDE.md` already asks for the
  core to stay portable, and a web app will need this eventually. It is more
  work up front: SQLite in WASM and file access.

The harness also needs test hooks that skip native dialogs, for example
`import_folder(dir, format)` called directly with a path.

### The real app, for shell-specific checks

The packaged Tauri app on macOS can be driven by WebdriverIO's
`@wdio/tauri-service` with the embedded provider
(`tauri-plugin-wdio-webdriver`, debug builds only). The official
`tauri-driver` doesn't support macOS because WKWebView has no WebDriver.

Keep this suite small, for things the browser can't show:

- menus;
- file open and save;
- the keychain;
- WKWebView-specific rendering (fonts, KaTeX in WebKit).

It runs on a Mac or a macOS CI runner, not in the agent loop.

### Interactive exploration

For an agent poking around by hand, add the Playwright MCP server to the
Claude Code session. The agent then navigates, clicks and takes screenshots
itself. Scripted runs cover the bulk; MCP is for following up on a specific
page.

## Observe: what a run records

`tools/ui-run` (Playwright, TypeScript) does the following for each corpus
project:

1. Imports the project.
2. Opens each document.
3. Records into `tools/corpus/runs/<run-id>/<project>/<doc>/`:
   - `page.png`: a full-page screenshot at a fixed viewport, theme and font
     set;
   - `blocks/*.png`: element screenshots of interesting blocks (each display
     equation, figure, table and directive), keyed by node ID so they line up
     across runs;
   - `a11y.yml`: Playwright's ARIA snapshot (structure an agent can read
     without pixels);
   - `checks.json`: deterministic checks (below), console errors, and load
     and render time.

Then it writes `summary.md`, with pages ranked worst first.

Deterministic checks, run in the page:

| Check | How |
| --- | --- |
| Math failed | `.katex-error` elements; raw `$` or `\begin` left in text nodes |
| Image broken | `img` with `naturalWidth == 0` |
| Reference unresolved | link targets that `locate()` returns null for; `??` numbering |
| Unknown kind shown | generic-fallback blocks, counted by kind |
| Content lost | imported node count vs MyST AST node count |
| Overflow | blocks wider than the column (`scrollWidth > clientWidth`) |
| Errors | console errors, uncaught exceptions |

Stable selectors help a lot: render `data-node-id` and `data-kind` on block
DOM nodes.

## Judge: three layers

1. **Checks**: the table above. These are pass/fail numbers, tracked per run.
   A change that makes them worse fails.
2. **Reference comparison.** Build each corpus project with its own tool
   (`jupyter book build --html` / `myst build --html`) once, and screenshot
   the same pages. The agent views ours next to the reference, page by page
   and block by block, and lists differences that matter. The reference
   answers "what should this look like?" without anyone writing a spec.
3. **Visual regression.** Once a page looks right, store its screenshots as
   baselines (Playwright `toHaveScreenshot`, with a tolerance). Later runs
   flag pixel changes for review instead of judging taste again.

Claude reads screenshots directly, so layer 2 is a prompt plus image pairs.
The useful discipline is to ask it for specific, located findings, for
example "eq. 3 in `lectures/kalman.md`: the equation number is missing; the
reference shows (3)". Don't ask for an overall verdict.

## The loop

```
fetch corpus (once) → build references (once)
repeat:
  ui-run --tier 2                      # checks + screenshots
  agent: read summary.md, pick the top issue,
         compare with the reference, fix, add a test
  ui-run (same pages) → compare with the previous run → keep or revert
```

- Each iteration is one issue, one commit, and one before/after pair in the
  commit message.
- Tier 3 runs nightly or before a release, for scale and performance.

## Order of work

1. Add `data-node-id` / `data-kind` to block rendering.
2. Dev HTTP bridge and `httpBackend`.
3. `ui-run` with screenshots, the ARIA snapshot and checks; `summary.md`.
4. Reference builds and the side-by-side view.
5. Baselines; the macOS WebdriverIO suite.
