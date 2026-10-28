-- lv-notes project file, schema version 2. Files at version 1 are
-- migrated by migrate_v2.sql.
--
-- One file holds one project. IDs are 16-byte UUIDv7 BLOBs. The op log is
-- the source of truth; tables marked "projection" are derived from it and
-- can be dropped and rebuilt (invariant 7).

-- The project this file holds. Exactly one row.
CREATE TABLE project (
    id          BLOB    PRIMARY KEY CHECK (length(id) = 16),
    name        TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    singleton   INTEGER NOT NULL DEFAULT 1 UNIQUE CHECK (singleton = 1)
) STRICT;

-- Principals and grants. A registry, not a projection: ops reference them.
CREATE TABLE person (
    id            BLOB    PRIMARY KEY CHECK (length(id) = 16),
    display_name  TEXT    NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE agent (
    id            BLOB    PRIMARY KEY CHECK (length(id) = 16),
    display_name  TEXT    NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE grants (
    id          BLOB    PRIMARY KEY CHECK (length(id) = 16),
    -- The holder: a person, or an agent acting for that person.
    person_id   BLOB    NOT NULL REFERENCES person(id),
    agent_id    BLOB    REFERENCES agent(id),
    scope       TEXT    NOT NULL CHECK (scope IN ('owner', 'delegated')),
    -- Agent may accept its own change sets without approval.
    auto_accept INTEGER NOT NULL DEFAULT 0 CHECK (auto_accept IN (0, 1)),
    created_at  INTEGER NOT NULL,
    revoked_at  INTEGER
) STRICT;

-- The op log. `seq` is the position the sequencer assigned (1, 2, ...).
CREATE TABLE op_log (
    seq          INTEGER PRIMARY KEY CHECK (seq > 0),
    op_id        BLOB    NOT NULL UNIQUE CHECK (length(op_id) = 16),
    base         INTEGER NOT NULL CHECK (base >= 0 AND base < seq),
    -- Principal: the person, plus the agent when one acted for them.
    person_id    BLOB    NOT NULL REFERENCES person(id),
    agent_id     BLOB    REFERENCES agent(id),
    grant_id     BLOB    NOT NULL REFERENCES grants(id),
    change_set   BLOB    CHECK (change_set IS NULL OR length(change_set) = 16),
    created_at   INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    body         TEXT    NOT NULL CHECK (json_valid(body)),
    body_format  INTEGER NOT NULL
) STRICT;

CREATE INDEX op_log_change_set ON op_log (change_set) WHERE change_set IS NOT NULL;

-- The log is append-only.
CREATE TRIGGER op_log_no_update BEFORE UPDATE ON op_log
BEGIN SELECT RAISE(ABORT, 'op_log is append-only'); END;
CREATE TRIGGER op_log_no_delete BEFORE DELETE ON op_log
BEGIN SELECT RAISE(ABORT, 'op_log is append-only'); END;

-- ---- Projections --------------------------------------------------------

-- Which projections are built, by what, and up to where.
CREATE TABLE projection_meta (
    name               TEXT    PRIMARY KEY,
    producer           TEXT    NOT NULL,  -- code or model that built it
    producer_version   TEXT    NOT NULL,
    built_through_seq  INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Projection: documents.
CREATE TABLE document (
    id          BLOB    PRIMARY KEY CHECK (length(id) = 16),
    kind        TEXT    NOT NULL,  -- a document kind from the registry
    title       TEXT    NOT NULL,
    created_seq INTEGER NOT NULL,
    updated_seq INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
) STRICT;

-- Projection: the node tree of every document. `parent_id` NULL means the
-- node sits at its document's root. `position` is the node's index among
-- its siblings, tombstones included. `num` is a file-local row number used
-- only to key the search index; it is never shared or exported.
CREATE TABLE node (
    num          INTEGER PRIMARY KEY,
    id           BLOB    NOT NULL UNIQUE CHECK (length(id) = 16),
    document_id  BLOB    NOT NULL REFERENCES document(id),
    parent_id    BLOB    REFERENCES node(id),
    position     INTEGER NOT NULL,
    type         TEXT    NOT NULL,
    identifier   TEXT,   -- MyST label target, for resolving references
    attrs        TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(attrs)),
    text         TEXT,
    updated_seq  INTEGER NOT NULL,
    deleted      INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
) STRICT;

CREATE INDEX node_children ON node (document_id, parent_id, position);
CREATE INDEX node_identifier ON node (identifier) WHERE identifier IS NOT NULL;

-- Projection: change sets and their review state.
CREATE TABLE change_set (
    id          BLOB    PRIMARY KEY CHECK (length(id) = 16),
    person_id   BLOB    NOT NULL,
    agent_id    BLOB,
    base        INTEGER NOT NULL,
    rationale   TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('open', 'accepted', 'rejected')),
    opened_seq  INTEGER NOT NULL,
    closed_seq  INTEGER
) STRICT;

-- Projection: graph edges, one per live link, cite or reference node.
-- `target` is the raw target as written: a URL, a label, a citation key,
-- or an lv-notes ID (`node_…`, `doc_…`), which also fills target_node or
-- target_doc. Labels resolve at query time by joining node.identifier.
CREATE TABLE edge (
    source_node  BLOB    PRIMARY KEY REFERENCES node(id),
    source_doc   BLOB    NOT NULL REFERENCES document(id),
    kind         TEXT    NOT NULL CHECK (kind IN ('link', 'crossReference', 'cite')),
    target       TEXT,
    target_node  BLOB,
    target_doc   BLOB,
    relation     TEXT     -- NULL while untyped
) STRICT;

CREATE INDEX edge_target ON edge (target) WHERE target IS NOT NULL;

CREATE INDEX edge_target_node ON edge (target_node);
CREATE INDEX edge_target_doc ON edge (target_doc);

-- Projection: the project's vocabulary (document kinds, block kinds,
-- relations), from declare/update/rename/merge_kind ops. `entry` is the
-- KindEntry as JSON.
CREATE TABLE registry (
    namespace   TEXT    NOT NULL CHECK (namespace IN ('document', 'block', 'relation')),
    name        TEXT    NOT NULL,
    status      TEXT    NOT NULL CHECK (status IN ('declared', 'provisional', 'retired')),
    entry       TEXT    NOT NULL CHECK (json_valid(entry)),
    PRIMARY KEY (namespace, name)
) STRICT;

-- Projection: full-text search over live node text, keyed by node.num.
CREATE VIRTUAL TABLE search USING fts5 (text);
