# Desktop shell and editor (layer 2, first cut)

Status: implemented (Tauri 2 + ProseMirror), not yet released.

## Shape

```
apps/desktop/
  src/                 web UI (TypeScript, no framework)
    editor/            ProseMirror: MyST schema, conversion, IDs, keys, links
    backend.ts         Tauri commands, or an in-memory stand-in for previews
    linking.ts         link picker, link bubble, backlinks panel
    main.ts            sidebar, title, toolbar, save loop
  src-tauri/           the `lv-desktop` crate
    src/session.rs     an open project: Store + Sequencer + the local grant
    src/lib.rs         Tauri commands over a Session
  test/                vitest: conversion, IDs, recorded editor traces
```

The editor code under `src/editor/` has no Tauri dependency, so a web app
can reuse it later.

## The client protocol, in-process

The UI never builds ops. It sends whole documents:

| Command | Does |
| --- | --- |
| `create_project(path, name)`, `open_project(path)`, `open_last_project()` | Opens a `.lv` file, registering the local person as owner and declaring the built-in vocabulary if needed |
| `create_document(title)` | Imports a root with one empty paragraph |
| `load_document(doc)` | Returns `{version, title, myst}`, with `myst` as native MyST (IDs included) |
| `save_document(doc, base, myst)` | Reconciles the tree into ops (`lv_core::convert::update_document`) and submits one batch |
| `rename_document`, `delete_document` | One op each |
| `backlinks(doc)` | Links pointing into `doc`, with the source note's title and the text of the block each link sits in |
| `search_blocks(text, limit?)` | Blocks whose text matches every word (as a prefix), best first |
| `search(text, limit?)` | The search screen: notes whose title matches, matching blocks (200 by default), and whether there were more |
| `locate(target)` | Where a `doc_…` / `node_…` link target is now, or null if it is gone |
| `registry()` | The project's vocabulary: document kinds, block kinds, relations (`registry.md`) |
| `export_project(dir, format)` | Writes every note into a new folder in `dir`, as a Jupyter Book (`book`) or an Obsidian vault (`obsidian`) |
| `import_folder(dir, format)` | Imports a Jupyter Book or Obsidian vault as new notes, in one batch |
| `export_document(doc, path)` | Writes one note as MyST Markdown to `path` |

After every accepted batch the app emits `project-changed {version,
documents}`. An editor with no unsaved edits reloads its document if
someone else changed it.

Errors reach the UI as `{code, message}`:

- `NO_PROJECT`;
- refusal codes (`UNKNOWN_DOCUMENT`, ...);
- `INVALID_DOCUMENT`: the tree isn't MyST, or has missing or duplicate IDs;
- `STALE_BASE`: the document's content changed after `base`. The editor
  reloads rather than overwriting.

### Why whole trees

- **Diffing lives in the core.** `update_document` compares the tree with
  the document by node ID and returns the minimal ops:
  - nodes that stayed in order don't move;
  - typing is one `edit_text`;
  - a tree that is already saved produces no ops.

  It is Rust, so it is tested in Rust (including a seeded random-edit
  test), and every client (web, iPad, agents) gets the same behavior.
- **Untouched nodes cost nothing.** On load, the editor remembers the MyST
  each ProseMirror node came from. ProseMirror nodes are immutable and
  shared between versions, so an untouched node converts back to exactly
  its loaded MyST, source positions and unmodelled fields included. Only
  edited nodes are rebuilt from the editor model, and only they produce
  ops.

## Node IDs come from the editor

