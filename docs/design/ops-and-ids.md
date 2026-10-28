# Proposal: IDs and ops

Status: **accepted** (2026-10-27, all five questions answered with the
proposed defaults). Implemented in `crates/lv-core`: `ids`, `principal`, `op`,
and `store` (schema in `store/schema.sql`).

This settles the types that the op log, the sequencer and the SQLite schema
all depend on. Retrofitting any of them means migrating the op log, so they
are decided first.

## 1. IDs

All shared identities are **UUIDv7**: 128-bit, generated anywhere without
coordination, and time-ordered, so new rows land at the end of SQLite
B-tree indexes instead of at random positions. Each kind gets its own
newtype so a `NodeId` can never be passed where a `DocumentId` is expected.

```rust
pub struct ProjectId(Uuid);
pub struct DocumentId(Uuid);
pub struct NodeId(Uuid);      // blocks and addressable inline nodes
pub struct OpId(Uuid);
pub struct ChangeSetId(Uuid);
pub struct PersonId(Uuid);
pub struct AgentId(Uuid);
pub struct GrantId(Uuid);
```

- **Storage:** 16-byte `BLOB`.
- **Text form** (logs, MCP, debugging): a type prefix plus the canonical UUID,
  e.g. `node_0192f3c4-…`. Parsing checks the prefix, so an agent that passes a
  document ID as a node ID gets a refusal code, not a silent miss.
- **Dependency:** the `uuid` crate (`v7`, `serde`). It builds for wasm with its
  `js` feature, so portability holds.

### Versions are positions, not identities

```rust
/// Position in a project's op log, assigned by the project's sequencer.
/// 0 = empty project; the first accepted op is 1.
pub struct Version(u64);
```

Invariant 4 rules out auto-increment *identities*. `Version` is not one: it
is the order that the project's single sequencer assigns, and it is only
meaningful inside that project. Every op is identified by its `OpId`. When
sync arrives, the server becomes the sequencer for the project and versions
stay per-project counters.

### Which AST nodes get a `NodeId`

- **Every block-level node** (paragraph, heading, list, list item, code,
  math, directive, container, table, row, cell, …).
- **Inline nodes that are anchors or graph edges**: `link`, `crossReference`,
  `cite`, `footnoteReference`, `mystTarget`, `image`. Typed links hang off
  these.
- **Plain inline text runs do not.** Positions inside text are the text-CRDT's
  job (open question). Comments and ink anchor to `(NodeId, text anchor)`.

The ID lives in the AST as a field on `Common`:

```rust
pub struct Common {
    pub id: Option<NodeId>,   // new; serialized as "id"
    pub identifier: Option<String>,
    pub label: Option<String>,
    ...
}
```

It is `Option` because imported Markdown does not have IDs yet; import
assigns them before anything reaches the log. Inside a project every node,
text runs included, has a `NodeId`, because `insert_node` addresses one. Text
run IDs are internal: they are never exported, and nothing should hold on
to them. Native MyST export (section 6) keeps the other IDs, so it
re-imports as the same nodes. Markdown export drops them, so a re-imported
Markdown file is a new set of nodes unless we later add a hidden ID comment
to the export (not proposed for v1).

## 2. Principals and grants (invariant 1)

```rust
pub enum Principal {
    Person { person: PersonId },
    Agent  { agent: AgentId, on_behalf_of: PersonId },
}
```

Every op carries `principal` and `grant: GrantId`. In v1 there is one local
person, an owner grant for them, and a delegated grant per agent. The single
permission check (invariant 3) is:

```rust
pub fn authorize(project: &ProjectState, principal: &Principal, grant: GrantId, op: &OpBody)
    -> Result<(), Refusal>;
```

It returns `Ok` for now, but the sequencer calls it on every op.

## 3. The op envelope

