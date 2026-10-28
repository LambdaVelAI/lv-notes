//! MyST AST -> ops -> project state -> MyST AST must give back the document.

use lv_core::convert::{Flavor, ImportError, Update, import_document, import_nodes, materialize, update_document};
use lv_core::ids::{DocumentId, NodeId, OpId, PersonId, ProjectId};
use lv_core::myst::{ChildrenPolicy, Node, TEXT_RUN_TYPES, children_policy};
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
        self.submit(import_document(doc, DocKind::note(), "Imported", root).unwrap()).unwrap();
        doc
    }

    fn export(&self, doc: DocumentId, flavor: Flavor) -> Value {
        serde_json::to_value(materialize(self.seq.state(), doc, flavor).unwrap()).unwrap()
    }

    /// Saves `edited` over `doc` and checks the document now exports as it.
    fn save(&mut self, doc: DocumentId, edited: &Value) -> Update {
        let update = update_document(self.seq.state(), doc, &parse(edited.clone())).unwrap();
        if !update.ops.is_empty() {
            self.submit(update.ops.clone()).unwrap_or_else(|code| panic!("{code:?} for {:#?}", update.ops));
        }
        let mut expected = edited.clone();
        rename(&mut expected, &update.renamed);
        drop_optional_empty_children(&mut expected);
        assert_eq!(self.export(doc, Flavor::Native), expected, "save did not give the edited tree");
        update
    }
}

fn rename(value: &mut Value, renamed: &[(NodeId, NodeId)]) {
    let Value::Object(map) = value else { return };
    if let Some(Value::String(id)) = map.get_mut("id")
        && let Some((_, new)) = renamed.iter().find(|(old, _)| old.to_string() == *id)
    {
        *id = new.to_string();
    }
    if let Some(Value::Array(children)) = map.get_mut("children") {
        children.iter_mut().for_each(|c| rename(c, renamed));
    }
}

/// State does not record an empty `children` on types where it is optional
/// (a known gap), so an emptied directive exports without it.
fn drop_optional_empty_children(value: &mut Value) {
    let Value::Object(map) = value else { return };
    let optional = map.get("type").and_then(Value::as_str).is_some_and(|t| children_policy(t) != ChildrenPolicy::Always);
    if optional && map.get("children").is_some_and(|c| c.as_array().is_some_and(Vec::is_empty)) {
        map.remove("children");
    }
    if let Some(Value::Array(children)) = map.get_mut("children") {
        children.iter_mut().for_each(drop_optional_empty_children);
    }
}

fn kinds(update: &Update) -> Vec<&'static str> {
    update.ops.iter().map(OpBody::kind).collect()
}

/// The child at `path` (indices into `children`) of `root`.
fn at<'a>(root: &'a mut Value, path: &[usize]) -> &'a mut Value {
    path.iter().fold(root, |v, &i| &mut v["children"][i])
}

