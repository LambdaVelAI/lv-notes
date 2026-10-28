//! The project schema registry: declared vocabulary, the one conformance
//! check, renames and merges, lenient import, and the store's projection
//! and migration.

use lv_core::convert::import_document;
use lv_core::ids::{AgentId, ChangeSetId, DocumentId, NodeId, OpId, PersonId, ProjectId, Version};
use lv_core::myst::Node;
use lv_core::op::{DocKind, NodeContent, Op, OpBody, Parent, Relation, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::refusal::RefusalCode;
use lv_core::schema::{KindEntry, KindStatus, Namespace, bootstrap, builtins, declare_unknown};
use lv_core::sequencer::{Sequencer, SubmitError};
use lv_core::store::Store;
use serde_json::{Value, json};

struct World {
    store: Store,
    seq: Sequencer,
    person: Grant,
    agent: Grant,
}

impl World {
    fn new() -> World {
        World::with(|p| Store::create_in_memory(p, "Registry").unwrap())
    }

    fn with(store: impl FnOnce(ProjectId) -> Store) -> World {
        let project = ProjectId::new();
        let person = PersonId::new();
        let w = World {
            store: store(project),
            seq: Sequencer::new(project),
            person: Grant::new(Principal::Person { person }, GrantScope::Owner),
            agent: Grant::new(Principal::Agent { agent: AgentId::new(), on_behalf_of: person }, GrantScope::Delegated),
        };
        w.store.add_person(person, "A. Scholar").unwrap();
        w.store.add_agent(w.agent.holder.agent().unwrap(), "Agent").unwrap();
        w.store.add_grant(&w.person).unwrap();
        w.store.add_grant(&w.agent).unwrap();
        w
    }

    /// A project with the built-in vocabulary declared.
    fn declared() -> World {
        let mut w = World::new();
        w.submit(builtins()).unwrap();
        w
    }

    fn submit_as(&mut self, grant: &Grant, change_set: Option<ChangeSetId>, bodies: Vec<OpBody>) -> Result<(), RefusalCode> {
        let batch = bodies
            .into_iter()
            .map(|body| Op {
                id: OpId::new(),
                project: self.seq.project(),
                principal: grant.holder,
                grant: grant.id,
                base: self.seq.head(),
                change_set,
                created_at: Timestamp::now(),
                body,
            })
            .collect();
        self.seq.submit(&mut self.store, batch).map(drop).map_err(|e| match e {
            SubmitError::Refused(r) => r.code,
            SubmitError::Log(e) => panic!("{e}"),
        })
    }

    fn submit(&mut self, bodies: Vec<OpBody>) -> Result<(), RefusalCode> {
        let person = self.person.clone();
        self.submit_as(&person, None, bodies)
    }

    fn doc(&mut self, kind: &str) -> Result<DocumentId, RefusalCode> {
        let doc = DocumentId::new();
        self.submit(vec![OpBody::CreateDocument { doc, kind: DocKind::new(kind), title: kind.into() }]).map(|_| doc)
    }

    fn insert(&mut self, doc: DocumentId, node_type: &str, fields: Value) -> Result<NodeId, RefusalCode> {
        let node = NodeId::new();
        let content = NodeContent { node_type: node_type.into(), fields: fields.as_object().unwrap().clone() };
        self.submit(vec![OpBody::InsertNode { node, parent: Parent::DocumentRoot(doc), after: None, content }]).map(|_| node)
    }

    fn set(&mut self, node: NodeId, fields: Value) -> Result<(), RefusalCode> {
        self.submit(vec![OpBody::SetAttrs { node, set: fields.as_object().unwrap().clone(), unset: vec![] }])
    }

    fn relate(&mut self, node: NodeId, relation: &str) -> Result<(), RefusalCode> {
        self.submit(vec![OpBody::SetRelation { node, relation: Some(Relation::new(relation)) }])
    }

    fn kind_of(&self, node: NodeId) -> String {
        self.seq.state().node(node).unwrap().content.fields["kind"].as_str().unwrap().to_owned()
    }
}

fn declare(namespace: Namespace, name: &str, entry: KindEntry) -> OpBody {
    OpBody::DeclareKind { namespace, name: name.into(), entry }
}

fn retire(namespace: Namespace, name: &str, label: &str) -> OpBody {
    let mut entry = KindEntry::new(label);
    entry.status = KindStatus::Retired;
    OpBody::UpdateKind { namespace, name: name.into(), entry }
}

#[test]
fn a_project_without_declarations_is_not_checked() {
    let mut w = World::new();
    let doc = w.doc("anything").unwrap();
    let node = w.insert(doc, "proof", json!({ "kind": "made-up" })).unwrap();
    w.submit(vec![OpBody::InsertNode {
        node: NodeId::new(),
        parent: Parent::Node(node),
        after: None,
        content: NodeContent { node_type: "link".into(), fields: json!({ "url": "x" }).as_object().unwrap().clone() },
    }])
    .unwrap();
    assert!(!w.seq.state().registry().is_enforced());
}

#[test]
fn ops_must_use_declared_kinds() {
    let mut w = World::declared();
    assert_eq!(w.doc("article"), Err(RefusalCode::UndeclaredKind));
    let doc = w.doc("note").unwrap();

    let theorem = w.insert(doc, "proof", json!({ "kind": "theorem" })).unwrap();
    assert_eq!(w.insert(doc, "proof", json!({ "kind": "theorm" })), Err(RefusalCode::UndeclaredKind));
    assert_eq!(w.insert(doc, "admonition", json!({ "kind": "rant" })), Err(RefusalCode::UndeclaredKind));
    w.insert(doc, "admonition", json!({})).expect("no kind, nothing to check");
    w.insert(doc, "mystDirective", json!({ "name": "anything" })).expect("directive names are not checked");

    // Changing the kind is a new use; changing anything else isn't.
    assert_eq!(w.set(theorem, json!({ "kind": "nonsense" })), Err(RefusalCode::UndeclaredKind));
    w.set(theorem, json!({ "kind": "lemma" })).unwrap();
    w.set(theorem, json!({ "label": "thm-1" })).unwrap();

    let link = w.insert(doc, "link", json!({ "url": "https://example.org" })).unwrap();
    w.relate(link, "supports").unwrap();
    assert_eq!(w.relate(link, "backs"), Err(RefusalCode::UndeclaredKind));
    w.submit(vec![declare(Namespace::Relation, "backs", KindEntry::new("Backs"))]).unwrap();
    w.relate(link, "backs").unwrap();
    w.submit(vec![OpBody::SetRelation { node: link, relation: None }]).expect("untyping is always fine");
}

#[test]
fn retired_kinds_keep_their_uses_but_take_no_new_ones() {
    let mut w = World::declared();
    let doc = w.doc("note").unwrap();
    let remark = w.insert(doc, "proof", json!({ "kind": "remark" })).unwrap();
    w.submit(vec![retire(Namespace::Block, "proof:remark", "Remark")]).unwrap();

    w.set(remark, json!({ "label": "r1" })).expect("the existing use stays valid");
    assert_eq!(w.insert(doc, "proof", json!({ "kind": "remark" })), Err(RefusalCode::RetiredKind));
    w.submit(vec![retire(Namespace::Document, "draft", "Draft")]).unwrap();
    assert_eq!(w.doc("draft"), Err(RefusalCode::RetiredKind));
}

#[test]
fn declarations_are_checked() {
    let mut w = World::declared();
    let invalid = [
        declare(Namespace::Block, "theorem", KindEntry::new("Theorem")),
        declare(Namespace::Block, "heading:big", KindEntry::new("Big")),
        declare(Namespace::Relation, " padded", KindEntry::new("Padded")),
        declare(Namespace::Relation, "refutes", KindEntry { counter: Some("x".into()), ..KindEntry::new("Refutes") }),
        declare(Namespace::Relation, "refutes", KindEntry::new("  ")),
    ];
    for op in invalid {
        assert_eq!(w.submit(vec![op.clone()]), Err(RefusalCode::InvalidOp), "{op:?}");
    }
    assert_eq!(w.submit(vec![declare(Namespace::Relation, "supports", KindEntry::new("Supports"))]), Err(RefusalCode::KindExists));
    assert_eq!(w.submit(vec![retire(Namespace::Relation, "refutes", "Refutes")]), Err(RefusalCode::UnknownKind));
}

#[test]
fn renames_and_merges_rewrite_every_use() {
    let mut w = World::declared();
    let doc = w.doc("note").unwrap();
    let lemma = w.insert(doc, "proof", json!({ "kind": "lemma" })).unwrap();
    let gone = w.insert(doc, "proof", json!({ "kind": "lemma" })).unwrap();
    w.submit(vec![OpBody::DeleteNode { node: gone }]).unwrap();
    let link = w.insert(doc, "link", json!({ "url": "https://example.org" })).unwrap();
    w.relate(link, "uses").unwrap();

    let rename = |from: &str, to: &str| OpBody::RenameKind { namespace: Namespace::Relation, from: from.into(), to: to.into() };
    w.submit(vec![rename("uses", "relies-on")]).unwrap();
    assert_eq!(w.seq.state().node(link).unwrap().relation, Some(Relation::new("relies-on")));
    assert!(w.seq.state().registry().get(Namespace::Relation, "uses").is_none());
    assert_eq!(w.submit(vec![rename("uses", "x")]), Err(RefusalCode::UnknownKind));
    assert_eq!(w.submit(vec![rename("relies-on", "supports")]), Err(RefusalCode::KindExists));

    let merge = |from: &str, into: &str| OpBody::MergeKind { namespace: Namespace::Block, from: from.into(), into: into.into() };
    assert_eq!(w.submit(vec![merge("proof:lemma", "admonition:note")]), Err(RefusalCode::InvalidOp));
    assert_eq!(w.submit(vec![merge("proof:lemma", "proof:lemma")]), Err(RefusalCode::InvalidOp));
    w.submit(vec![merge("proof:lemma", "proof:theorem")]).unwrap();
    assert_eq!((w.kind_of(lemma), w.kind_of(gone)), ("theorem".into(), "theorem".into()), "tombstones too");
    assert_eq!(w.insert(doc, "proof", json!({ "kind": "lemma" })), Err(RefusalCode::UndeclaredKind));

    w.submit(vec![OpBody::RenameKind { namespace: Namespace::Document, from: "note".into(), to: "jotting".into() }]).unwrap();
    assert_eq!(w.seq.state().document(doc).unwrap().kind, DocKind::new("jotting"));
}

#[test]
fn agents_propose_vocabulary_in_change_sets() {
    let mut w = World::declared();
    let cs = ChangeSetId::new();
    let agent = w.agent.clone();
    assert_eq!(
        w.submit_as(&agent, None, vec![declare(Namespace::Relation, "refutes", KindEntry::new("Refutes"))]),
        Err(RefusalCode::Forbidden)
    );
    w.submit_as(
        &agent,
        Some(cs),
        vec![
            OpBody::OpenChangeSet { change_set: cs, rationale: "A relation for counterexamples".into() },
            declare(Namespace::Relation, "refutes", KindEntry::new("Refutes")),
        ],
    )
    .unwrap();
    assert!(w.seq.state().registry().get(Namespace::Relation, "refutes").is_none(), "pending until accepted");
    w.submit(vec![OpBody::AcceptChangeSet { change_set: cs }]).unwrap();
    assert!(w.seq.state().registry().get(Namespace::Relation, "refutes").is_some());
}

#[test]
fn import_declares_unknown_kinds_as_provisional() {
    let mut w = World::declared();
    let root: Node = serde_json::from_value(json!({ "type": "root", "children": [
        { "type": "proof", "kind": "theorem", "children": [] },
        { "type": "proof", "kind": "claim", "children": [] },
        { "type": "paragraph", "children": [{ "type": "link", "url": "x", "relation": "refutes", "children": [] }] },
    ]}))
    .unwrap();
    let ops = import_document(DocumentId::new(), DocKind::new("memo"), "Memo", &root).unwrap();
    assert_eq!(w.submit(ops.clone()), Err(RefusalCode::UndeclaredKind), "ops are strict");

    let declared = declare_unknown(w.seq.state(), &ops);
    let names: Vec<(Namespace, String)> = declared
        .iter()
        .map(|op| match op {
            OpBody::DeclareKind { namespace, name, entry } => {
                assert_eq!(entry.status, KindStatus::Provisional);
                (*namespace, name.clone())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        [(Namespace::Document, "memo".into()), (Namespace::Block, "proof:claim".into()), (Namespace::Relation, "refutes".into())]
    );
    w.submit(declared.into_iter().chain(ops).collect()).unwrap();
    assert_eq!(w.seq.state().registry().get(Namespace::Block, "proof:claim").unwrap().label, "Claim");
}

#[test]
fn bootstrap_declares_builtins_and_what_old_content_uses() {
    let mut w = World::new();
    let doc = w.doc("memo").unwrap();
    w.insert(doc, "proof", json!({ "kind": "theorem" })).unwrap();
    w.insert(doc, "proof", json!({ "kind": "claim" })).unwrap();
    let ops = bootstrap(w.seq.state());
    assert_eq!(ops.len(), builtins().len() + 2);
    w.submit(ops).unwrap();
    let registry = w.seq.state().registry();
    assert!(registry.is_enforced());
    assert_eq!(registry.get(Namespace::Document, "memo").unwrap().status, KindStatus::Provisional);
    assert_eq!(registry.get(Namespace::Block, "proof:theorem").unwrap().status, KindStatus::Declared);
    assert!(bootstrap(w.seq.state()).is_empty(), "only once");
}

#[test]
fn the_registry_is_a_projection_like_the_others() {
    let mut w = World::declared();
    let doc = w.doc("note").unwrap();
    w.insert(doc, "proof", json!({ "kind": "lemma" })).unwrap();
    w.submit(vec![OpBody::MergeKind { namespace: Namespace::Block, from: "proof:lemma".into(), into: "proof:theorem".into() }]).unwrap();
    w.submit(vec![retire(Namespace::Relation, "cites", "Cites")]).unwrap();

    let snapshot = w.store.projection_snapshot().unwrap();
    assert!(snapshot.iter().any(|l| l.starts_with("registry relation 'cites' retired")), "{snapshot:#?}");
    assert!(!snapshot.iter().any(|l| l.contains("'proof:lemma'")));

    let mut fresh = Store::create_in_memory(w.seq.project(), "Registry").unwrap();
    fresh.add_person(w.person.holder.person(), "A. Scholar").unwrap();
    fresh.add_agent(w.agent.holder.agent().unwrap(), "Agent").unwrap();
    fresh.add_grant(&w.person).unwrap();
    fresh.add_grant(&w.agent).unwrap();
    fresh.append(&w.store.ops_since(Version::EMPTY).unwrap()).unwrap();
    let replayed = Sequencer::load(&mut fresh).unwrap();
    assert_eq!(replayed.state(), w.seq.state());
    assert_eq!(fresh.projection_snapshot().unwrap(), snapshot);
}

#[test]
fn version_1_files_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.lv");
    let doc;
    {
        let mut w = World::with(|p| Store::create(&path, p, "Old").unwrap());
        doc = w.doc("note").unwrap();
    }
    // Turn it back into a version 1 file: no registry table, and the old
    // document table with its fixed kinds.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE registry;
             CREATE TABLE document_v1 (
                 id BLOB PRIMARY KEY CHECK (length(id) = 16),
                 kind TEXT NOT NULL CHECK (kind IN ('note', 'draft')),
                 title TEXT NOT NULL, created_seq INTEGER NOT NULL, updated_seq INTEGER NOT NULL,
                 deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
             ) STRICT;
             INSERT INTO document_v1 SELECT * FROM document;
             DROP TABLE document;
             ALTER TABLE document_v1 RENAME TO document;
             DELETE FROM projection_meta WHERE name = 'registry';
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let backup = lv_core::store::backup_path(&path, 1);
    let mut store = Store::open(&path).unwrap();
    let seq = Sequencer::load(&mut store).unwrap();
    // The untouched version 1 file was kept next to it.
    assert_eq!(backup.file_name().unwrap(), "old (before upgrade from v1).lv");
    let kept = rusqlite::Connection::open(&backup).unwrap();
    assert_eq!(kept.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1);
    assert_eq!(kept.query_row("SELECT count(*) FROM op_log", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(lv_core::store::backup_path(&path, 1).file_name().unwrap(), "old (before upgrade from v1) 2.lv");
    assert!(seq.state().document(doc).is_some());
    assert!(store.projections_current().unwrap());
    let snapshot = store.projection_snapshot().unwrap();
    assert!(snapshot.iter().any(|l| l.starts_with("document ") && l.contains("'note'")), "{snapshot:#?}");
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(version, 2);
}
