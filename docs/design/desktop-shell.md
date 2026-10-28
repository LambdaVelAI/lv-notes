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
| `locate(target)` | Where a `doc_…` / `node_…` link target is now, or null if it is gone |
| `registry()` | The project's vocabulary: document kinds, block kinds, relations (`registry.md`) |
| `export_project(dir)` | Writes every note as MyST Markdown into a new folder in `dir` |
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
  - hard breaks.
- **Opaque:** everything else (directives, figures, tables, footnotes,
  citations, cross-references, roles, inline math, unknown types) is held
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

## Export

"Export all…" in the sidebar asks for a folder and writes every note into
a new subfolder named after the project (`my-thesis`, then `my-thesis-2`,
...), so an export never overwrites anything. "Export note…" saves the open
note to a file the save dialog picks. Both use `lv_core::export`
(`docs/design/ops-and-ids.md`, section 6); the shell only writes the files.
The browser preview can't export.

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
  links.

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
- **Search covers text runs only.** A block whose words are split across
  formatting (`**slo**w`) matches by fragment, and code and math blocks
  match by their source.
- **Images:** relative URLs don't resolve to project assets.
- **Cost per save** is O(document): the whole tree is serialized and
  reconciled. That is fine for notes; long drafts may want per-block saves.
- **Anyone who opens a file becomes an owner** (v1 is single-user), and
  the local identity is a UUID in the app's settings file.
- **Not checked in a real window yet.** The UI was checked in a browser
  with the in-memory stand-in, and the commands through the mock IPC. The
  built app launches cleanly, but nobody has used it end to end in its
  own window.