```rust
pub struct Op {
    pub id: OpId,
    pub project: ProjectId,              // invariant 2
    pub principal: Principal,            // invariant 1
    pub grant: GrantId,
    pub base: Version,                   // invariant 5: version it was made against
    pub change_set: Option<ChangeSetId>, // invariant 6
    pub created_at: Timestamp,           // client wall clock, informational only
    pub body: OpBody,
}

/// What the sequencer returns: the op as accepted (possibly rebased) and its position.
pub struct Sequenced {
    pub seq: Version,
    pub op: Op,
}
```

Clients submit `Op`. The sequencer checks `authorize`, rebases against the
ops in `(base, head]` if needed, then either assigns `seq = head + 1` or
refuses with a `Refusal { code, message }`.

## 4. Op bodies

These are small, structural and ID-addressed (invariant 4). Sibling order is
expressed by naming the left neighbour (`after`), not an index, so two
concurrent inserts under the same parent never collide on a position. The
sequencer's total order breaks ties.

```rust
pub enum OpBody {
    // Documents
    CreateDocument { doc: DocumentId, kind: DocKind /* Note | Draft */, title: String },
    SetDocumentTitle { doc: DocumentId, title: String },
    DeleteDocument { doc: DocumentId },              // tombstone, restorable

    // Tree structure (works across documents in a project, which is how
    // "move block to another note" stays atomic)
    InsertNode { node: NodeId, parent: Parent, after: Option<NodeId>, content: NodeContent },
    MoveNode   { node: NodeId, parent: Parent, after: Option<NodeId> },
    DeleteNode { node: NodeId },                     // tombstones the subtree

    // Content of one node
    SetAttrs { node: NodeId, set: Map<String, Value>, unset: Vec<String> },
    EditText { node: NodeId, edit: TextEdit },
    // Change the MyST type in place (paragraph → heading), keeping the ID and
    // so every link and anchor on it. `set`/`unset` may include `value`.
    SetType { node: NodeId, node_type: String, set: Map<String, Value>, unset: Vec<String> },

    // Typed links: the edge is the link/cite node itself, so the graph
    // projection reads it from the tree. Typing it later is its own op.
    SetRelation { node: NodeId, relation: Option<Relation> },

    // The project's vocabulary (docs/design/registry.md)
    DeclareKind { namespace: Namespace, name: String, entry: KindEntry },
    UpdateKind  { namespace: Namespace, name: String, entry: KindEntry },
    RenameKind  { namespace: Namespace, from: String, to: String },
    MergeKind   { namespace: Namespace, from: String, into: String },

    // Change sets
    OpenChangeSet   { change_set: ChangeSetId, rationale: String },
    AcceptChangeSet { change_set: ChangeSetId },
    RejectChangeSet { change_set: ChangeSetId, reason: Option<String> },
}

pub enum Parent { DocumentRoot(DocumentId), Node(NodeId) }

/// A node's own fields without its children: the `type` plus attributes.
/// Children arrive as their own InsertNode ops, so every node has an ID-addressed history.
pub struct NodeContent { /* MyST node minus children, as JSON */ }

pub enum TextEdit {
    /// v1 placeholder: replace the node's whole text value.
    Replace { value: String },
    // Later: CRDT updates (Yjs/Loro/Automerge), once that open question is settled.
}

/// An extensible vocabulary: well-known relations plus project-defined ones.
pub struct Relation(String); // "cites", "supports", "contradicts", ...
```

Composite edits (split a paragraph, paste a section, import a file) are a
**batch** of these primitives, submitted together with one `base`. The
sequencer accepts or refuses the batch as a whole, and its ops get
consecutive `seq` values.

### Change sets

- Ops with `change_set: None` apply immediately (normal human typing).
- Ops in a change set are sequenced into the log like any other op, but the
  document projection applies them only after an `AcceptChangeSet`. Until then
  the projection shows them as a proposal overlay. A rejected set stays in the
  log for the record.
- Agent edits use a change set by default (CLAUDE.md). The change set records
  its author (the principal of `OpenChangeSet`), its base version and its
  rationale.

### Sequencer rules (v1, implemented)

