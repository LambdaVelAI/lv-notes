//! Writes the projection tables from a [`ProjectState`].
//!
//! Projections hold no logic of their own: every row is a copy of state that
//! [`ProjectState::apply`] computed. Incremental updates rewrite exactly the
//! rows named by [`Effects`]; a rebuild rewrites everything. Both must give
//! identical tables, which the tests check.

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::ids::{DocumentId, NodeId, Version};
use crate::op::Parent;
use crate::state::{Effects, ProjectState};

use super::Result;

/// Projection tables, as named in `projection_meta`.
pub const PROJECTIONS: [&str; 6] = ["document", "node", "edge", "change_set", "search", "registry"];
pub const PRODUCER: &str = "lv-core";
/// Bump whenever projection rows would come out differently for the same
/// log; files built by another version are rebuilt on load.
pub const PROJECTION_VERSION: &str = "2";

/// Whether every projection was built by this version through `head`.
pub(super) fn is_current(conn: &Connection, head: Version) -> Result<bool> {
    let current: i64 = conn.query_row(
        "SELECT count(*) FROM projection_meta
         WHERE producer = ?1 AND producer_version = ?2 AND built_through_seq = ?3",
        params![PRODUCER, PROJECTION_VERSION, head.0 as i64],
        |r| r.get(0),
    )?;
    Ok(current == PROJECTIONS.len() as i64)
}

pub(super) fn mark_built(conn: &Connection, through: Version) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO projection_meta (name, producer, producer_version, built_through_seq)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (name) DO UPDATE SET
           producer = excluded.producer,
           producer_version = excluded.producer_version,
           built_through_seq = excluded.built_through_seq",
    )?;
    for name in PROJECTIONS {
        stmt.execute(params![name, PRODUCER, PROJECTION_VERSION, through.0 as i64])?;
    }
    Ok(())
}

/// Rewrites the rows named by `effects`. Expects foreign keys to be
/// deferred, since effects are not ordered parent-first.
pub(super) fn write_effects(conn: &Connection, state: &ProjectState, effects: &Effects) -> Result<()> {
    for &doc in &effects.documents {
        write_document(conn, state, doc)?;
    }
    for &node in &effects.nodes {
        write_node(conn, state, node)?;
    }
    for &parent in &effects.parents {
        renumber(conn, state, parent)?;
    }
    for &cs in &effects.change_sets {
        write_change_set(conn, state, cs)?;
    }
    if effects.registry {
        write_registry(conn, state)?;
    }
    Ok(())
}

/// Rewrites the whole registry table: it is small, and renames move rows.
fn write_registry(conn: &Connection, state: &ProjectState) -> Result<()> {
    conn.execute("DELETE FROM registry", [])?;
    let mut stmt = conn.prepare_cached("INSERT INTO registry (namespace, name, status, entry) VALUES (?1, ?2, ?3, ?4)")?;
    for (namespace, name, entry) in state.registry().entries() {
        let json = serde_json::to_string(entry).expect("serializes");
        stmt.execute(params![namespace.as_str(), name, entry.status.as_str(), json])?;
    }
    Ok(())
}

/// Replaces every projection row with `state`.
pub(super) fn rebuild(conn: &Connection, state: &ProjectState) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM search; DELETE FROM edge; DELETE FROM node;
         DELETE FROM change_set; DELETE FROM document; DELETE FROM registry;",
    )?;
    let effects = Effects {
        documents: state.documents().map(|(id, _)| id).collect(),
        nodes: state.nodes().map(|(id, _)| id).collect(),
        parents: Default::default(),
        change_sets: state.change_sets().map(|(id, _)| id).collect(),
        registry: true,
    };
    write_effects(conn, state, &effects)
}

fn write_document(conn: &Connection, state: &ProjectState, id: DocumentId) -> Result<()> {
    let d = state.document(id).expect("effects name existing documents");
    let kind = d.kind.0.as_str();
    conn.prepare_cached(
        "INSERT INTO document (id, kind, title, created_seq, updated_seq, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET
           kind = excluded.kind, title = excluded.title,
           updated_seq = excluded.updated_seq, deleted = excluded.deleted",
    )?
    .execute(params![id, kind, d.title, d.created.0 as i64, d.updated.0 as i64, d.deleted])?;
    Ok(())
}

