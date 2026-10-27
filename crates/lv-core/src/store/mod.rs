//! Project storage: one SQLite file per project.
//!
//! The store persists the op log, the principal registry and the
//! projections. It checks storage integrity (right project, contiguous
//! positions, known principals and grants) but not permissions: those go
//! through the sequencer's single permission check.
//!
//! Projections are written in the same transaction as the ops they reflect
//! ([`Store::commit`]), so they never drift from the log. If they are behind
//! or were built by another version, the next commit or
//! [`Store::rebuild_projections`] rebuilds them from the state.

mod projection;

pub use projection::{PROJECTION_VERSION, PROJECTIONS};

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::ids::{AgentId, ChangeSetId, DocumentId, GrantId, NodeId, OpId, PersonId, ProjectId, Version};
use crate::op::{Op, OpBody, Parent, Relation, Sequenced, Timestamp};
use crate::principal::{Grant, GrantScope, Principal};
use crate::state::{Effects, ProjectState};

/// `PRAGMA application_id` of lv-notes project files ("LVNP").
pub const APPLICATION_ID: i32 = 0x4C56_4E50;
/// Schema version this code writes, stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 1;
/// Encoding of `op_log.body`: 1 = JSON of [`OpBody`].
pub const BODY_FORMAT: i64 = 1;