Because ops address IDs, rebasing means checking the op again at the head;
the op itself is never rewritten. `ProjectState::apply` (`state.rs`) is the
one definition of op semantics. The sequencer validates against it, and
replaying the log through it rebuilds the project. The full list of codes is
`RefusalCode` in `refusal.rs`. The main rules:

| Code | When |
|---|---|
| `WRONG_PROJECT` | `op.project` ≠ this sequencer's project |
| `BASE_IN_FUTURE` | `op.base` is after the head |
| `DUPLICATE_OP` | the op ID was accepted before, or appears twice in the batch |
| `FORBIDDEN` | `authorize` refused: unknown grant, or a grant held by someone else |
| `NODE_DELETED` / `DOCUMENT_DELETED` | the target, or the parent of an insert or move, is a tombstone |
| `CYCLE` | a move would put a node inside its own subtree |
| `STALE_TEXT` | a *different* principal replaced the node's text after `op.base` (goes away with the CRDT) |
| `CHANGE_SET_CONFLICT` | an open change set no longer applies on top of the current project |
| `CHANGE_SET_CLOSED` | adding to, accepting or rejecting a change set already accepted or rejected |
| `UNDECLARED_KIND` / `RETIRED_KIND` | a new use of a kind or relation the registry doesn't declare, or has retired (`registry.md`) |
| `KIND_EXISTS` / `UNKNOWN_KIND` | declaring an existing name, or updating, renaming or merging a missing one |
| `INVALID_OP` | e.g. `set_attrs` on `type`/`id`/`children`/`value`, a relation on a non-link node, or a `set_type` to or from a text run, onto a leaf that still has children, or off an edge that still has a relation |

Details decided while implementing:

- **Anchors.** Tombstones keep their place among siblings, so an insert
  after a deleted anchor lands right after its nearest surviving left
  sibling. An anchor that is no longer a sibling (it was moved) means first
  child. An anchor that never existed is `UNKNOWN_NODE`. Of two inserts after
  the same anchor, the later-sequenced one comes first.
- **Deletes are idempotent.** Deleting a tombstone is accepted and changes
  nothing.
- **Pipelining.** `STALE_TEXT` ignores the principal's own earlier edits, so a
  client may send several replaces on one base.
- **Change sets.** Each op in a change set is checked against the project
  *with that change set applied*, so an agent can insert a node and then
  edit it inside the same proposal. Accepting replays the set on the current
  project; if any op no longer applies, the accept is refused and the set
  stays open. `OpenChangeSet` belongs to the set it opens. Accept and reject
  belong to no set.
- **Batches** are checked op by op on a copy of the state, written to the
  log, and only then made current. A log failure leaves the sequencer
  unchanged.

### Agent approval and auto mode (implemented)

Agent work always arrives as change sets. Whether a person must approve it
is a per-grant toggle, `Grant::auto_accept`, off by default. Each agent
session has its own delegated grant, so the toggle is per session and can
change mid-session (`Store::set_auto_accept`); it takes effect on the next op.

| Agent op | Auto-accept off | Auto-accept on |
|---|---|---|
| Edit outside a change set | `FORBIDDEN` | `FORBIDDEN` |
| Edit inside its own open change set | allowed | allowed |
| Accept its own change set | `FORBIDDEN` (needs the person) | allowed |
| Reject (withdraw) its own change set | allowed | allowed |
| Accept or reject anyone else's | `FORBIDDEN` | `FORBIDDEN` |

Auto mode does not lose attribution. The change set records the agent as
author, its ops carry the agent's principal, grant and time, and the accept
op does too. The log therefore shows the change was self-approved, and the
set stays one reviewable unit that can be inspected (and later reverted)
after the fact. Auto-accept can happen in the same batch as the edits.

The UX for the toggle (where it lives, how self-accepted sets are
surfaced for later review) is still to be designed.

## 5. Storage and projections (implemented)

One SQLite file per project; the schema is in `store/schema.sql` (version 2;
`store/migrate_v2.sql` upgrades version 1 files).

