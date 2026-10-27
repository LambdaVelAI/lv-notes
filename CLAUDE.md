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
- Design notes: `docs/design/` (`ops-and-ids.md`, `desktop-shell.md`).

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

## Document model

- MyST AST is the contract; Markdown is import, export and interchange.
- Graph edges are typed (cites, supports, contradicts, ...) from an extensible
  vocabulary. Links may start untyped and gain a type later, as a separate op.

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
- Starting relation vocabulary for typed links.
- Op log compaction policy.