fn write_node(conn: &Connection, state: &ProjectState, id: NodeId) -> Result<()> {
    let n = state.node(id).expect("effects name existing nodes");
    let parent_id = match n.parent {
        Parent::Node(p) => Some(p),
        Parent::DocumentRoot(_) => None,
    };
    let position = position(state, n.parent, id);
    let mut attrs = n.content.fields.clone();
    let text = match attrs.remove("value") {
        Some(Value::String(t)) => Some(t),
        Some(other) => {
            attrs.insert("value".to_owned(), other);
            None
        }
        None => None,
    };
    let identifier = n.content.fields.get("identifier").and_then(Value::as_str);

    let num: i64 = conn
        .prepare_cached(
            "INSERT INTO node
               (id, document_id, parent_id, position, type, identifier, attrs, text, updated_seq, deleted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
               document_id = excluded.document_id, parent_id = excluded.parent_id,
               position = excluded.position, type = excluded.type,
               identifier = excluded.identifier, attrs = excluded.attrs, text = excluded.text,
               updated_seq = excluded.updated_seq, deleted = excluded.deleted
             RETURNING num",
        )?
        .query_row(
            params![
                id,
                n.document,
                parent_id,
                position,
                n.content.node_type,
                identifier,
                Value::Object(attrs).to_string(),
                text,
                n.updated.0 as i64,
                n.deleted,
            ],
            |r| r.get(0),
        )?;

    conn.prepare_cached("DELETE FROM search WHERE rowid = ?1")?.execute([num])?;
    if let Some(text) = text.as_deref().filter(|t| !t.is_empty() && !n.deleted) {
        conn.prepare_cached("INSERT INTO search (rowid, text) VALUES (?1, ?2)")?.execute(params![num, text])?;
    }

    conn.prepare_cached("DELETE FROM edge WHERE source_node = ?1")?.execute([id])?;
    if n.is_edge() && !n.deleted {
        let target = edge_target(&n.content.node_type, &n.content.fields);
        let target_node = target.and_then(|t| t.parse::<NodeId>().ok());
        let target_doc = target.and_then(|t| t.parse::<DocumentId>().ok());
        conn.prepare_cached(
            "INSERT INTO edge (source_node, source_doc, kind, target, target_node, target_doc, relation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            id,
            n.document,
            n.content.node_type,
            target,
            target_node,
            target_doc,
            n.relation.as_ref().map(|r| r.0.as_str()),
        ])?;
    }
    Ok(())
}

/// Where an edge node points, as written. In-document link anchors (`#x`)
/// become the bare label so they join with `node.identifier`.
fn edge_target<'a>(node_type: &str, fields: &'a serde_json::Map<String, Value>) -> Option<&'a str> {
    let raw = match node_type {
        "link" => fields.get("url"),
        "crossReference" => fields.get("identifier").or_else(|| fields.get("label")),
        "cite" => fields.get("label"),
        _ => None,
    }?
    .as_str()?;
    Some(raw.strip_prefix('#').unwrap_or(raw))
}

fn position(state: &ProjectState, parent: Parent, id: NodeId) -> i64 {
    state
        .child_slots(parent)
        .iter()
        .position(|&c| c == id)
        .expect("a node is among its parent's children") as i64
}

fn renumber(conn: &Connection, state: &ProjectState, parent: Parent) -> Result<()> {
    let mut stmt = conn.prepare_cached("UPDATE node SET position = ?2 WHERE id = ?1 AND position <> ?2")?;
    for (i, child) in state.child_slots(parent).iter().enumerate() {
        stmt.execute(params![child, i as i64])?;
    }
    Ok(())
}

fn write_change_set(conn: &Connection, state: &ProjectState, id: crate::ids::ChangeSetId) -> Result<()> {
    let cs = state.change_set(id).expect("effects name existing change sets");
    conn.prepare_cached(
        "INSERT INTO change_set (id, person_id, agent_id, base, rationale, state, opened_seq, closed_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, closed_seq = excluded.closed_seq",
    )?
    .execute(params![
        id,
        cs.author.person(),
        cs.author.agent(),
        cs.base.0 as i64,
        cs.rationale,
        cs.status.as_str(),
        cs.opened.0 as i64,
        cs.closed.map(|v| v.0 as i64),
    ])?;
    Ok(())
}

/// Every projection row as sorted text, without file-local row numbers, so
/// two files can be compared.
pub(super) fn snapshot(conn: &Connection) -> Result<Vec<String>> {
    const QUERIES: [&str; 6] = [
        "SELECT 'registry ' || namespace || ' ' || quote(name) || ' ' || status || ' ' || entry FROM registry",
        "SELECT 'document ' || quote(id) || ' ' || quote(kind) || ' ' || quote(title) || ' '
                || created_seq || ' ' || updated_seq || ' ' || deleted FROM document",
        "SELECT 'node ' || quote(id) || ' ' || quote(document_id) || ' ' || quote(parent_id) || ' '
                || position || ' ' || quote(type) || ' ' || quote(identifier) || ' ' || quote(attrs) || ' '
                || quote(text) || ' ' || updated_seq || ' ' || deleted FROM node",
        "SELECT 'edge ' || quote(source_node) || ' ' || quote(source_doc) || ' ' || kind || ' '
                || quote(target) || ' ' || quote(target_node) || ' ' || quote(target_doc) || ' '
                || quote(relation) FROM edge",
        "SELECT 'change_set ' || quote(id) || ' ' || quote(person_id) || ' ' || quote(agent_id) || ' '
                || base || ' ' || quote(rationale) || ' ' || state || ' ' || opened_seq || ' '
                || quote(closed_seq) FROM change_set",
        "SELECT 'search ' || quote(node.id) || ' ' || quote(search.text)
         FROM search JOIN node ON node.num = search.rowid",
    ];
    let mut lines = Vec::new();
    for sql in QUERIES {
        let mut stmt = conn.prepare(sql)?;
        for line in stmt.query_map([], |r| r.get::<_, String>(0))? {
            lines.push(line?);
        }
    }
    lines.sort();
    Ok(lines)
}