- **Source of truth:** `op_log`, which is append-only (enforced by
  triggers), plus the `person`, `agent` and `grants` registry. Op bodies are
  JSON with a `body_format` column, so the format can change later.
- **Projections:**
  - `document` and `node`: the tree, with sibling `position`, MyST
    `identifier`, attributes, text and tombstones.
  - `edge`: one row per live link, cross-reference or citation, with its
    raw target and relation.
  - `change_set`: author, rationale and review state.
  - `registry`: the project's declared vocabulary.
  - `search`: FTS5 over live node text.
  - `projection_meta`: records which version built the projections and up
    to which op (invariant 7).

How projections stay correct:

- **No SQL semantics.** `ProjectState::apply` returns `Effects`: the
  documents, nodes, re-ordered parents and change sets an op changed. The
  store rewrites exactly those rows from the new state. A rebuild writes
  every row from a replayed state, and a test checks after every op that
  both give identical tables.
- **Same transaction as the log.** `Store::commit` appends ops and updates
  projections atomically. If the projections were not current before a
  commit (e.g. after a log-only `append`, or a file from another
  `PROJECTION_VERSION`), the commit rebuilds them instead.
  `Sequencer::load` rebuilds them on open for the same reasons.
- **Pending proposals are not projected.** Open change sets appear only as
  `change_set` rows. A UI shows a proposal with
  `ProjectState::with_change_set`.
- **References by label.** Edges keep their target as written. Labels
  (`#slow-reading`, `{ref}` identifiers) resolve at query time by joining
  `node.identifier`, so adding or renaming a label needs no edge rewrite.
  IDs (`node_…`, `doc_…`) also fill `target_node` / `target_doc`.
- **`node.num`** is a file-local row number keying the FTS index. It is
  never shared or exported, so invariant 4 holds.

Queries so far: `Store::children`, `Store::search` (FTS5 syntax),
`Store::backlinks` (to one node, by ID or label, with relation) and
`Store::document_backlinks` (into a document: at it, or at any of its live
nodes). `convert::enclosing_block` and `convert::plain_text` turn a hit or
a link into the block it sits in and that block's text.

## 6. MyST import and export (implemented)

`convert.rs` turns a MyST AST into ops and reads a document back out of a
`ProjectState`.

- **Import** (`import_document`, `import_nodes` for a pasted fragment):
  - emits `create_document`, then one `insert_node` per node in document
    order, each `after` its previous sibling;
  - keeps a node's lv-notes `id` if it has one, and assigns a new one
    otherwise;
  - turns a `relation` field on a link, cross-reference or citation into a
    `set_relation` op;
  - refuses: a document whose top node isn't `root`, a duplicate `id`, an
    `id` that is not a `node_…` ID, and children that are not nodes.
  - The result is ordinary ops, so an import goes through the sequencer
    like any edit: one batch, all or nothing, attributed and permission
    checked.
- **The root is a node.** An imported document has a single top-level
  `root` node. Its own fields (frontmatter, …) are edited with `set_attrs`
  like any other node's. A document built by ops without a `root`
  materializes under a synthesized one.
- **Export** (`materialize`) builds the live tree in sibling order, with
  tombstones left out, in one of two flavors:
  - `Plain` is standard MyST.
  - `Native` adds `id` to every node except text runs (`TEXT_RUN_TYPES`:
    `text`, `emphasis`, `strong`, `inlineCode`, …), and `relation` to typed
    links.
- **Saving an edited tree** (`update_document`) is the reverse diff, used
  by the editor:
  - Nodes match by `id`, and text runs by type and order.
  - The longest run of children already in order stays put; the rest move.
  - Changed fields become `set_attrs`, `edit_text` and `set_relation`.
  - A changed type becomes `set_type`, carrying the field changes. A node
    becoming a leaf (paragraph → code) has its children deleted first, and
    a relation the new type can't carry is cleared first.
  - Missing nodes are deleted.
  - An ID that can't be reused (the node was deleted, or it changes to or
    from a text run) gets a new ID, reported in `renamed`.

  A seeded random-edit test checks that every save materializes back to
  the edited tree. See `desktop-shell.md`.
