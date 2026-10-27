//! MyST AST -> ops -> project state -> MyST AST must give back the document.

use lv_core::convert::{Flavor, ImportError, import_document, import_nodes, materialize};
use lv_core::ids::{DocumentId, NodeId, OpId, PersonId, ProjectId};
use lv_core::myst::{Node, TEXT_RUN_TYPES};
use lv_core::op::{DocKind, NodeContent, Op, OpBody, Parent, Relation, TextEdit, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::refusal::RefusalCode;
use lv_core::sequencer::{MemoryLog, Sequencer, SubmitError};
use lv_core::store::Store;
use serde_json::{Map, Value, json};

const FIXTURES: [&str; 4] = ["prose", "directives", "references", "unknown"];

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/myst/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn parse(value: Value) -> Node {
    serde_json::from_value(value).unwrap()
}

struct World {
    seq: Sequencer,
    log: MemoryLog,
    person: Grant,
}

impl World {
    fn new() -> World {
        let person = Grant::new(Principal::Person { person: PersonId::new() }, GrantScope::Owner);
        let mut log = MemoryLog::default();
        log.add_grant(person.clone());
        World { seq: Sequencer::new(ProjectId::new()), log, person }
    }

    fn ops(&self, bodies: Vec<OpBody>) -> Vec<Op> {
        bodies
            .into_iter()
            .map(|body| Op {
                id: OpId::new(),
                project: self.seq.project(),
                principal: self.person.holder,
                grant: self.person.id,
                base: self.seq.head(),
                change_set: None,
                created_at: Timestamp::now(),
                body,
            })
            .collect()
    }

    fn submit(&mut self, bodies: Vec<OpBody>) -> Result<(), RefusalCode> {
        let batch = self.ops(bodies);
        self.seq.submit(&mut self.log, batch).map(drop).map_err(|e| match e {
            SubmitError::Refused(r) => r.code,
            SubmitError::Log(never) => match never {},
        })
    }

    fn import(&mut self, root: &Node) -> DocumentId {
        let doc = DocumentId::new();
        self.submit(import_document(doc, DocKind::Note, "Imported", root).unwrap()).unwrap();
        doc
    }

    fn export(&self, doc: DocumentId, flavor: Flavor) -> Value {
        serde_json::to_value(materialize(self.seq.state(), doc, flavor).unwrap()).unwrap()
    }
}

/// Every object in `value` that has a `type`, depth first.
fn nodes(value: &Value) -> Vec<&Map<String, Value>> {
    let mut out = Vec::new();
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        if let Value::Object(map) = v {
            out.push(map);
            if let Some(Value::Array(children)) = map.get("children") {
                stack.extend(children.iter().rev());
            }
        }
    }
    out
}

fn strip_lv_fields(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("id");
        map.remove("relation");
        if let Some(Value::Array(children)) = map.get_mut("children") {
            children.iter_mut().for_each(strip_lv_fields);
        }
    }
}

#[test]
fn fixtures_roundtrip_through_the_project() {
    for name in FIXTURES {
        let original = fixture(name);
        let mut w = World::new();
        let doc = w.import(&parse(original.clone()));
        assert_eq!(w.export(doc, Flavor::Plain), original, "{name} did not round-trip");
    }
}

#[test]
fn import_is_one_insert_per_node_in_document_order() {
    let root = parse(fixture("prose"));
    let doc = DocumentId::new();
    let bodies = import_document(doc, DocKind::Draft, "Prose", &root).unwrap();
    assert!(matches!(&bodies[0], OpBody::CreateDocument { doc: d, kind: DocKind::Draft, .. } if *d == doc));
    let types: Vec<&str> = bodies[1..]
        .iter()
        .map(|b| match b {
            OpBody::InsertNode { content, .. } => content.node_type.as_str(),
            other => panic!("unexpected {}", other.kind()),
        })
        .collect();
    let expected: Vec<&str> = root.walk().map(Node::type_name).collect();
    assert_eq!(types, expected);
}

