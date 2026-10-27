# Desktop shell and editor (layer 2, first cut)

Status: implemented (Tauri 2 + ProseMirror), not yet released.

## Shape

```
apps/desktop/
  src/                 web UI (TypeScript, no framework)
    editor/            ProseMirror: MyST schema, conversion, IDs, keys
    backend.ts         Tauri commands, or an in-memory stand-in for previews
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
| `create_project(path, name)`, `open_project(path)`, `open_last_project()` | Opens a `.lv` file, registering the local person as owner if needed |
| `create_document(title)` | Imports a root with one empty paragraph |
| `load_document(doc)` | Returns `{version, title, myst}`, with `myst` as native MyST (IDs included) |
| `save_document(doc, base, myst)` | Reconciles the tree into ops (`lv_core::convert::update_document`) and submits one batch |
| `rename_document`, `delete_document` | One op each |

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
  restored by undo) or its type changed.

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
  implicit paragraphs, ID assignment and renames.

## Still open

- **Concurrent edits are refused, not merged.** `STALE_BASE` is right while
  the editor is the only writer. Once the in-app agent edits the same
  document (layer 3), it needs real merging: the text-CRDT question.
- **Changing a block's type loses its ID.** `set_attrs` cannot change
  `type`, so paragraph → heading is delete plus insert, and anything
  anchored to the old ID (comments, backlinks by ID) is lost. A
  `set_type` op would fix this.
- **Edited blocks drop text-run details.** Source positions on their text,
  unmodelled fields on text runs and empty text nodes are lost. Mark
  nesting is normalized to schema order (link > strong > emphasis > …).
- **Not editable yet:** links (they show and save, but there's no UI to
  add or edit one), cross-references, citations, directives, tables and
  figures.
- **Images:** relative URLs don't resolve to project assets.
- **Cost per save** is O(document): the whole tree is serialized and
  reconciled. That is fine for notes; long drafts may want per-block saves.
- **Anyone who opens a file becomes an owner** (v1 is single-user), and
  the local identity is a UUID in the app's settings file.
- **Not checked in a real window yet.** The UI was checked in a browser
  with the in-memory stand-in, and the commands through the mock IPC. The
  built app launches cleanly, but nobody has used it end to end in its
  own window.
