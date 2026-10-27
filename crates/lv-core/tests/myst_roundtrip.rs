//! MyST AST JSON must survive parse -> typed AST -> serialize unchanged.

use lv_core::myst::Node;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/myst/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// Parses, re-serializes (through a string, as storage would) and checks
/// the JSON is unchanged.
fn roundtrip(name: &str) -> Node {
    let original = fixture(name);
    let node: Node = serde_json::from_value(original.clone()).unwrap();
    let text = serde_json::to_string(&node).unwrap();
    let back: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(back, original, "{name} did not round-trip");
    let reparsed: Node = serde_json::from_str(&text).unwrap();
    assert_eq!(reparsed, node, "{name} reparsed to a different AST");
    node
}

fn unknown_types(node: &Node) -> Vec<String> {
    node.walk()
        .filter(|n| matches!(n, Node::Unknown(_)))
        .map(|n| n.type_name().to_owned())
        .collect()
}

#[test]
fn prose_roundtrips_fully_typed() {
    let root = roundtrip("prose");
    assert_eq!(unknown_types(&root), Vec::<String>::new());

    let Node::Root(r) = &root else { panic!("not a root") };
    let Node::Block(block) = &r.children[0] else { panic!("not a block") };
    let Node::Heading(h) = &block.children[0] else { panic!("not a heading") };
    assert_eq!(h.depth, 1);
    assert_eq!(h.common.identifier.as_deref(), Some("on-reading-slowly"));
    assert_eq!(h.common.position.as_ref().unwrap().end.column, Some(20));
    let Node::List(list) = &block.children[2] else { panic!("not a list") };
    assert_eq!((list.ordered, list.start), (Some(true), Some(3)));
}

#[test]
fn directives_roundtrip_fully_typed() {
    let root = roundtrip("directives");
    assert_eq!(unknown_types(&root), Vec::<String>::new());

    let figure = root
        .walk()
        .find_map(|n| match n {
            Node::Container(c) if c.kind == "figure" => Some(c),
            _ => None,
        })
        .expect("figure container");
    assert_eq!(figure.common.label.as_deref(), Some("fig-graph"));

    let abbr = root
        .walk()
        .find_map(|n| match n {
            Node::MystRole(r) if r.name == "abbr" => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(abbr.children.is_none(), "absent children must stay absent");
}

#[test]
fn references_roundtrip_fully_typed() {
    let root = roundtrip("references");
    assert_eq!(unknown_types(&root), Vec::<String>::new());

    let keys: Vec<_> = root
        .walk()
        .filter_map(|n| match n {
            Node::Cite(c) => c.common.label.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(keys, ["knuth1984", "knuth1984", "ramsey1994"]);
}

#[test]
fn unknown_types_and_fields_are_preserved() {
    let root = roundtrip("unknown");
    assert_eq!(unknown_types(&root), ["proof", "abbreviation"]);

    let Node::Root(r) = &root else { panic!("not a root") };
    assert!(r.extra.contains_key("frontmatter"));
    let Node::Block(block) = &r.children[0] else { panic!("not a block") };
    assert_eq!(block.extra.get("key"), Some(&Value::from("k8Xq2")));
    let Node::Heading(h) = &block.children[0] else { panic!("not a heading") };
    assert_eq!(h.enumerated, Some(false));
    assert!(h.extra.contains_key("data"));
}

#[test]
fn node_ids_roundtrip() {
    let id = lv_core::ids::NodeId::new();
    let json = serde_json::json!({
        "type": "paragraph",
        "id": id.to_string(),
        "children": [{ "type": "text", "value": "x" }]
    });
    let node: Node = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(node.common().unwrap().id, Some(id));
    assert_eq!(serde_json::to_value(&node).unwrap(), json);

    let wrong_kind = serde_json::json!({ "type": "paragraph", "id": lv_core::ids::DocumentId::new() });
    assert!(serde_json::from_value::<Node>(wrong_kind).is_err());
}

#[test]
fn node_without_type_is_rejected() {
    let err = serde_json::from_str::<Node>(r#"{"children": []}"#).unwrap_err();
    assert!(err.to_string().contains("type"), "{err}");
}
