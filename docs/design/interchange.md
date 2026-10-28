# Interchange: Jupyter Book 2 and Obsidian

Status: implemented in `crates/lv-core` (`markdown.rs`, `import.rs`,
`export.rs`) and the desktop app's Import… / Export… menus.

A project can leave as a folder of Markdown and come back. That makes alpha
files safe to depend on, and lets people bring existing notes in. Both
formats go both ways, and a round trip keeps the IDs of linked blocks, so
links made before an export still work after re-importing it.

## Formats

| | Jupyter Book 2 | Obsidian vault |
| --- | --- | --- |
| Files | `<slug>.md` per note, plus `myst.yml` (title, table of contents, `book-theme`) | `<Title>.md` per note |
| Title | `title:` in frontmatter | the file name |
| Note link | `[text](other.md)` | `[[Other]]`, `[[Other\|text]]` |
| Block link | `[text](other.md#label)`; a block without a label gets a `(node-<uuid>)=` target | `[[Other#^n-<uuid>\|text]]`, and the block ends with `^n-<uuid>`; headings link by text, `[[Other#Heading]]` |
| Admonitions, theorems | ```` ```{note} ````, ```` ```{prf:theorem} ```` | callouts: `> [!note]`, `> [!theorem]` |
| Sub/superscript, underline | `{sub}`…`` roles | `<sub>…</sub>` |
| Targets, comments, block breaks | `(label)=`, `% …`, `+++` | the same inside `%% … %%`, which Obsidian hides |
| Other directives and roles | MyST syntax | MyST syntax (Obsidian shows it as code) |

## Reading Markdown

`markdown::parse` runs markdown-rs (CommonMark, GFM, math, frontmatter)
and turns the MyST and Obsidian syntax it leaves as text or code into MyST
nodes: directives (including `:::` colon fences), roles, targets, comments,
block breaks, citations (`@key`, `[see @a, p. 2; @b]`), `[x](#label)`
cross-references, callouts, wikilinks and `^block-ids`. Headings in MyST
files get mystmd's implicit label (a slug of their text). What it doesn't
recognize stays as text or code: nothing is dropped.

`import::import_folder` reads a whole folder in one batch:

- **Order:** a book's `myst.yml` table of contents, then the remaining
  files by path. Subfolders are flattened (there are no folders in a
  project yet).
- **IDs:** every block gets one. `(node-<uuid>)=` and `^n-<uuid>` (from our
  own exports) restore the block's old ID, unless the project already uses
  it; then the block gets a new one, so importing twice makes a copy.
- **Links** become ID links: wikilinks by file name (case-insensitive, with
  or without the folder), `#Heading` by heading text, `#^id` by block ID,
  and relative `.md` links by path and label. A wikilink to a note that
  isn't in the folder stays as text, and is counted as unresolved.
- **Kinds** the project doesn't know (a custom callout type) are declared
  provisional, like any import (`registry.md`).
- **Skipped:** notebooks (`.ipynb`) are reported, not imported. Hidden
  folders (`.obsidian`, `.git`), `_build` and files over 5 MB are not read.

## Tests

`tests/interchange.rs`: every fixture plus linked notes go app → book → new
project → book, and the same for Obsidian; the second export must equal the
first, and the linked paragraph must keep its ID with both links to it. A
hand-written vault (folders, aliases, heading and block links, a missing
note, an embed, callouts) and a hand-written book (nested table of
contents, relative links, a notebook) check import on its own.

## Still open

- **Notebooks** are skipped.
- **Attachments** (images, PDFs) are not copied, and embeds (`![[x]]`)
  stay as text.
- **Tags** (`#tag`) and Obsidian properties beyond frontmatter are kept as
  text and frontmatter, not modelled.
- **Folders** are flattened on import and not recreated on export.
- **Lossy spots:** an admonition's `class` doesn't survive Obsidian, and a
  heading's ID doesn't survive a round trip through either format
  (headings are linked by label or text).
- **Jupyter Book 1** (`_config.yml`, `_toc.yml`) is not supported.