const SCHEMA_V1: &str = include_str!("schema_v1.sql");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("op body is not valid JSON for this version: {0}")]
    Body(#[from] serde_json::Error),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("not an lv-notes project file")]
    NotAProject,
    #[error("project file has schema version {found}; this build supports up to {SCHEMA_VERSION}")]
    SchemaTooNew { found: i32 },
    #[error("op {op} belongs to project {found}, not {expected}")]
    WrongProject { op: OpId, expected: ProjectId, found: ProjectId },
    #[error("op {op} has position {found}, expected {expected}")]
    NotContiguous { op: OpId, expected: Version, found: Version },
    #[error("op {op} at {seq} claims base {base}, which is not before it")]
    BaseNotBeforeSeq { op: OpId, seq: Version, base: Version },
    #[error("op {0} is already in the log")]
    DuplicateOp(OpId),
    #[error("op at {seq} has unsupported body format {format}")]
    UnsupportedBodyFormat { seq: Version, format: i64 },
    #[error("grant {0} has an unknown scope")]
    BadGrantScope(GrantId),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// An open project file.
pub struct Store {
    conn: Connection,
    project: ProjectId,
}

impl Store {
    /// Creates a new project file at `path`. Fails if the file exists.
    pub fn create(path: impl AsRef<Path>, project: ProjectId, name: &str) -> Result<Store> {
        let path = path.as_ref();
        if path.exists() {
            return Err(StoreError::AlreadyExists(path.display().to_string()));
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, project, name)
    }

    /// Creates a project that lives only in memory (tests, scratch).
    pub fn create_in_memory(project: ProjectId, name: &str) -> Result<Store> {
        Self::init(Connection::open_in_memory()?, project, name)
    }

    /// Opens an existing project file.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        Self::configure(&conn)?;
        let app_id: i32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if app_id != APPLICATION_ID {
            return Err(StoreError::NotAProject);
        }
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew { found: version });
        }
        // Migrations from older versions go here once there is a version 2.
        let project = conn.query_row("SELECT id FROM project", [], |r| r.get(0))?;
        Ok(Store { conn, project })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(())
    }

    fn init(mut conn: Connection, project: ProjectId, name: &str) -> Result<Store> {
        Self::configure(&conn)?;
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.execute(
            "INSERT INTO project (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![project, name, Timestamp::now().0],
        )?;
        projection::mark_built(&tx, Version::EMPTY)?;
        tx.commit()?;
        Ok(Store { conn, project })
    }

    pub fn project(&self) -> ProjectId {
        self.project
    }

    pub fn project_name(&self) -> Result<String> {
        Ok(self.conn.query_row("SELECT name FROM project", [], |r| r.get(0))?)
    }

    // ---- Principals and grants -------------------------------------------

    pub fn add_person(&self, id: PersonId, display_name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO person (id, display_name, created_at) VALUES (?1, ?2, ?3)",
            params![id, display_name, Timestamp::now().0],
        )?;
        Ok(())
    }

    pub fn add_agent(&self, id: AgentId, display_name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent (id, display_name, created_at) VALUES (?1, ?2, ?3)",
            params![id, display_name, Timestamp::now().0],
        )?;
        Ok(())
    }

    pub fn add_grant(&self, grant: &Grant) -> Result<()> {
        self.conn.execute(
            "INSERT INTO grants (id, person_id, agent_id, scope, auto_accept, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                grant.id,
                grant.holder.person(),
                grant.holder.agent(),
                grant.scope.as_str(),
                grant.auto_accept,
                Timestamp::now().0,
            ],
        )?;
        Ok(())
    }

    pub fn grant(&self, id: GrantId) -> Result<Option<Grant>> {
        let row = self
            .conn
            .query_row(
                "SELECT person_id, agent_id, scope, auto_accept FROM grants WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, PersonId>(0)?,
                        r.get::<_, Option<AgentId>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(person, agent, scope, auto_accept)| {
            let scope = GrantScope::parse(&scope).ok_or(StoreError::BadGrantScope(id))?;
            Ok(Grant { id, holder: principal(person, agent), scope, auto_accept })
        })
        .transpose()
    }

    pub fn has_person(&self, id: PersonId) -> Result<bool> {
        let n: i64 = self.conn.query_row("SELECT count(*) FROM person WHERE id = ?1", [id], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Unrevoked grants held by `holder`, oldest first.
    pub fn grants_held_by(&self, holder: Principal) -> Result<Vec<Grant>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM grants
             WHERE person_id = ?1 AND agent_id IS ?2 AND revoked_at IS NULL
             ORDER BY created_at, id",
        )?;
        let ids = stmt
            .query_map(params![holder.person(), holder.agent()], |r| r.get::<_, GrantId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().filter_map(|id| self.grant(id).transpose()).collect()
    }

    /// Turns auto-accept on or off for a grant. Takes effect on the next
    /// submitted op. Returns false if there is no such grant.
    pub fn set_auto_accept(&self, id: GrantId, on: bool) -> Result<bool> {
        let changed = self.conn.execute("UPDATE grants SET auto_accept = ?2 WHERE id = ?1", params![id, on])?;
        Ok(changed == 1)
    }

    // ---- Op log ------------------------------------------------------------

    /// The position of the last op in the log; [`Version::EMPTY`] if none.
    pub fn head(&self) -> Result<Version> {
        head(&self.conn)
    }

    /// Appends sequenced ops atomically: all of them or none.
    ///
    /// Positions must continue the log without gaps, every op must belong to
    /// this project and be new, and its principal and grant must be
    /// registered. This writes the log only and leaves projections behind;
    /// prefer [`Store::commit`].
    pub fn append(&mut self, batch: &[Sequenced]) -> Result<()> {
        let tx = self.conn.transaction()?;
        append_log(&tx, self.project, batch)?;
        tx.commit()?;
        Ok(())
    }

    /// Appends `batch` and updates the projections to `state`, the project
    /// after the batch, in one transaction. `effects` names what the batch
    /// changed; if the projections were not current before the batch they
    /// are rebuilt from `state` instead.
    pub fn commit(&mut self, batch: &[Sequenced], state: &ProjectState, effects: &Effects) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        let was_current = projection::is_current(&tx, head(&tx)?)?;
        append_log(&tx, self.project, batch)?;
        if was_current {
            projection::write_effects(&tx, state, effects)?;
        } else {
            projection::rebuild(&tx, state)?;
        }
        projection::mark_built(&tx, head(&tx)?)?;
        tx.commit()?;
        Ok(())
    }

    /// Whether the projections were built by this version up to the head.
    pub fn projections_current(&self) -> Result<bool> {
        projection::is_current(&self.conn, self.head()?)
    }

    /// Rewrites every projection from `state`, which must be the project at
    /// the log's head (e.g. from replaying the log).
    pub fn rebuild_projections(&mut self, state: &ProjectState) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        projection::rebuild(&tx, state)?;
        projection::mark_built(&tx, head(&tx)?)?;
        tx.commit()?;
        Ok(())
    }

    /// Every projection row as sorted text, for comparing two files.
    #[doc(hidden)]
    pub fn projection_snapshot(&self) -> Result<Vec<String>> {
        projection::snapshot(&self.conn)
    }

    // ---- Projection queries -------------------------------------------------

    /// Live children of `parent`, in order.
    pub fn children(&self, parent: Parent) -> Result<Vec<NodeId>> {
        let (doc, parent_id): (Option<DocumentId>, Option<NodeId>) = match parent {
            Parent::DocumentRoot(doc) => (Some(doc), None),
            Parent::Node(id) => (None, Some(id)),
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM node
             WHERE deleted = 0
               AND ((?1 IS NOT NULL AND document_id = ?1 AND parent_id IS NULL) OR parent_id = ?2)
             ORDER BY position",
        )?;
        let rows = stmt.query_map(params![doc, parent_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Live nodes whose text matches an FTS5 `query`, best match first.
    pub fn search(&self, query: &str) -> Result<Vec<NodeId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT node.id FROM search
             JOIN node ON node.num = search.rowid
             JOIN document ON document.id = node.document_id AND document.deleted = 0
             WHERE search MATCH ?1
             ORDER BY rank",
        )?;
        let rows = stmt.query_map([query], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Live links, references and citations pointing at `target`, either by
    /// its ID or by its MyST label, with their relation if typed.
    pub fn backlinks(&self, target: NodeId) -> Result<Vec<(NodeId, Option<Relation>)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT edge.source_node, edge.relation FROM edge
             JOIN document ON document.id = edge.source_doc AND document.deleted = 0
             WHERE edge.target_node = ?1
                OR edge.target = (SELECT identifier FROM node WHERE id = ?1 AND deleted = 0)
             ORDER BY edge.source_node",
        )?;
        let rows = stmt.query_map([target], |r| {
            Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.map(Relation)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All ops after position `after`, in log order.
    pub fn ops_since(&self, after: Version) -> Result<Vec<Sequenced>> {
        let mut stmt = self.conn.prepare_cached(&format!("{SELECT_OP} WHERE seq > ?1 ORDER BY seq"))?;
        let rows = stmt.query_map([after.0 as i64], RawOp::from_row)?;
        rows.map(|row| row?.into_sequenced(self.project)).collect()
    }

    /// The op with this ID, if it is in the log.
    pub fn op(&self, id: OpId) -> Result<Option<Sequenced>> {
        let mut stmt = self.conn.prepare_cached(&format!("{SELECT_OP} WHERE op_id = ?1"))?;
        stmt.query_row([id], RawOp::from_row)
            .optional()?
            .map(|raw| raw.into_sequenced(self.project))
            .transpose()
    }
}

fn principal(person: PersonId, agent: Option<AgentId>) -> Principal {
    match agent {
        None => Principal::Person { person },
        Some(agent) => Principal::Agent { agent, on_behalf_of: person },
    }
}

fn append_log(tx: &Transaction<'_>, project: ProjectId, batch: &[Sequenced]) -> Result<()> {
    let mut expected = head(tx)?.next();
    for sequenced in batch {
        check(project, expected, sequenced)?;
        insert(tx, sequenced)?;
        expected = expected.next();
    }
    Ok(())
}

fn head(conn: &Connection) -> Result<Version> {
    let seq: i64 = conn.query_row("SELECT coalesce(max(seq), 0) FROM op_log", [], |r| r.get(0))?;
    Ok(Version(seq as u64))
}

fn check(project: ProjectId, expected: Version, s: &Sequenced) -> Result<()> {
    let op = &s.op;
    if op.project != project {
        return Err(StoreError::WrongProject { op: op.id, expected: project, found: op.project });
    }
    if s.seq != expected {
        return Err(StoreError::NotContiguous { op: op.id, expected, found: s.seq });
    }
    if op.base >= s.seq {
        return Err(StoreError::BaseNotBeforeSeq { op: op.id, seq: s.seq, base: op.base });
    }
    Ok(())
}

fn insert(tx: &Transaction<'_>, s: &Sequenced) -> Result<()> {
    let op = &s.op;
    let exists: Option<i64> = tx
        .query_row("SELECT 1 FROM op_log WHERE op_id = ?1", [op.id], |r| r.get(0))
        .optional()?;
    if exists.is_some() {
        return Err(StoreError::DuplicateOp(op.id));
    }
    tx.execute(
        "INSERT INTO op_log
           (seq, op_id, base, person_id, agent_id, grant_id, change_set,
            created_at, kind, body, body_format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            s.seq.0 as i64,
            op.id,
            op.base.0 as i64,
            op.principal.person(),
            op.principal.agent(),
            op.grant,
            op.change_set,
            op.created_at.0,
            op.body.kind(),
            serde_json::to_string(&op.body)?,
            BODY_FORMAT,
        ],
    )?;
    Ok(())
}

const SELECT_OP: &str = "SELECT seq, op_id, base, person_id, agent_id, grant_id, change_set,
                                created_at, body, body_format
                         FROM op_log";

/// An `op_log` row before its body is decoded.
struct RawOp {
    seq: i64,
    id: OpId,
    base: i64,
    person: PersonId,
    agent: Option<AgentId>,
    grant: GrantId,
    change_set: Option<ChangeSetId>,
    created_at: i64,
    body: String,
    body_format: i64,
}

impl RawOp {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawOp> {
        Ok(RawOp {
            seq: r.get(0)?,
            id: r.get(1)?,
            base: r.get(2)?,
            person: r.get(3)?,
            agent: r.get(4)?,
            grant: r.get(5)?,
            change_set: r.get(6)?,
            created_at: r.get(7)?,
            body: r.get(8)?,
            body_format: r.get(9)?,
        })
    }

    fn into_sequenced(self, project: ProjectId) -> Result<Sequenced> {
        let seq = Version(self.seq as u64);
        if self.body_format != BODY_FORMAT {
            return Err(StoreError::UnsupportedBodyFormat { seq, format: self.body_format });
        }
        let body: OpBody = serde_json::from_str(&self.body)?;
        Ok(Sequenced {
            seq,
            op: Op {
                id: self.id,
                project,
                principal: principal(self.person, self.agent),
                grant: self.grant,
                base: Version(self.base as u64),
                change_set: self.change_set,
                created_at: Timestamp(self.created_at),
                body,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every schema table exists, including the FTS5 projection.
    #[test]
    fn schema_creates_all_tables() {
        let store = Store::create_in_memory(ProjectId::new(), "t").unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'search_%' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(
            names,
            [
                "agent", "change_set", "document", "edge", "grants", "node", "op_log", "person",
                "project", "projection_meta", "search",
            ]
        );
    }

    #[test]
    fn op_log_is_append_only() {
        let project = ProjectId::new();
        let mut store = Store::create_in_memory(project, "t").unwrap();
        let person = PersonId::new();
        let grant = Grant::new(Principal::Person { person }, GrantScope::Owner);
        store.add_person(person, "P").unwrap();
        store.add_grant(&grant).unwrap();
        let op = Op {
            id: OpId::new(),
            project,
            principal: grant.holder,
            grant: grant.id,
            base: Version::EMPTY,
            change_set: None,
            created_at: Timestamp::now(),
            body: OpBody::DeleteNode { node: crate::ids::NodeId::new() },
        };
        store.append(&[Sequenced { seq: Version(1), op }]).unwrap();

        for sql in ["UPDATE op_log SET kind = 'x'", "DELETE FROM op_log"] {
            let err = store.conn.execute(sql, []).unwrap_err();
            assert!(err.to_string().contains("append-only"), "{sql}: {err}");
        }
        assert_eq!(store.head().unwrap(), Version(1));
    }
}
