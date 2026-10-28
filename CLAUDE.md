# lv-notes

A note-taking and document authoring system for academics. Notes form a typed
knowledge graph, drafts grow out of notes, and people and the agents acting for
them work in the same documents, with every change attributable and reviewable.

Full plan (living doc): https://claude.ai/artifact/Sub9Ec6n5k4QiSf1FdkGYD

## Current target: v1

- **Local, single device.** A desktop app (macOS first). No servers, no sync
  between devices, no multi-user editing yet.
- **Human-agent collaboration is in v1.** An in-app agent edits the same
  documents through the same op layer as the editor. This is real concurrency
  and must be handled properly from the start.
- Only network dependency: the Claude API, using the user's own API key
  stored in the OS keychain.

## Architecture

- **Project = corpus = one SQLite file.** A project holds documents, the op
  log, the graph and the search index together. Export is a file copy.
- **The project is the unit** of consistency, versioning and permission.
  Cross-document ops (moving blocks between notes, renames that update
  backlinks, typed links) are atomic within a project.
- **Rust core** (workspace crate): MyST AST model, op log, sequencer, merge,
  projections (documents, graph, search), single permission check. It is a
  library used by every client and, later, by the server unchanged. Keep it
  portable; compiling it to WebAssembly later should stay possible.
- **Desktop shell: Tauri.** The UI is web: a ProseMirror editor with a MyST
  schema, plus native-feeling chrome. The same UI should later serve a web app.
- **Clients talk to the sequencer through a protocol**, even locally: submit
  ops with the version they were based on; the sequencer accepts or rebases
  and assigns a position. Locally the transport is in-process; later it is a
  websocket, and nothing else changes.
- **Agents are just clients.** The in-app agent and outside agents (via an MCP
  server the app exposes) use the same ops with a delegated grant. No back
  doors.
- **The project carries its own schema.** A registry, stored in the project
  and changed by ops, declares document kinds (note, article, deck), block
  kinds (theorem, lemma, ...) and edge types (cites, proves, ...) as data,
  not code. A project copy carries its own
  "installed extensions"; nothing depends on what a client has installed.
  Built-in kinds are pre-filled entries, not hard-coded.

## Layout and commands

- `crates/lv-core`: the core. `cargo test -p lv-core`.
- `apps/desktop`: the desktop app.
  - Web UI in `src/`, Tauri crate `lv-desktop` in `src-tauri/`.
  - The root `package.json` forwards to it, so these work from the root or
    from `apps/desktop`.
  - Setup: `npm install` and `npm run build` (the Rust crate embeds `dist/`).
  - Run: `npm run tauri dev`.
  - Browser preview with an in-memory backend: `npm run dev`.
  - Tests: `npm test`, and `cargo test -p lv-desktop`.
- Rust 1.90 or later (Tauri 2.12's minimum).
- Design notes: `docs/design/` (`ops-and-ids.md`, `desktop-shell.md`,
  `registry.md`, `interchange.md`).
- CI runs on every push to `main` (`.github/workflows/ci.yml`). Releases:
  push a `v*` tag; see `docs/release.md`.

## Invariants (do not compromise; retrofitting means migrating the op log)

1. Every op carries a principal: a person, or an agent acting for a person,
   plus the grant that authorized it.
2. Every document and node belongs to a project.
3. All permission checks go through one function, even while it returns true.
4. Nodes and blocks have stable, globally unique IDs. Ops address IDs, never
   paths or offsets. Never use auto-increment IDs for anything shared.
5. Every op records the version it was made against.
6. Ops can belong to a change set (author, base version, rationale), so
   suggestions, agent proposals and later offline edits share one mechanism.
7. Search and graph tables are projections, rebuildable from the op log.
   Derived data records the model and version that produced it.
8. Ops are validated against the project schema in one place in the core, like
   the permission check. Clients ask; they do not decide conformance.

## Document model

- MyST AST is the contract; Markdown is import, export and interchange.
- Graph edges are typed (cites, supports, contradicts, ...) from an extensible
  vocabulary. Links may start untyped and gain a type later, as a separate op.
- Conformance has three layers. Structure: a small fixed set of node types the
  core enforces. Meaning: `kind` values and edge types, checked against the
  project registry. Presentation: per client, always with a generic fallback,
  so an unfamiliar kind is shown plainly and never lost.
- Unknown kinds: Markdown import is lenient (preserve losslessly, then
  register or flag as undeclared); ops are strict (refused with
  `undeclared_kind`). Declaring, renaming or merging kinds are ops too, so
  agents can propose vocabulary in change sets.
- Theorems and proofs follow MyST's `prf:` directives: one `proof` node type
  with a `kind` attribute, not a node type per kind. References to them are
  ordinary graph edges; relations like `proves` or `uses` come later as
  vocabulary.
- Document kinds declare a layout model. Flow documents (notes, articles)
  reflow; canvas documents (decks, later posters) are a sequence of
  fixed-aspect frames. Slides are explicit container blocks with stable IDs,
  never derived from headings. Speaker notes, columns and incremental reveal
  are registry kinds or attributes; positions are attributes on block IDs in
  frame-relative units. Quarto's reveal.js conventions are the import and
  export format. Slides embed blocks from notes by reference, so a result
  lives once and appears in paper, deck and notes.
- Math: MyST `inlineMath` and `math` nodes; the LaTeX string is the source of
  truth. Equations are blocks with stable IDs; labels are names on top.
  Numbering is a projection. Macros are project state.

## Agent surface (MCP)

- A small set of general verbs, not one tool per action.
- A `guide` tool returns documentation chapters on demand.
- Refusals return a code; the guide explains each code and how to fix the call.
- Agent edits arrive as change sets by default, reviewable as single units.

## Roadmap

1. Core engine (Rust): MyST round-trips; projections rebuild from the log.
2. Desktop app: Tauri + ProseMirror; links, backlinks, block references, export.
3. Agents in the app: in-app agent, MCP for outside agents, change sets.
4. iPad with Pencil: Swift shell around the same editor and core, PencilKit ink.
5. Server, identity and sync (Supabase, a long-running sync host, Modal for
   heavy model jobs). Not needed for v1.
6. Multi-human real-time collaboration.

## Open questions

- Text merge inside blocks: adopt a CRDT under our block ops (Yjs, Automerge,
  Loro) or build our own sequence CRDT? Leaning CRDT over server-side rebasing.
- Primitive structural ops (split, merge, move, re-nest) and how they carry
  anchors and ink.
- Registry entries beyond the first cut (`docs/design/registry.md`):
  allowed children, relation source/target constraints, numbering.
- Math editing: source with live preview first; whether a visual editor
  (MathLive) is worth its noisy LaTeX rewrites.
- Layout inside a canvas frame: flow by default with layouts and absolute
  positioning as an escape hatch (Quarto), or free placement throughout
  (Keynote)? Leaning flow-first: it reflows across aspect ratios and agents
  edit it reliably.
- Op log compaction policy.
