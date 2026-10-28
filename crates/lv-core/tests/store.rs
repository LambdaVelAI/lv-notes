//! The op log persists exactly what was appended and refuses bad appends.

use lv_core::ids::{AgentId, ChangeSetId, DocumentId, GrantId, NodeId, OpId, PersonId, ProjectId, Version};
use lv_core::op::{DocKind, NodeContent, Op, OpBody, Parent, Relation, Sequenced, TextEdit, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::store::{Store, StoreError};
use serde_json::{Map, json};

struct Fixture {
    store: Store,
    owner: Grant,
    agent: Grant,
}

impl Fixture {
    fn new(store: Store) -> Fixture {
        let person = PersonId::new();
        let agent = AgentId::new();
        let owner = Grant::new(Principal::Person { person }, GrantScope::Owner);
        let delegated = Grant::new(Principal::Agent { agent, on_behalf_of: person }, GrantScope::Delegated);
        store.add_person(person, "A. Scholar").unwrap();
        store.add_agent(agent, "In-app agent").unwrap();
        store.add_grant(&owner).unwrap();
        store.add_grant(&delegated).unwrap();
        Fixture { store, owner, agent: delegated }
    }

    fn in_memory() -> Fixture {
        Fixture::new(Store::create_in_memory(ProjectId::new(), "Test project").unwrap())
    }

    fn op(&self, grant: &Grant, base: u64, body: OpBody) -> Op {
        Op {
            id: OpId::new(),
            project: self.store.project(),
            principal: grant.holder,
            grant: grant.id,
            base: Version(base),
            change_set: None,
            created_at: Timestamp::now(),
            body,
        }
    }
}

fn at(seq: u64, op: Op) -> Sequenced {
    Sequenced { seq: Version(seq), op }
}

/// A short history that uses every principal kind, a change set and most op kinds.
fn history(f: &Fixture) -> Vec<Sequenced> {
    let doc = DocumentId::new();
    let para = NodeId::new();
    let link = NodeId::new();
    let cs = ChangeSetId::new();
    let in_cs = |mut op: Op| {
        op.change_set = Some(cs);
        op
    };
    vec![
        at(1, f.op(&f.owner, 0, OpBody::CreateDocument { doc, kind: DocKind::note(), title: "Reading".into() })),
        at(2, f.op(&f.owner, 1, OpBody::InsertNode {
            node: para,
            parent: Parent::DocumentRoot(doc),
            after: None,
            content: NodeContent { node_type: "paragraph".into(), fields: Map::new() },
        })),
        at(3, f.op(&f.owner, 2, OpBody::InsertNode {
            node: link,
            parent: Parent::Node(para),
            after: None,
            content: NodeContent {
                node_type: "link".into(),
                fields: json!({ "url": "#fig-graph" }).as_object().unwrap().clone(),
            },
        })),
        at(4, in_cs(f.op(&f.agent, 3, OpBody::OpenChangeSet { change_set: cs, rationale: "Type the link".into() }))),
        at(5, in_cs(f.op(&f.agent, 3, OpBody::SetRelation { node: link, relation: Some(Relation::new(Relation::SUPPORTS)) }))),
        at(6, f.op(&f.owner, 5, OpBody::AcceptChangeSet { change_set: cs })),
        at(7, f.op(&f.owner, 6, OpBody::EditText { node: para, edit: TextEdit::Replace { value: "Hi".into() } })),
    ]
}

#[test]
fn appended_ops_read_back_identically() {
    let mut f = Fixture::in_memory();
    assert_eq!(f.store.head().unwrap(), Version::EMPTY);

    let ops = history(&f);
    f.store.append(&ops[..3]).unwrap();
    f.store.append(&ops[3..]).unwrap();

    assert_eq!(f.store.head().unwrap(), Version(7));
    assert_eq!(f.store.ops_since(Version::EMPTY).unwrap(), ops);
    assert_eq!(f.store.ops_since(Version(5)).unwrap(), ops[5..]);
    assert_eq!(f.store.op(ops[4].op.id).unwrap().as_ref(), Some(&ops[4]));
    assert_eq!(f.store.op(OpId::new()).unwrap(), None);

    let agent_op = &f.store.ops_since(Version(3)).unwrap()[0].op;
    assert!(matches!(agent_op.principal, Principal::Agent { .. }));
    assert_eq!(agent_op.grant, f.agent.id);
}

#[test]
fn project_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.lvn");
    let project = ProjectId::new();

    let (ops, grant) = {
        let mut f = Fixture::new(Store::create(&path, project, "On disk").unwrap());
        let ops = history(&f);
        f.store.append(&ops).unwrap();
        (ops, f.agent.clone())
    };

    let store = Store::open(&path).unwrap();
    assert_eq!(store.project(), project);
    assert_eq!(store.project_name().unwrap(), "On disk");
    assert_eq!(store.ops_since(Version::EMPTY).unwrap(), ops);
    assert_eq!(store.grant(grant.id).unwrap(), Some(grant));

    assert!(matches!(Store::create(&path, project, "again"), Err(StoreError::AlreadyExists(_))));
}