#[test]
fn native_export_carries_ids_and_reimports_with_them() {
    for name in FIXTURES {
        let original = fixture(name);
        let mut w = World::new();
        let doc = w.import(&parse(original.clone()));
        let native = w.export(doc, Flavor::Native);

        for node in nodes(&native) {
            let node_type = node["type"].as_str().unwrap();
            let id = node.get("id").and_then(Value::as_str);
            if TEXT_RUN_TYPES.contains(&node_type) {
                assert_eq!(id, None, "{name}: a `{node_type}` text run got an ID");
            } else {
                let id: NodeId = id.unwrap_or_else(|| panic!("{name}: `{node_type}` has no ID")).parse().unwrap();
                assert!(w.seq.state().node(id).is_some());
            }
        }

        let mut plain = native.clone();
        strip_lv_fields(&mut plain);
        assert_eq!(plain, original, "{name}: native export is plain export plus lv fields");

        // Into another project the IDs come along; text runs get new ones.
        let mut other = World::new();
        let copy = other.import(&parse(native.clone()));
        assert_eq!(other.export(copy, Flavor::Native), native, "{name}: native export did not re-import");
    }
}

#[test]
fn relations_export_natively_and_reimport() {
    let mut w = World::new();
    let doc = w.import(&parse(fixture("references")));
    let link = w
        .seq
        .state()
        .nodes()
        .find(|(_, n)| n.content.node_type == "crossReference")
        .map(|(id, _)| id)
        .expect("a cross-reference");
    w.submit(vec![OpBody::SetRelation { node: link, relation: Some(Relation::new(Relation::SUPPORTS)) }]).unwrap();

    let native = w.export(doc, Flavor::Native);
    let typed: Vec<_> = nodes(&native).into_iter().filter(|n| n.contains_key("relation")).collect();
    assert_eq!(typed.len(), 1);
    assert_eq!((&typed[0]["id"], &typed[0]["relation"]), (&json!(link.to_string()), &json!("supports")));
    assert_eq!(w.export(doc, Flavor::Plain), fixture("references"), "plain export has no relations");

    let mut other = World::new();
    other.import(&parse(native));
    let relation = other.seq.state().node(link).unwrap().relation.clone();
    assert_eq!(relation, Some(Relation::new(Relation::SUPPORTS)));
}

#[test]
fn edits_show_in_the_materialized_document() {
    let mut w = World::new();
    let doc = w.import(&parse(json!({
        "type": "root",
        "children": [
            { "type": "paragraph", "children": [{ "type": "text", "value": "first" }] },
            { "type": "paragraph", "children": [{ "type": "text", "value": "second" }] }
        ]
    })));
    let state = w.seq.state();
    let root = state.children(Parent::DocumentRoot(doc))[0];
    let [first, second] = state.children(Parent::Node(root))[..] else { panic!("two paragraphs") };
    let text = state.children(Parent::Node(first))[0];

    let fragment = parse(json!({ "type": "heading", "depth": 2, "children": [{ "type": "text", "value": "Intro" }] }));
    let mut bodies = vec![
        OpBody::EditText { node: text, edit: TextEdit::Replace { value: "first, edited".into() } },
        OpBody::DeleteNode { node: second },
    ];
    bodies.extend(import_nodes(Parent::Node(root), None, &[fragment]).unwrap());
    w.submit(bodies).unwrap();

    assert_eq!(
        w.export(doc, Flavor::Plain),
        json!({
            "type": "root",
            "children": [
                { "type": "heading", "depth": 2, "children": [{ "type": "text", "value": "Intro" }] },
                { "type": "paragraph", "children": [{ "type": "text", "value": "first, edited" }] }
            ]
        })
    );
}

#[test]
fn documents_built_without_a_root_get_one() {
    let mut w = World::new();
    let doc = DocumentId::new();
    let para = NodeId::new();
    let content = NodeContent { node_type: "paragraph".into(), fields: Map::new() };
    w.submit(vec![
        OpBody::CreateDocument { doc, kind: DocKind::Note, title: "Bare".into() },
        OpBody::InsertNode { node: para, parent: Parent::DocumentRoot(doc), after: None, content },
    ])
    .unwrap();
    assert_eq!(
        w.export(doc, Flavor::Native),
        json!({ "type": "root", "children": [{ "type": "paragraph", "id": para.to_string(), "children": [] }] })
    );
}

