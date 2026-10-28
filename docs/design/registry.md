# The project schema registry

Status: **accepted** (2026-10-27, all five decisions with the proposed
defaults) and implemented: `crates/lv-core/src/schema.rs`, the registry
ops in `op.rs` / `state.rs`, the `registry` projection, and the relation
menu in the desktop app's link bubble.

`CLAUDE.md` asks for a registry that is stored in the project, changed by
ops, and checked in one place (invariant 8). It declares document kinds,
block kinds and edge types as data. Before this:

- Document kinds were a Rust enum (`Note | Draft`) and a SQL `CHECK` on
  the `document` table.
- Relations were free strings: `set_relation` accepted any name.
- `kind` on `proof`, `admonition` and `container` nodes was not checked.

The registry comes before agents (layer 3), because agents are meant to
propose vocabulary in change sets, and typed links need a vocabulary to
pick from.

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
    pub label: String,                  // "Theorem", shown in UIs and the agent guide
    pub status: KindStatus,             // Declared | Provisional | Retired
    pub description: Option<String>,
    pub layout: Option<Layout>,         // document kinds only: Flow | Canvas
    pub counter: Option<String>,        // block kinds only: shared numbering counter, e.g. "theorem"
    pub inverse_label: Option<String>,  // relations only: "Supported by", for backlinks
}

pub enum KindStatus {
    /// Normal use.
    Declared,
    /// Arrived with imported content; usable, flagged for review.
    Provisional,
    /// Existing uses stay valid; new uses are refused.
    Retired,
}
```

The per-namespace fields are optional fields on the entry rather than an
enum (a change from the proposal): the JSON in the log stays flat, and
declaring or updating an entry with another namespace's field is refused
with `INVALID_OP`.

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

`schema::Registry` checks every new use, called from `ProjectState::apply`
for `create_document` (`check_use`), `insert_node`, `set_attrs` and
`set_type` (`check_content`) and `set_relation` (`check_relation`). Only
new uses are checked: content keeping the kind it had stays valid even if
that kind was retired or predates the registry.

A project with no declarations at all is not checked; its log predates the
registry. Replaying a log therefore checks each op against the registry as
it was at that op's position.

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
  On open, the app submits `schema::bootstrap` once, as the local person:
  the built-ins, plus every kind or relation the content already uses,
  declared `Provisional`. Ops already in the log were accepted without
  checks and stay valid.
- **Import is lenient.** `schema::declare_unknown` adds a
  `DeclareKind { status: Provisional }` to an import batch for every kind
  or relation it brings that the project doesn't know, so the content is
  kept losslessly and the new kind is flagged for review rather than
  refused. Ops from editors and agents are strict.

## 6. Migrations

- `DocKind` becomes a string newtype like `Relation`. It serializes the
  same way (`"note"`, `"draft"`), so the op log does not change.
- The `document.kind` `CHECK` constraint goes, and a `registry`
  projection table (namespace, name, status, entry JSON) arrives, so UIs
  and the MCP server can list the vocabulary without replaying. This is
  schema version 2 (`store/schema.sql`); `store/migrate_v2.sql` upgrades
  version 1 files by emptying the projections, which the next load
  rebuilds from the untouched op log. `Store::open` first copies the file
  aside (`Thesis (before upgrade from v1).lv`) with `VACUUM INTO`. `PROJECTION_VERSION` is 2.

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

## Decisions

1. Built-ins are ops in each project's log.
2. Checked in v1: document kinds, `proof` / `admonition` / `container`
   kinds and relations. Directive and role names are not checked.
3. Rename and merge rewrite every use, tombstones included.
4. Import declares unknown kinds as `Provisional`.
5. The starting relations are the eight in section 7.

## In the desktop app

- The link bubble has a relation menu with the project's relations
  (provisional ones marked, retired ones hidden unless in use). Choosing
  one sets the link's `relation`, which saves as `set_relation`.
- The backlinks panel labels typed links with the relation's label.
- `registry()` is a Tauri command; the preview backend returns the
  built-in relations.

## Still open

- **No UI to declare, retire, rename or merge** kinds yet, and no review
  view for provisional ones. The ops exist; agents (layer 3) and a
  vocabulary panel will use them.
- **Numbering** isn't projected yet; entries only name their counters.
- **The MCP `guide`** chapters for the new refusal codes come with the MCP
  server.
- **Open change sets and renames.** A change set that uses a name renamed
  or merged after it was opened fails to replay on accept
  (`CHANGE_SET_CONFLICT`); the agent has to redo it.
