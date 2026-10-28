//! Projections: incremental updates must equal a rebuild from the log, and
//! the query helpers must agree with the in-memory state.

use std::path::Path;

use lv_core::ids::{AgentId, ChangeSetId, DocumentId, NodeId, OpId, PersonId, ProjectId, Version};
use lv_core::op::{DocKind, NodeContent, Op, OpBody, Parent, Relation, Sequenced, TextEdit, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::sequencer::Sequencer;
use lv_core::store::Store;
use serde_json::{Value, json};

struct World {
    store: Store,
    seq: Sequencer,
    person: Grant,
    agent: Grant,
}

fn register(store: &Store, person: &Grant, agent: &Grant) {
    store.add_person(person.holder.person(), "A. Scholar").unwrap();
    store.add_agent(agent.holder.agent().unwrap(), "Agent").unwrap();
    store.add_grant(person).unwrap();
    store.add_grant(agent).unwrap();
}

impl World {
    fn create(store: impl FnOnce(ProjectId) -> Store) -> World {
        let project = ProjectId::new();
        let person = PersonId::new();
        let world = World {
            store: store(project),
            seq: Sequencer::new(project),
            person: Grant::new(Principal::Person { person }, GrantScope::Owner),
            agent: Grant::new(Principal::Agent { agent: AgentId::new(), on_behalf_of: person }, GrantScope::Delegated),
        };
        register(&world.store, &world.person, &world.agent);
        world
    }

    fn in_memory() -> World {
        World::create(|p| Store::create_in_memory(p, "Projections").unwrap())
    }

    fn on_disk(path: &Path) -> World {
        World::create(|p| Store::create(path, p, "Projections").unwrap())
    }

    fn op(&self, grant: &Grant, change_set: Option<ChangeSetId>, body: OpBody) -> Op {
        Op {
            id: OpId::new(),
            project: self.seq.project(),
            principal: grant.holder,
            grant: grant.id,
            base: self.seq.head(),
            change_set,
            created_at: Timestamp::now(),
            body,
        }
    }

    fn person(&mut self, body: OpBody) -> Vec<Sequenced> {
        let op = self.op(&self.person.clone(), None, body);
        self.seq.submit(&mut self.store, vec![op]).unwrap()
    }

    fn agent(&mut self, cs: ChangeSetId, body: OpBody) -> Vec<Sequenced> {
        let op = self.op(&self.agent.clone(), Some(cs), body);
        self.seq.submit(&mut self.store, vec![op]).unwrap()
    }

    /// The projections a rebuild from scratch would produce now.
    fn rebuilt_snapshot(&self) -> Vec<String> {
        let mut fresh = Store::create_in_memory(self.seq.project(), "Projections").unwrap();
        register(&fresh, &self.person, &self.agent);
        fresh.append(&self.store.ops_since(Version::EMPTY).unwrap()).unwrap();
        let replayed = Sequencer::load(&mut fresh).unwrap();
        assert_eq!(replayed.state(), self.seq.state());
        fresh.projection_snapshot().unwrap()
    }
}

fn content(node_type: &str, fields: Value) -> NodeContent {
    let Value::Object(fields) = fields else { panic!("fields must be an object") };
    NodeContent { node_type: node_type.into(), fields }
}

fn insert(node: NodeId, parent: Parent, after: Option<NodeId>, c: NodeContent) -> OpBody {
    OpBody::InsertNode { node, parent, after, content: c }
}

fn text(node: NodeId, parent: NodeId, value: &str) -> OpBody {
    insert(node, Parent::Node(parent), None, content("text", json!({ "value": value })))
}

type Step = Box<dyn Fn(&mut World)>;

/// Ids of interest in [`history`].
struct Ids {
    note: DocumentId,
    draft: DocumentId,
    heading: NodeId,
    para: NodeId,
    para_text: NodeId,
    id_link: NodeId,
    label_ref: NodeId,
    cite: NodeId,
    proposed_link: NodeId,
}

/// A history touching every projection: documents, nested nodes, text,
/// links by ID and by label, citations, moves across documents, deletes, and
/// change sets that are accepted, rejected and left open. `check` runs after
/// every submit.
fn history(w: &mut World, mut check: impl FnMut(&World)) -> Ids {
    let note = DocumentId::new();
    let draft = DocumentId::new();
    let gone = DocumentId::new();
    let heading = NodeId::new();
    let heading_text = NodeId::new();
    let para = NodeId::new();
    let para_text = NodeId::new();
    let id_link = NodeId::new();
    let label_ref = NodeId::new();
    let cite = NodeId::new();
    let scrap = NodeId::new();
    let proposed = NodeId::new();
    let proposed_link = NodeId::new();
    let (accepted, rejected, pending) = (ChangeSetId::new(), ChangeSetId::new(), ChangeSetId::new());
    let root = |d| Parent::DocumentRoot(d);

    let steps: Vec<Step> = vec![
        Box::new(move |w| { w.person(OpBody::CreateDocument { doc: note, kind: DocKind::note(), title: "Reading".into() }); }),
        Box::new(move |w| { w.person(OpBody::CreateDocument { doc: draft, kind: DocKind::draft(), title: "Essay".into() }); }),
        Box::new(move |w| { w.person(OpBody::CreateDocument { doc: gone, kind: DocKind::note(), title: "Scratch".into() }); }),
        Box::new(move |w| { w.person(insert(heading, root(note), None, content("heading", json!({ "depth": 1, "identifier": "slow-reading", "label": "Slow reading" })))); }),
        Box::new(move |w| { w.person(text(heading_text, heading, "Slow reading")); }),
        Box::new(move |w| { w.person(insert(para, root(note), Some(heading), content("paragraph", json!({})))); }),
        Box::new(move |w| { w.person(text(para_text, para, "Marginalia record a reader thinking")); }),
        Box::new(move |w| { w.person(insert(id_link, Parent::Node(para), Some(para_text), content("link", json!({ "url": heading.to_string() })))); }),
        Box::new(move |w| { w.person(insert(label_ref, Parent::Node(para), Some(id_link), content("crossReference", json!({ "identifier": "slow-reading" })))); }),
        Box::new(move |w| { w.person(insert(cite, Parent::Node(para), Some(label_ref), content("cite", json!({ "label": "knuth1984" })))); }),
        Box::new(move |w| { w.person(OpBody::SetRelation { node: id_link, relation: Some(Relation::new(Relation::SUPPORTS)) }); }),
        Box::new(move |w| { w.person(OpBody::EditText { node: para_text, edit: TextEdit::Replace { value: "Marginalia record a reader arguing".into() } }); }),
        Box::new(move |w| { w.person(OpBody::SetAttrs { node: heading, set: json!({ "depth": 2 }).as_object().unwrap().clone(), unset: vec![] }); }),
        // Retype the heading and back: the same node throughout, so links to it hold.
        Box::new(move |w| { w.person(OpBody::SetType { node: heading, node_type: "paragraph".into(), set: Default::default(), unset: vec!["depth".into()] }); }),
        Box::new(move |w| { w.person(OpBody::SetType { node: heading, node_type: "heading".into(), set: json!({ "depth": 2 }).as_object().unwrap().clone(), unset: vec![] }); }),
        Box::new(move |w| { w.person(insert(scrap, root(note), None, content("paragraph", json!({})))); }),
        Box::new(move |w| { w.person(OpBody::DeleteNode { node: scrap }); }),
        Box::new(move |w| { w.person(OpBody::SetDocumentTitle { doc: note, title: "On reading".into() }); }),
        // Agent proposal, accepted.
        Box::new(move |w| { w.agent(accepted, OpBody::OpenChangeSet { change_set: accepted, rationale: "Add a claim".into() }); }),
        Box::new(move |w| { w.agent(accepted, insert(proposed, root(draft), None, content("paragraph", json!({})))); }),
        Box::new(move |w| { w.agent(accepted, insert(proposed_link, Parent::Node(proposed), None, content("link", json!({ "url": "#slow-reading" })))); }),
        Box::new(move |w| { w.agent(accepted, OpBody::SetRelation { node: proposed_link, relation: Some(Relation::new(Relation::CONTRADICTS)) }); }),
        Box::new(move |w| { w.person(OpBody::AcceptChangeSet { change_set: accepted }); }),
        // Agent proposal, rejected.
        Box::new(move |w| { w.agent(rejected, OpBody::OpenChangeSet { change_set: rejected, rationale: "Remove the heading".into() }); }),
        Box::new(move |w| { w.agent(rejected, OpBody::DeleteNode { node: heading }); }),
        Box::new(move |w| { w.person(OpBody::RejectChangeSet { change_set: rejected, reason: Some("Keep it".into()) }); }),
        // Agent proposal, left open.
        Box::new(move |w| { w.agent(pending, OpBody::OpenChangeSet { change_set: pending, rationale: "Pending".into() }); }),
        Box::new(move |w| { w.agent(pending, OpBody::EditText { node: para_text, edit: TextEdit::Replace { value: "proposed wording".into() } }); }),
        // Move the paragraph (with its links) into the draft, then drop the scratch document.
        Box::new(move |w| { w.person(OpBody::MoveNode { node: para, parent: root(draft), after: Some(proposed) }); }),
        Box::new(move |w| { w.person(insert(NodeId::new(), root(gone), None, content("paragraph", json!({})))); }),
        Box::new(move |w| { w.person(OpBody::DeleteDocument { doc: gone }); }),
    ];
    for step in steps {
        step(w);
        check(w);
    }
    Ids { note, draft, heading, para, para_text, id_link, label_ref, cite, proposed_link }
}

#[test]
fn incremental_projections_equal_a_rebuild_after_every_op() {
    let mut w = World::in_memory();
    history(&mut w, |w| {
        assert!(w.store.projections_current().unwrap());
        assert_eq!(w.store.projection_snapshot().unwrap(), w.rebuilt_snapshot(), "at {}", w.seq.head());
    });
}

#[test]
fn queries_agree_with_state() {
    let mut w = World::in_memory();
    let ids = history(&mut w, |_| {});
    let state = w.seq.state();

    for doc in [ids.note, ids.draft] {
        assert_eq!(w.store.children(Parent::DocumentRoot(doc)).unwrap(), state.children(Parent::DocumentRoot(doc)));
    }
    assert_eq!(
        w.store.children(Parent::Node(ids.para)).unwrap(),
        [ids.para_text, ids.id_link, ids.label_ref, ids.cite]
    );

    // Search sees the latest accepted text, not the pending proposal or deleted documents.
    assert_eq!(w.store.search("arguing").unwrap(), [ids.para_text]);
    assert!(w.store.search("thinking").unwrap().is_empty());
    assert!(w.store.search("proposed").unwrap().is_empty());

    // Backlinks to the heading: by ID (typed), by label, and via `#label` (typed by the agent).
    let mut expected = vec![
        (ids.id_link, Some(Relation::new("supports"))),
        (ids.label_ref, None),
        (ids.proposed_link, Some(Relation::new("contradicts"))),
    ];
    expected.sort_by_key(|(id, _)| *id);
    assert_eq!(w.store.backlinks(ids.heading).unwrap(), expected);

    // The pending change set is a row, but none of its edits are.
    let snapshot = w.store.projection_snapshot().unwrap().join("\n");
    assert!(snapshot.contains("'Pending'"));
    assert!(snapshot.contains(" open "));
    assert!(!snapshot.contains("proposed wording"));
}

#[test]
fn document_backlinks_cover_the_document_and_its_blocks() {
    let mut w = World::in_memory();
    let ids = history(&mut w, |_| {});
    // A link to the whole note, from inside the note's own heading.
    let doc_link = NodeId::new();
    w.person(insert(doc_link, Parent::Node(ids.heading), None, content("link", json!({ "url": ids.note.to_string() }))));

    let links = w.store.document_backlinks(ids.note).unwrap();
    let mut got: Vec<_> = links.iter().map(|b| (b.source, b.source_doc, b.target, b.relation.clone())).collect();
    got.sort_by_key(|(source, ..)| *source);
    let mut expected = vec![
        (ids.id_link, ids.draft, Some(ids.heading), Some(Relation::new("supports"))),
        (ids.label_ref, ids.draft, Some(ids.heading), None),
        (ids.proposed_link, ids.draft, Some(ids.heading), Some(Relation::new("contradicts"))),
        (doc_link, ids.note, None, None),
    ];
    expected.sort_by_key(|(source, ..)| *source);
    assert_eq!(got, expected);
    // Nothing points into the draft.
    assert!(w.store.document_backlinks(ids.draft).unwrap().is_empty());

    // Moving the heading to the draft moves its backlinks with it.
    w.person(OpBody::MoveNode { node: ids.heading, parent: Parent::DocumentRoot(ids.draft), after: None });
    assert_eq!(w.store.document_backlinks(ids.draft).unwrap().len(), 3);
    // The link to the note moved with the heading, so it now comes from the draft.
    let to_note = w.store.document_backlinks(ids.note).unwrap();
    assert_eq!(to_note.iter().map(|b| (b.source, b.source_doc)).collect::<Vec<_>>(), [(doc_link, ids.draft)]);
    // Deleting the target drops them.
    w.person(OpBody::DeleteNode { node: ids.heading });
    assert!(w.store.document_backlinks(ids.draft).unwrap().is_empty());
}

#[test]
fn moved_subtree_changes_document_in_projection() {
    let mut w = World::in_memory();
    let ids = history(&mut w, |_| {});
    let line = w
        .store
        .projection_snapshot()
        .unwrap()
        .into_iter()
        .find(|l| l.starts_with("edge ") && l.contains("supports"))
        .unwrap();
    // The typed link moved with its paragraph into the draft.
    assert!(line.contains(&hex(ids.draft.as_bytes())), "{line}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[test]
fn load_rebuilds_projections_that_are_behind_or_from_another_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.lvn");
    let (project, person, expected) = {
        let mut w = World::on_disk(&path);
        history(&mut w, |_| {});
        (w.seq.project(), w.person.clone(), w.store.projection_snapshot().unwrap())
    };

    // Built by another version: rebuilt on load.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("UPDATE projection_meta SET producer_version = '0'", []).unwrap();
        conn.execute("DELETE FROM search", []).unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert!(!store.projections_current().unwrap());
    let seq = Sequencer::load(&mut store).unwrap();
    assert!(store.projections_current().unwrap());
    assert_eq!(store.projection_snapshot().unwrap(), expected);

    // Behind the log (a log-only append): the next commit rebuilds.
    let doc = DocumentId::new();
    let op = |base, body| Op {
        id: OpId::new(),
        project,
        principal: person.holder,
        grant: person.id,
        base,
        change_set: None,
        created_at: Timestamp::now(),
        body,
    };
    let behind = Sequenced {
        seq: seq.head().next(),
        op: op(seq.head(), OpBody::CreateDocument { doc, kind: DocKind::note(), title: "Log only".into() }),
    };
    store.append(std::slice::from_ref(&behind)).unwrap();
    assert!(!store.projections_current().unwrap());
    let mut seq2 = Sequencer::load(&mut store).unwrap();
    assert!(store.projections_current().unwrap());
    assert!(store.projection_snapshot().unwrap().iter().any(|l| l.contains("'Log only'")));

    seq2.submit(&mut store, vec![op(seq2.head(), OpBody::SetDocumentTitle { doc, title: "Renamed".into() })])
        .unwrap();
    assert!(store.projections_current().unwrap());
}
