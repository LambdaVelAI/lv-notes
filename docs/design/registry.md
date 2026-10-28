# Proposal: the project schema registry

Status: **proposal**, for review. Nothing here is built yet.

`CLAUDE.md` asks for a registry that is stored in the project, changed by
ops, and checked in one place (invariant 8). It declares document kinds,
block kinds and edge types as data. Today none of that exists:

- Document kinds are a Rust enum (`DocKind::Note | Draft`) and a SQL
  `CHECK` on the `document` table.
- Relations are free strings: `set_relation` accepts any name.
- `kind` on `proof`, `admonition` and `container` nodes is not checked.

This settles the registry before agents arrive (layer 3), because agents
are meant to propose vocabulary in change sets, and typed links need a
vocabulary to pick from.

## 1. What is registered

Three namespaces, each a map from name to entry:

| Namespace | Names | Checked on |
| --- | --- | --- |
| `document` | `note`, `draft`, later `article`, `deck` | `create_document` |
| `block` | `<node type>:<kind>`, e.g. `proof:theorem`, `admonition:warning`, `container:figure` | `insert_node`, `set_attrs`, `set_type` of a `proof`, `admonition` or `container` |
| `relation` | `supports`, `contradicts`, ... | `set_relation` |

Block kinds are keyed by node type, because MyST reuses `kind` across
types (`proof` has `kind: theorem`, `admonition` has `kind: warning`). This
follows `CLAUDE.md`: one `proof` node type with a `kind`, not a node type
per kind.

**Not checked in v1:** directive and role names (`mystDirective.name`,
`mystRole.name`). They stay opaque MyST that the editor preserves. They can
join the `block` namespace later as `directive:<name>`.

## 2. Entry shape

Kept small; only fields something uses now.

```rust
pub struct KindEntry {
    pub label: String,            // "Theorem", shown in UIs and the agent guide
    pub status: KindStatus,       // Declared | Provisional | Retired
    pub description: Option<String>,
    pub detail: KindDetail,
}

pub enum KindStatus {
    /// Normal use.
    Declared,
    /// Arrived with imported content; usable, flagged for review.
    Provisional,
    /// Existing uses stay valid; new uses are refused.
    Retired,
}

pub enum KindDetail {
    Document { layout: Layout },                   // Flow | Canvas
    Block { counter: Option<String> },             // shared numbering counter, e.g. "theorem"
    Relation { inverse_label: Option<String> },    // "supported by", for backlinks
}
```

- **Numbering** stays a projection (`CLAUDE.md`). An entry only names its
  counter, so `lemma` and `theorem` can share one (Theorem 1, Lemma 2) or
  not.
- **Allowed children, source/target constraints on relations, icons and
  colours** are left out until something needs them. The entry is JSON in
  the op log, so adding optional fields later is not a migration.

## 3. Ops

```rust
DeclareKind { namespace: Namespace, name: String, entry: KindEntry },
UpdateKind  { namespace: Namespace, name: String, entry: KindEntry },  // label, status, detail
RenameKind  { namespace: Namespace, from: String, to: String },
MergeKind   { namespace: Namespace, from: String, into: String },
```

- **Rename and merge rewrite every use** in the same op: every live node
  with that `kind` (or relation) gets the new name. Within a project this is
  atomic, like a cross-document move. The alternative, an alias table that
  queries resolve through, keeps old names around forever and makes every
  reader resolve them.
- **No delete.** `Retired` keeps old documents valid and the log replayable.
- These are ordinary ops: attributed, permission-checked, and usable in
  change sets, so an agent can propose a new relation or a renamed kind
  and the person reviews it like any edit.

## 4. One check, in the core

A new `schema` module with one function, called from `ProjectState::apply`
for every op that creates or changes a kind, relation or document:

```rust
pub fn conforms(registry: &Registry, op: &OpBody, state: &ProjectState) -> Result<(), Refusal>;
```

Refusal codes:

- `UNDECLARED_KIND`: the name is not in the registry for that namespace.
- `RETIRED_KIND`: new use of a retired kind.
- `KIND_EXISTS` / `UNKNOWN_KIND`: declaring an existing name, or updating,
  renaming or merging a missing one.

The MCP `guide` explains each code and how to fix the call (declare the
kind first, or pick a declared one).

## 5. Where entries come from

- **Built-ins are ops in the project's own log.** A new project's first
  batch declares the built-in vocabulary (section 7). A copied project
  therefore carries its vocabulary, and nothing depends on the app version
  that opens it. Built-ins are pre-filled entries, not hard-coded, as
  `CLAUDE.md` asks.
- **Existing projects** (created before the registry) have no declarations.
  On open, the app submits the built-in batch once, as the local person.
  Ops already in the log were accepted without checks and stay valid:
  replay checks an op against the registry as it was at that op's position.
- **Import is lenient.** Importing MyST with an unknown `kind` adds a
  `DeclareKind { status: Provisional }` to the import batch, so the content
  is kept losslessly and the new kind is flagged for review rather than
  refused. Ops from editors and agents are strict.

## 6. Migrations this needs

- `DocKind` becomes a string newtype like `Relation`. It serializes the
  same way (`"note"`, `"draft"`), so the op log does not change.
- The `document.kind` `CHECK` constraint goes. The table is a projection,
  so this is a schema-version bump that drops and rebuilds projections; the
  op log is untouched.
- A `registry` projection table (namespace, name, entry JSON, status), so
  UIs and the MCP server can list the vocabulary without replaying.

## 7. Starting vocabulary

- **Documents:** `note` (flow), `draft` (flow). `article` and `deck` come
  with their layouts.
- **Proofs** (MyST's `prf:` kinds): `theorem`, `lemma`, `corollary`,
  `proposition`, `conjecture`, `definition`, `axiom`, `assumption`,
  `example`, `remark`, `observation`, `property`, `criterion`, `notation`,
  `algorithm`, `proof`. Counters: `theorem`, `lemma`, `corollary`,
  `proposition` and `conjecture` share `theorem`; the rest count alone, and
  `proof` is unnumbered.
- **Admonitions:** MyST's set: `note`, `tip`, `hint`, `important`,
  `warning`, `caution`, `attention`, `danger`, `error`, `seealso`,
  `admonition`.
- **Containers:** `figure`, `table`, `code`.
- **Relations** (to start; easy to extend per project):

  | Relation | Inverse label |
  | --- | --- |
  | `supports` | supported by |
  | `contradicts` | contradicted by |
  | `extends` | extended by |
  | `uses` | used by |
  | `proves` | proved by |
  | `cites` | cited by |
  | `defines` | defined by |
  | `example-of` | has example |

  `CITES`, `SUPPORTS` and `CONTRADICTS` already exist as constants in
  `op.rs`.

## Decisions needed

1. **Built-ins as ops in each project's log** (proposed), rather than an
   implicit baseline the app supplies.
2. **Scope for v1:** document kinds, `proof` / `admonition` / `container`
   kinds and relations are checked; directive and role names are not
   (proposed).
3. **Rename and merge rewrite uses** (proposed), rather than keeping an
   alias table.
4. **Import declares unknown kinds as `Provisional`** (proposed), rather
   than refusing or leaving them undeclared and flagged.
5. **The starting relations** in section 7: keep, trim or extend.