#[test]
fn import_refuses_what_it_cannot_represent() {
    let doc = DocumentId::new();
    let para = parse(json!({ "type": "paragraph", "children": [] }));
    assert_eq!(import_document(doc, DocKind::Note, "x", &para), Err(ImportError::NotARoot("paragraph".into())));

    let id = NodeId::new();
    let twice = parse(json!({
        "type": "root",
        "children": [{ "type": "paragraph", "id": id }, { "type": "paragraph", "id": id }]
    }));
    assert_eq!(import_document(doc, DocKind::Note, "x", &twice), Err(ImportError::DuplicateId(id)));

    let foreign = parse(json!({ "type": "root", "children": [{ "type": "widget", "id": "w-17" }] }));
    assert_eq!(import_document(doc, DocKind::Note, "x", &foreign), Err(ImportError::BadId("w-17".into())));

    let junk = parse(json!({ "type": "root", "children": [{ "type": "widget", "children": [3] }] }));
    assert_eq!(import_document(doc, DocKind::Note, "x", &junk), Err(ImportError::BadChildren("widget".into())));
}

#[test]
fn ops_must_keep_documents_valid_myst() {
    let mut w = World::new();
    let doc = w.import(&parse(fixture("prose")));
    let state = w.seq.state();
    let (heading, text) = {
        let find = |t: &str| state.nodes().find(|(_, n)| n.content.node_type == t).map(|(id, _)| id).unwrap();
        (find("heading"), find("text"))
    };

    let bad_depth = Map::from_iter([("depth".to_owned(), json!("deep"))]);
    let bad_heading = NodeContent { node_type: "heading".into(), fields: bad_depth.clone() };
    let insert = |parent, content| OpBody::InsertNode { node: NodeId::new(), parent, after: None, content };
    assert_eq!(w.submit(vec![insert(Parent::DocumentRoot(doc), bad_heading)]), Err(RefusalCode::InvalidOp));
    assert_eq!(
        w.submit(vec![OpBody::SetAttrs { node: heading, set: bad_depth, unset: vec![] }]),
        Err(RefusalCode::InvalidOp)
    );
    let paragraph = NodeContent { node_type: "paragraph".into(), fields: Map::new() };
    assert_eq!(w.submit(vec![insert(Parent::Node(text), paragraph)]), Err(RefusalCode::InvalidOp), "text is a leaf");

    assert_eq!(w.export(doc, Flavor::Plain), fixture("prose"));
}

#[test]
fn materializes_the_same_after_reloading_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.lv");
    let mut store = Store::create(&path, ProjectId::new(), "Reload").unwrap();
    let person = PersonId::new();
    store.add_person(person, "Ada").unwrap();
    let grant = Grant::new(Principal::Person { person }, GrantScope::Owner);
    store.add_grant(&grant).unwrap();

    let mut seq = Sequencer::new(store.project());
    let doc = DocumentId::new();
    let batch = import_document(doc, DocKind::Note, "Directives", &parse(fixture("directives")))
        .unwrap()
        .into_iter()
        .map(|body| Op {
            id: OpId::new(),
            project: seq.project(),
            principal: grant.holder,
            grant: grant.id,
            base: seq.head(),
            change_set: None,
            created_at: Timestamp::now(),
            body,
        })
        .collect();
    seq.submit(&mut store, batch).unwrap();
    let before = materialize(seq.state(), doc, Flavor::Native).unwrap();
    drop(store);

    let mut store = Store::open(&path).unwrap();
    let reloaded = Sequencer::load(&mut store).unwrap();
    assert_eq!(materialize(reloaded.state(), doc, Flavor::Native).unwrap(), before);
    assert_eq!(serde_json::to_value(materialize(reloaded.state(), doc, Flavor::Plain).unwrap()).unwrap(), fixture("directives"));
}