- **Ops keep documents valid MyST.** `insert_node` and `set_attrs` refuse
  content that does not parse as its MyST type, for example a `heading`
  whose `depth` is a string, with `INVALID_OP`. Inserting under a MyST
  leaf (`text`, `code`, …) is refused too. That makes `materialize`
  infallible in practice.

- **Markdown export** (`export.rs`) writes MyST Markdown from the plain
  tree: each live document becomes `<slug of its title>.md` (`-2`, ... on
  clashes) with its title in the frontmatter. Links inside the project are
  rewritten from IDs to paths: `doc_…` to the document's file, `node_…` to
  `file.md#label`. A linked block uses its own label if it has one;
  otherwise the export adds a `(node-<uuid>)=` target before it. A link
  whose target is gone keeps its ID as the url. IDs and relations are not
  exported, so a re-imported Markdown file is new nodes. Directives are
  written from their source (`value`) when they kept it, and from their
  parsed content otherwise. `tests/export.rs` checks the fixtures against
  `tests/fixtures/markdown/*.md` (regenerate with `WRITE_GOLDEN=1`).

Tests (`tests/convert.rs`): each fixture goes through AST → ops → state →
AST and must come back unchanged. A native export must equal the plain
export plus `id`/`relation`, and must re-import into another project
unchanged. Further tests cover edits and fragments, relations, the import
refusals and invalid-content ops, and reloading from a project file.

## Decisions

1. UUIDv7 with typed text prefixes (`node_…`, `doc_…`, …).
2. IDs on blocks and anchor-like inlines (links, citations, refs, targets,
   images). Text runs get no IDs.
3. `EditText::Replace` is the v1 placeholder until the text CRDT is chosen.
4. Change-set ops go in the main log, and documents apply them once accepted.
5. A deleted `after` anchor falls back to the nearest surviving left sibling;
   it is not refused.

## Still open (found while implementing)

- **Principals and grants are a registry, not ops.** `person`, `agent` and
  `grants` are plain tables that ops reference by foreign key. Granting and
  revoking are therefore not yet in the op log's audit trail. They should
  probably become ops before sharing (roadmap step 5).
- **The store checks integrity, not permission.** It refuses wrong-project,
  non-contiguous, duplicate or unregistered-principal ops. Permission is
  `authorize` (`auth.rs`).
- **Cost per submit.** The sequencer validates on a full copy of the
  in-memory state (projection writes are proportional to what changed), and change-set ops also replay their set on a copy. That
  is O(project size) per submit: fine for v1-sized projects, but it calls for
  persistent maps (e.g. `imbl`) or an undo journal before large corpora.
- **The toggle itself is not logged.** Grants are a registry table, so
  turning auto mode on or off is not in the op log yet; it should become an
  op along with grant changes generally.
- **Updated versions after accept.** Ops replayed by an accept keep their
  own position as the node's `updated` version, not the accept's.
- **Label resolution is project-wide and first-match.** Two nodes with the
  same `identifier` both match a backlink query. MyST requires unique labels
  per project, but nothing enforces that yet.
- **Revoked grants.** `grants.revoked_at` exists, but there is no revoke API
  and `authorize` does not look at it yet.
- **Explicit empty `children` on optional parents.** A `cite`,
  `mystDirective`, `mystRole` or unknown node with `"children": []` exports
  without the field, since state does not record whether an empty list was
  present. Known parent types always export it.
- **Document title vs. frontmatter title.** The document's `title` (an op)
  and the root's `frontmatter.title` (an attribute) are separate. Import
  takes the title as an argument, and nothing keeps the two in sync.
- **`relation` is an lv-notes extension field.** A foreign MyST file with
  its own `relation` field on a link would be read as a typed link. Fine
  until the relation vocabulary is settled.
- **wasm.** The crate builds without the `sqlite` feature. On
  `wasm32-unknown-unknown`, `OpId::new()` and `Timestamp::now()` will also need
  `uuid`'s `js` feature and a JS clock.