Every node except text runs carries an ID the editor assigns (UUIDv7 in
lv-core's `node_…` form), so a node has one identity from the moment it is
typed.

- An `appendTransaction` plugin gives an ID to new nodes, and a fresh one
  to the second copy when a split or paste duplicates one. Links (a mark
  in the editor, a node in MyST) and nodes inside opaque MyST are covered
  too.
- An ID the core can't reuse is reported back in `renamed`, and the editor
  applies it. This happens when the node was deleted (for example,
  restored by undo).
- Changing a block's type (the block menu, `# `, ```` ``` ````) keeps its
  ID: the editor's `setBlockType` and type input rules carry `id` and
  `extra` over, and the core turns the change into one `set_type`. Links
  to the block keep working.

## The schema

- **Editable:**
  - blocks: `block`, `paragraph`, `heading`, `blockquote`, `list`,
    `listItem`, `code`, `math`, `thematicBreak`, `image`, `admonition`;
  - inline marks: strong, emphasis, underline, delete, subscript,
    superscript, inline code, link;
  - inline math (`inlineMath`, an atom whose `value` is the LaTeX) and hard
    breaks.
- **Opaque:** everything else (directives, figures, tables, footnotes,
  citations, cross-references, roles, unknown types) is held
  verbatim in `opaque` or `opaqueInline` atoms, shown as labelled chips. It
  can be moved and deleted, and it saves back unchanged.
- **Implicit paragraphs:** phrasing directly inside a container (a tight
  list item) shows as a paragraph flagged `implicit`, and is unwrapped on
  save.

## Links and backlinks

A link to a note or a block is an ordinary MyST `link` whose `url` is the
target's ID: `doc_…` for a note, `node_…` for a block. It never holds a
title or a path, so renaming a note or moving a block (even into another
note) doesn't break it. Plain MyST export keeps the ID as the url. Web
links are links with any other url.

- **Graph.** The core's `edge` projection already reads the ID off the url
  into `target_doc` / `target_node`. `Store::document_backlinks(doc)` returns
  every live link, reference or citation that points at the document, at
  one of its live nodes by ID, or at one of its labels. A link to a block
  follows the block when it moves. Links start untyped; the link bubble's
  relation menu types them from the project's registry (`registry.md`).
- **Adding a link.** ⌘K, the toolbar's Link button, or typing `[[` opens
  the picker. It lists notes by title (from the project info, filtered
  locally) and blocks by text (`search_blocks`, FTS5 prefix match), and
  offers a web address when the query looks like one. With text selected,
  the selection becomes the link. With a bare cursor, the note's title or
  the block's text (shortened) is inserted as the link text. On an existing
  link, picking retargets it and keeps its node ID.
- **Following and editing.** With the cursor in a link, a bubble shows
  where it goes (the note title, or "note › block text", resolved with
  `locate` and cached until the next change), with Open, Edit and Remove.
  ⌘-click also opens it. Opening a block link opens its note, selects the
  block and briefly highlights it (a decoration, so ProseMirror doesn't undo
  it). Web and mail links (`http:`, `https:`, `mailto:` only) open in the
  system browser through Tauri's opener plugin.
- **Broken links.** On open and after every change, the editor resolves
  the note's distinct link targets (`locate`, cached) and struck-through
  the links whose target was deleted.
- **Backlinks panel.** Under the note, "Linked from …" groups links by
  source note and shows the text around each one. Clicking one opens the
  source note at the link. The panel refreshes on open and after every
  `project-changed`.
- The in-memory preview backend answers the same three queries by walking
  its stored trees.

## Search

The field at the top of the sidebar (⇧⌘F) searches the whole project as
you type. The main pane then shows the notes whose titles match, followed by
the matching blocks grouped by note, with the matching words marked and
long blocks cut to the part around the first match. The arrow keys move
through the blocks, Return or a click opens the note at that block (which
flashes, as when following a link), and Escape clears the field and goes
back to the open note. Clicking the field again with a query still in it
shows the results again. The results refresh when the project changes.

A word matches when a word in the text starts with it, so `measur`
finds *measurable*. A block matches when it contains every word, even when
the words fall in different text runs (`plain **bold**`).
`Store::search_blocks` asks FTS5 for all the words together in one run, and
those blocks rank first. It then asks for each word on its own and adds the
blocks that have every word somewhere. The link picker uses the same query.
Titles aren't in the FTS index; the session matches them directly against
the state.

## Import and export

The sidebar's **Export…** menu saves the open note as MyST Markdown, or
writes every note into a new subfolder named after the project
(`my-thesis`, then `my-thesis-2`, ...) as a Jupyter Book 2 project or an
Obsidian vault, so an export never overwrites anything. **Import…** reads a
Jupyter Book folder or an Obsidian vault into new notes, opens the first,
and reports skipped notebooks and links that point outside the folder. The
formats, and how links and IDs survive a round trip, are in
`interchange.md`; the shell only reads and writes files. The browser
preview can't import or export.

## Math

MyST's `inlineMath` and `math` nodes, rendered with KaTeX
(`editor/math.ts`). The LaTeX source is what the document stores and what
ops carry; the rendering is only presentation.

- **Inline math** is an atom showing the rendering. Clicking it or arrowing
  onto it selects it and opens a small source box under it, which updates
  the rendering as you type. Return, Escape, Tab, or arrowing past either
  end goes back to the text, and math left empty is removed. Typing `$x^2$`
  in text makes inline math: the closing `$` triggers it, and the source
  can't start or end with a space, so `$5 or $6` stays text. ⇧⌘E or the ∑
  button turns the selection into math, or inserts empty math to type into.
- **Display equations** are code-like blocks whose text is the source.
  Away from the cursor only the rendering shows; with the cursor inside, the
  source shows under it, with the LaTeX error (if any) and a label field.
  Clicking the rendering, or arrowing into it, puts the cursor in the
  source, which is hidden otherwise. `$$` and a space at the start of a
  paragraph, or Equation in the block menu, makes one, keeping the block's
  ID.
- **Labels** set MyST's `label` and `identifier` (normalized like the
  core's), so `{eq}` references and `(label)=`-style links resolve, and
  the link picker finds equations by their source like any block.
- **Numbers** are a CSS counter over the note's equations, skipping any
  with `enumerated: false`. They match mystmd's numbering for one note; a
  numbering projection across a document's parts can come later.
- **Errors:** LaTeX KaTeX can't parse shows as source in red, with the
  message, and saves unchanged.

## Tests

- `cargo test -p lv-desktop`:
  - Session behavior: save and reopen, stale bases, grants, cross-document
    moves.
  - `tests/ipc.rs`: drives every command through Tauri's IPC on the mock
    runtime, with the real capabilities and the UI's argument shapes.
  - `tests/editor_traces.rs`: replays trees recorded from real ProseMirror
    edits (`apps/desktop/test/traces/`) and checks each save loads back
    exactly. Regenerate the traces after changing the schema or conversion
    with `WRITE_TRACES=1 npm test`.
- `npm test`: conversion round trips on the core's fixtures, mark nesting,
  implicit paragraphs, ID assignment and renames; link commands (add,
  insert, retarget, remove, find by ID) and the preview backend's link
  queries. The editor traces include adding, retargeting and removing
  links. `test/search.test.ts`: highlighting, snippets, and the preview
  backend's search. `test/math.test.ts`: math conversion, labels, the
  `$…$` rule, and the math commands.

## Still open

- **Concurrent edits are refused, not merged.** `STALE_BASE` is right while
  the editor is the only writer. Once the in-app agent edits the same
  document (layer 3), it needs real merging: the text-CRDT question.
- **Edited blocks drop text-run details.** Source positions on their text,
  unmodelled fields on text runs and empty text nodes are lost. Mark
  nesting is normalized to schema order (link > strong > emphasis > …).
- **Not editable yet:** cross-references, citations, directives, tables
  and figures.
- **Wrapping or unwrapping** (into a list or quote) keeps the block's ID,
  but turning a list item into a paragraph replaces the item: links to the
  `listItem` itself dangle (links to the paragraph inside it don't).
- **Search splits on text runs.** A word that is split across formatting
  (`**slo**w`) matches only by its fragments, and code and math blocks
  match by their source. Search has no filters yet (by note, by block kind,
  by relation).
- **Math macros:** `\newcommand` works inside one equation, but there are
  no project-wide macros yet (CLAUDE.md: macros are project state). Nor is
  there a visual math editor; source with a live preview is the first cut.
- **Images:** relative URLs don't resolve to project assets.
- **Cost per save** is O(document): the whole tree is serialized and
  reconciled. That is fine for notes; long drafts may want per-block saves.
- **Anyone who opens a file becomes an owner** (v1 is single-user), and
  the local identity is a UUID in the app's settings file.
- **Not checked in a real window yet.** The UI was checked in a browser
  with the in-memory stand-in, and the commands through the mock IPC. The
  built app launches cleanly, but nobody has used it end to end in its
  own window.