#[test]
fn open_rejects_files_that_are_not_projects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.sqlite");
    // A zero-length file is a valid, empty SQLite database.
    std::fs::write(&path, b"").unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::NotAProject)));
    assert!(Store::open(dir.path().join("missing.lvn")).is_err());
}

#[test]
fn auto_accept_toggle_persists() {
    let f = Fixture::in_memory();
    assert!(!f.store.grant(f.agent.id).unwrap().unwrap().auto_accept);
    assert!(f.store.set_auto_accept(f.agent.id, true).unwrap());
    assert!(f.store.grant(f.agent.id).unwrap().unwrap().auto_accept);
    assert!(f.store.set_auto_accept(f.agent.id, false).unwrap());
    assert!(!f.store.grant(f.agent.id).unwrap().unwrap().auto_accept);
    assert!(!f.store.set_auto_accept(GrantId::new(), true).unwrap());
}

#[test]
fn open_rejects_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.lvn");
    drop(Store::create(&path, ProjectId::new(), "Future").unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    assert!(matches!(Store::open(&path), Err(StoreError::SchemaTooNew { found: 99 })));
}

#[test]
fn rejects_gaps_wrong_project_and_bad_base() {
    let mut f = Fixture::in_memory();
    let ops = history(&f);

    let gap = f.store.append(&[ops[1].clone()]).unwrap_err();
    assert!(matches!(gap, StoreError::NotContiguous { expected: Version(1), found: Version(2), .. }), "{gap}");

    let mut foreign = ops[0].clone();
    foreign.op.project = ProjectId::new();
    assert!(matches!(f.store.append(&[foreign]), Err(StoreError::WrongProject { .. })));

    let mut future_base = ops[0].clone();
    future_base.op.base = Version(1);
    assert!(matches!(f.store.append(&[future_base]), Err(StoreError::BaseNotBeforeSeq { .. })));

    assert_eq!(f.store.head().unwrap(), Version::EMPTY);
}

#[test]
fn rejects_duplicate_ops() {
    let mut f = Fixture::in_memory();
    let ops = history(&f);
    f.store.append(&ops[..1]).unwrap();
    let replay = at(2, ops[0].op.clone());
    assert!(matches!(f.store.append(&[replay]), Err(StoreError::DuplicateOp(_))));
}

#[test]
fn rejects_unregistered_principals_and_grants() {
    let mut f = Fixture::in_memory();
    let ops = history(&f);

    let mut stranger = ops[0].clone();
    stranger.op.principal = Principal::Person { person: PersonId::new() };
    assert!(matches!(f.store.append(&[stranger]), Err(StoreError::Sqlite(_))));

    let mut no_grant = ops[0].clone();
    no_grant.op.grant = GrantId::new();
    assert!(matches!(f.store.append(&[no_grant]), Err(StoreError::Sqlite(_))));
}

#[test]
fn failed_batch_appends_nothing() {
    let mut f = Fixture::in_memory();
    let mut ops = history(&f);
    ops[2].op.grant = GrantId::new(); // third op of the batch is bad
    assert!(f.store.append(&ops[..4]).is_err());
    assert_eq!(f.store.head().unwrap(), Version::EMPTY);
    assert!(f.store.ops_since(Version::EMPTY).unwrap().is_empty());
}
