//! The editor's contract with the core: every tree the ProseMirror editor
//! sends during real edits (recorded in `../test/traces/` by
//! `WRITE_TRACES=1 npm test`) saves, and the document then loads back as
//! exactly that tree.

use std::collections::HashMap;

use lv_core::myst::{ChildrenPolicy, Node, children_policy};
use lv_core::op::DocKind;
use lv_desktop::{Identity, Session};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Step {
    edit: String,
    myst: Value,
}

/// Applies the renames the editor would have applied after earlier saves.
fn rename(value: &mut Value, map: &HashMap<String, String>) {
    let Value::Object(obj) = value else { return };
    if let Some(Value::String(id)) = obj.get_mut("id")
        && let Some(new) = map.get(id.as_str())
    {
        *id = new.clone();
    }
    if let Some(Value::Array(children)) = obj.get_mut("children") {
        children.iter_mut().for_each(|c| rename(c, map));
    }
}

/// The core does not record an empty `children` where MyST makes it optional.
fn normalize(value: &mut Value) {
    let Value::Object(obj) = value else { return };
    let optional = obj.get("type").and_then(Value::as_str).is_some_and(|t| children_policy(t) != ChildrenPolicy::Always);
    if optional && obj.get("children").is_some_and(|c| c.as_array().is_some_and(Vec::is_empty)) {
        obj.remove("children");
    }
    if let Some(Value::Array(children)) = obj.get_mut("children") {
        children.iter_mut().for_each(normalize);
    }
}

#[test]
fn editor_traces_save_exactly() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test/traces");
    let mut traces: Vec<_> = std::fs::read_dir(dir).expect("traces; run WRITE_TRACES=1 npm test").map(|e| e.unwrap().path()).collect();
    traces.sort();
    assert!(!traces.is_empty());

    for path in traces {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let steps: Vec<Step> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Session::create(&tmp.path().join("p.lv"), "Traces", &Identity::new("Ada")).unwrap();

        let root: Node = serde_json::from_value(steps[0].myst.clone()).unwrap();
        let (doc, _) = s.import_document(&name, DocKind::note(), &root).unwrap();
        let mut loaded = s.load_document(doc).unwrap();
        let mut expected = steps[0].myst.clone();
        normalize(&mut expected);
        assert_eq!(loaded.myst, expected, "{name}: load");

        let mut renamed = HashMap::new();
        for step in &steps[1..] {
            let mut tree = step.myst.clone();
            rename(&mut tree, &renamed);
            let (saved, _) = s
                .save_document(doc, loaded.version, tree.clone())
                .unwrap_or_else(|e| panic!("{name}: {} was refused: {e}", step.edit));
            assert!(saved.ops > 0, "{name}: {} produced no ops", step.edit);
            let now: HashMap<_, _> = saved.renamed.iter().map(|(o, n)| (o.to_string(), n.to_string())).collect();
            rename(&mut tree, &now);
            renamed.extend(now);
            normalize(&mut tree);

            loaded = s.load_document(doc).unwrap();
            assert_eq!(loaded.myst, tree, "{name}: after {}", step.edit);
        }
    }
}
