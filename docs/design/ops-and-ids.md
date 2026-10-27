# Proposal: IDs and ops

Status: **accepted** (2026-10-27, all five questions answered with the
proposed defaults). Implemented in `crates/lv-core`: `ids`, `principal`, `op`,
and `store` (schema in `store/schema_v1.sql`).

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
assigns them before anything reaches the log. Markdown export drops them. A
re-import is therefore a new set of nodes, unless we later add a hidden ID
comment to the export (not proposed for v1).

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

    // Typed links: the edge is the link/cite node itself, so the graph
    // projection reads it from the tree. Typing it later is its own op.
    SetRelation { node: NodeId, relation: Option<Relation> },

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

### Rebase rules (sequencer, v1)

Because ops address IDs, most concurrent ops commute without change. The
sequencer refuses, with a code, when:

| Code | When |
|---|---|
| `NODE_DELETED` | the target or the `after` anchor was deleted after `base` (an `after` anchor alone could instead fall back to "after its nearest surviving left sibling"; propose doing that) |
| `CYCLE` | a `MoveNode` would make a node its own ancestor |
| `STALE_TEXT` | two `EditText::Replace` on one node since `base` (goes away with the CRDT) |
| `FORBIDDEN` | `authorize` refused |
| `WRONG_PROJECT` | `op.project` ≠ this sequencer's project |

## 5. What the storage layer will look like (preview only)

One SQLite file per project. These tables come next, after this proposal is
agreed:

- `project` (a single row: id, created_at, schema version)
- `person`, `agent`, `grant`
- `op_log` (`seq INTEGER PRIMARY KEY`, `op_id BLOB UNIQUE`, `base`, principal
  columns, `grant_id`, `change_set`, `created_at`, `body` as JSON plus
  `body_format` version)
- `change_set` (id, author, base, rationale, state)
- projections: `document`, `node` (id, doc, parent, order key, type, attrs
  JSON, text, deleted), `edge` (from node, target, relation), `search` (FTS5),
  plus `projection_meta` (name, producer and version, `built_through_seq`),
  which records invariant 7's provenance.

Op bodies are stored as **JSON** in v1: easy to debug and to migrate. The
format version column leaves room to move to a binary format later.

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
  non-contiguous, duplicate or unregistered-principal ops. Whether the
  principal may use the grant is `authorize`'s job in the sequencer, which
  is not written yet.
- **wasm.** The crate builds without the `sqlite` feature. On
  `wasm32-unknown-unknown`, `OpId::new()` and `Timestamp::now()` will also need
  `uuid`'s `js` feature and a JS clock.
