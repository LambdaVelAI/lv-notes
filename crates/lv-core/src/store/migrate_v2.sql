-- Schema version 1 -> 2. Document kinds now come from the project's
-- registry instead of a fixed CHECK, and the registry gets a projection
-- table. Only projections change: they are emptied here (projection_meta
-- too), so the next load rebuilds them from the op log.

DELETE FROM search;
DELETE FROM edge;
DELETE FROM node;
DELETE FROM change_set;
DELETE FROM document;
DELETE FROM projection_meta;

DROP TABLE document;
CREATE TABLE document (
    id          BLOB    PRIMARY KEY CHECK (length(id) = 16),
    kind        TEXT    NOT NULL,  -- a document kind from the registry
    title       TEXT    NOT NULL,
    created_seq INTEGER NOT NULL,
    updated_seq INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
) STRICT;

CREATE TABLE registry (
    namespace   TEXT    NOT NULL CHECK (namespace IN ('document', 'block', 'relation')),
    name        TEXT    NOT NULL,
    status      TEXT    NOT NULL CHECK (status IN ('declared', 'provisional', 'retired')),
    entry       TEXT    NOT NULL CHECK (json_valid(entry)),
    PRIMARY KEY (namespace, name)
) STRICT;