fn children(value: &mut Value) -> &mut Vec<Value> {
    value["children"].as_array_mut().unwrap()
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
    let bodies = import_document(doc, DocKind::draft(), "Prose", &root).unwrap();
    assert!(matches!(&bodies[0], OpBody::CreateDocument { doc: d, kind, .. } if *d == doc && *kind == DocKind::draft()));
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
        OpBody::CreateDocument { doc, kind: DocKind::note(), title: "Bare".into() },
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
    assert_eq!(import_document(doc, DocKind::note(), "x", &para), Err(ImportError::NotARoot("paragraph".into())));

    let id = NodeId::new();
    let twice = parse(json!({
        "type": "root",
        "children": [{ "type": "paragraph", "id": id }, { "type": "paragraph", "id": id }]
    }));
    assert_eq!(import_document(doc, DocKind::note(), "x", &twice), Err(ImportError::DuplicateId(id)));

    let foreign = parse(json!({ "type": "root", "children": [{ "type": "widget", "id": "w-17" }] }));
    assert_eq!(import_document(doc, DocKind::note(), "x", &foreign), Err(ImportError::BadId("w-17".into())));

    let junk = parse(json!({ "type": "root", "children": [{ "type": "widget", "children": [3] }] }));
    assert_eq!(import_document(doc, DocKind::note(), "x", &junk), Err(ImportError::BadChildren("widget".into())));
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
    let batch = import_document(doc, DocKind::note(), "Directives", &parse(fixture("directives")))
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

// ---- Saving edited documents ----------------------------------------------

/// A project with the `prose` fixture imported, and its native export.
fn edited_prose() -> (World, DocumentId, Value) {
    let mut w = World::new();
    let doc = w.import(&parse(fixture("prose")));
    let native = w.export(doc, Flavor::Native);
    (w, doc, native)
}

#[test]
fn saving_an_unchanged_document_is_no_ops() {
    let (mut w, doc, native) = edited_prose();
    assert_eq!(w.save(doc, &native), Update::default());
}

#[test]
fn typing_is_one_text_edit() {
    let (mut w, doc, mut native) = edited_prose();
    // root > block > heading > text
    at(&mut native, &[0, 0, 0])["value"] = json!("On reading slowly, again");
    assert_eq!(kinds(&w.save(doc, &native)), ["edit_text"]);
}

#[test]
fn reorder_insert_delete_and_attrs() {
    let (mut w, doc, mut native) = edited_prose();
    let block = at(&mut native, &[0]);
    let blocks = children(block);
    blocks.swap(0, 1);
    blocks.remove(2);
    let new = NodeId::new();
    blocks.insert(1, json!({ "type": "paragraph", "id": new, "children": [{ "type": "text", "value": "New" }] }));
    let heading = blocks.iter_mut().find(|b| b["type"] == "heading").unwrap();
    heading["depth"] = json!(3);
    heading.as_object_mut().unwrap().remove("position");

    let update = w.save(doc, &native);
    let mut kinds = kinds(&update);
    kinds.sort();
    assert_eq!(kinds, ["delete_node", "insert_node", "insert_node", "move_node", "set_attrs"]);
}

#[test]
fn wrapping_and_unwrapping_swap_ancestors() {
    let (mut w, doc, mut native) = edited_prose();
    let quote = NodeId::new();
    let blocks = children(at(&mut native, &[0]));
    let first = blocks.remove(0);
    blocks.insert(0, json!({ "type": "blockquote", "id": quote, "children": [first] }));
    w.save(doc, &native);

    // Now the heading takes the quote's place and holds it.
    let blocks = children(at(&mut native, &[0]));
    let mut quote = blocks.remove(0);
    let mut heading = children(&mut quote).remove(0);
    children(&mut heading).push(quote);
    blocks.insert(0, heading);
    assert!(kinds(&w.save(doc, &native)).iter().all(|k| *k == "move_node"));
}

#[test]
fn changing_a_type_keeps_the_id() {
    let (mut w, doc, mut native) = edited_prose();
    let blocks = children(at(&mut native, &[0]));
    let paragraph = blocks.iter_mut().find(|b| b["type"] == "paragraph").unwrap();
    let id: NodeId = paragraph["id"].as_str().unwrap().parse().unwrap();
    let text = paragraph["children"].clone();

    // Paragraph → heading: one op, same node.
    paragraph["type"] = json!("heading");
    paragraph["depth"] = json!(2);
    let update = w.save(doc, &native);
    assert_eq!((kinds(&update), update.renamed.len()), (vec!["set_type"], 0));
    assert_eq!(w.seq.state().node(id).unwrap().content.node_type, "heading");

    // Heading → code: its text runs go, and it takes a value.
    let block = children(at(&mut native, &[0])).iter_mut().find(|b| b["id"] == json!(id.to_string())).unwrap();
    *block = json!({ "type": "code", "id": id, "value": "print('hi')" });
    let update = w.save(doc, &native);
    let k = kinds(&update);
    assert_eq!(k.last(), Some(&"set_type"));
    assert!(k[..k.len() - 1].iter().all(|k| *k == "delete_node"), "{k:?}");

    // Code → paragraph: the value goes, and text runs come back.
    let block = children(at(&mut native, &[0])).iter_mut().find(|b| b["id"] == json!(id.to_string())).unwrap();
    *block = json!({ "type": "paragraph", "id": id, "children": text });
    let update = w.save(doc, &native);
    assert_eq!(kinds(&update)[0], "set_type");
    // Only the deleted link inside it comes back under a new ID; the block keeps its own.
    assert!(update.renamed.iter().all(|(old, _)| *old != id));
}

#[test]
fn reusing_a_deleted_id_renames() {
    let (mut w, doc, mut native) = edited_prose();
    let blocks = children(at(&mut native, &[0]));
    let i = blocks.iter().position(|b| b["type"] == "paragraph").unwrap();
    let paragraph = blocks.remove(i);
    let old: NodeId = paragraph["id"].as_str().unwrap().parse().unwrap();
    w.save(doc, &native);
    assert!(w.seq.state().node(old).unwrap().deleted);

    // Undo in the editor brings the old ID back: it is deleted, so renamed.
    children(at(&mut native, &[0])).insert(i, paragraph);
    let update = w.save(doc, &native);
    assert_eq!(update.renamed.first().map(|r| r.0), Some(old), "and the link inside it");
}

#[test]
fn set_type_refusals() {
    let (mut w, _doc, native) = edited_prose();
    let state = w.seq.state();
    let find = |t: &str| state.nodes().find(|(_, n)| n.content.node_type == t && !n.deleted).map(|(id, _)| id).unwrap();
    let (paragraph, text) = (find("paragraph"), find("text"));
    let retype = |node, t: &str| OpBody::SetType { node, node_type: t.into(), set: Default::default(), unset: vec![] };

    // Text runs have no identity to keep.
    assert_eq!(w.submit(vec![retype(text, "emphasis")]), Err(RefusalCode::InvalidOp));
    // A leaf can't keep children, and the result must be valid MyST.
    assert_eq!(w.submit(vec![retype(paragraph, "thematicBreak")]), Err(RefusalCode::InvalidOp));
    let bad_depth = OpBody::SetType {
        node: paragraph,
        node_type: "heading".into(),
        set: json!({ "depth": "two" }).as_object().unwrap().clone(),
        unset: vec![],
    };
    assert_eq!(w.submit(vec![bad_depth]), Err(RefusalCode::InvalidOp));
    let reserved = OpBody::SetType {
        node: paragraph,
        node_type: "heading".into(),
        set: json!({ "id": "x" }).as_object().unwrap().clone(),
        unset: vec![],
    };
    assert_eq!(w.submit(vec![reserved]), Err(RefusalCode::InvalidOp));
    assert_eq!(w.submit(vec![retype(paragraph, "heading")]), Ok(()));
    drop(native);
}

#[test]
fn nodes_move_between_documents() {
    let (mut w, from, mut native) = edited_prose();
    let to = w.import(&parse(json!({ "type": "root", "children": [] })));
    let mut target = w.export(to, Flavor::Native);
    let moved = children(at(&mut native, &[0])).remove(1);
    let moved_id: NodeId = moved["id"].as_str().unwrap().parse().unwrap();
    children(&mut target).push(moved);

    assert_eq!(kinds(&w.save(to, &target)), ["move_node"]);
    assert_eq!(w.seq.state().node(moved_id).unwrap().document, to);
    assert_eq!(w.save(from, &native), Update::default(), "already gone from the source");
}

#[test]
fn inline_edits_reuse_text_runs() {
    let (mut w, doc, mut native) = edited_prose();
    let heading = at(&mut native, &[0, 0]);
    *children(heading) = vec![
        json!({ "type": "text", "value": "On " }),
        json!({ "type": "strong", "children": [{ "type": "text", "value": "reading" }] }),
        json!({ "type": "text", "value": " slowly" }),
    ];
    let mut kinds = kinds(&w.save(doc, &native));
    kinds.sort();
    // The old text run becomes "On "; the rest is new.
    assert_eq!(kinds, ["edit_text", "insert_node", "insert_node", "insert_node"]);
}

#[test]
fn saving_refuses_trees_it_cannot_match() {
    let (w, doc, mut native) = edited_prose();
    let state = w.seq.state();
    let mut anonymous = native.clone();
    at(&mut anonymous, &[0, 0]).as_object_mut().unwrap().remove("id");
    assert_eq!(update_document(state, doc, &parse(anonymous)), Err(ImportError::MissingId("heading".into())));

    let first = at(&mut native, &[0, 0]).clone();
    let id: NodeId = first["id"].as_str().unwrap().parse().unwrap();
    children(at(&mut native, &[0])).push(first);
    assert_eq!(update_document(state, doc, &parse(native)), Err(ImportError::DuplicateId(id)));

    let missing = DocumentId::new();
    let empty = parse(json!({ "type": "root", "children": [] }));
    assert_eq!(update_document(state, missing, &empty), Err(ImportError::UnknownDocument(missing)));
}

/// A small deterministic generator, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Paths of every node under `value` that has a `children` array.
fn parents(value: &Value, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    if let Some(Value::Array(children)) = value.get("children") {
        out.push(path.clone());
        for (i, c) in children.iter().enumerate() {
            path.push(i);
            parents(c, path, out);
            path.pop();
        }
    }
}

/// Paths of every node under the root (not the root itself).
fn descendants(value: &Value) -> Vec<Vec<usize>> {
    let mut parent_paths = Vec::new();
    parents(value, &mut Vec::new(), &mut parent_paths);
    parent_paths
        .iter()
        .flat_map(|p| {
            let mut v = value;
            for &i in p {
                v = &v["children"][i];
            }
            (0..v["children"].as_array().unwrap().len()).map(move |i| [p.as_slice(), &[i]].concat())
        })
        .collect()
}

#[test]
fn random_edits_always_save_exactly() {
    for name in FIXTURES {
        let mut w = World::new();
        let doc = w.import(&parse(fixture(name)));
        let mut tree = w.export(doc, Flavor::Native);
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ name.len() as u64);
        for step in 0..60 {
            let nodes = descendants(&tree);
            match rng.below(5) {
                // Delete a node.
                0 if !nodes.is_empty() => {
                    let path = &nodes[rng.below(nodes.len())];
                    let (last, parent) = path.split_last().unwrap();
                    children(at(&mut tree, parent)).remove(*last);
                }
                // Move a node anywhere that takes children, outside itself.
                1 if !nodes.is_empty() => {
                    let path = nodes[rng.below(nodes.len())].clone();
                    let (last, parent) = path.split_last().unwrap();
                    let node = children(at(&mut tree, parent)).remove(*last);
                    let mut targets = Vec::new();
                    parents(&tree, &mut Vec::new(), &mut targets);
                    let target = &targets[rng.below(targets.len())];
                    let list = children(at(&mut tree, target));
                    let index = rng.below(list.len() + 1);
                    list.insert(index, node);
                }
                // Edit some text.
                2 => {
                    if let Some(path) = nodes.iter().find(|p| at(&mut tree, p)["type"] == "text") {
                        at(&mut tree, path)["value"] = json!(format!("edit {step}"));
                    }
                }
                // Change an attribute, or turn a paragraph into a heading and back.
                3 if !nodes.is_empty() => {
                    let path = &nodes[rng.below(nodes.len())];
                    let node = at(&mut tree, path);
                    if node["type"] == "paragraph" {
                        node["type"] = json!("heading");
                        node["depth"] = json!(1 + step % 3);
                    } else if node["type"] == "heading" {
                        node["type"] = json!("paragraph");
                        node.as_object_mut().unwrap().remove("depth");
                    } else if node["type"] != "text" {
                        node["data-step"] = json!(step);
                    }
                }
                // Insert a new paragraph.
                _ => {
                    let mut targets = Vec::new();
                    parents(&tree, &mut Vec::new(), &mut targets);
                    let target = &targets[rng.below(targets.len())];
                    let list = children(at(&mut tree, target));
                    let index = rng.below(list.len() + 1);
                    let p = json!({ "type": "paragraph", "id": NodeId::new(), "children": [{ "type": "text", "value": "new" }] });
                    list.insert(index, p);
                }
            }
            let update = w.save(doc, &tree);
            rename(&mut tree, &update.renamed);
        }
    }
}
