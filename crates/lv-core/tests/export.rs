//! Markdown export. The fixtures' exports are checked against
//! `tests/fixtures/markdown/*.md`; regenerate them with
//! `WRITE_GOLDEN=1 cargo test -p lv-core --test export` and review the diff.

use lv_core::convert::import_document;
use lv_core::export::{document_markdown, project_markdown, slug};
use lv_core::ids::{DocumentId, NodeId, OpId, PersonId, ProjectId};
use lv_core::myst::Node;
use lv_core::op::{DocKind, Op, OpBody, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::sequencer::{MemoryLog, Sequencer};
use serde_json::{Value, json};

const FIXTURES: [&str; 4] = ["prose", "directives", "references", "unknown"];

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/myst/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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

    fn submit(&mut self, bodies: Vec<OpBody>) {
        let batch = bodies
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
            .collect();
        self.seq.submit(&mut self.log, batch).unwrap();
    }

    fn import(&mut self, title: &str, root: Value) -> DocumentId {
        let doc = DocumentId::new();
        let root: Node = serde_json::from_value(root).unwrap();
        self.submit(import_document(doc, DocKind::Note, title, &root).unwrap());
        doc
    }
}

#[test]
fn fixtures_export_as_expected() {
    for name in FIXTURES {
        let mut w = World::new();
        let doc = w.import(name, fixture(name));
        let file = document_markdown(w.seq.state(), doc).unwrap();
        assert_eq!(file.name, format!("{name}.md"));
        let path = format!("{}/tests/fixtures/markdown/{name}.md", env!("CARGO_MANIFEST_DIR"));
        if std::env::var_os("WRITE_GOLDEN").is_some() {
            std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
            std::fs::write(&path, &file.markdown).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}; run with WRITE_GOLDEN=1"));
        assert_eq!(file.markdown, expected, "{name}");
    }
}

#[test]
fn links_between_notes_become_relative_paths() {
    let mut w = World::new();
    let (labelled, plain) = (NodeId::new(), NodeId::new());
    let target = w.import(
        "Slow reading",
        json!({ "type": "root", "children": [
            { "type": "heading", "id": labelled, "depth": 1, "identifier": "slow", "label": "slow", "children": [{ "type": "text", "value": "Slow" }] },
            { "type": "paragraph", "id": plain, "children": [{ "type": "text", "value": "A claim." }] },
        ]}),
    );
    let source = w.import(
        "Notes: a draft?",
        json!({ "type": "root", "children": [
            { "type": "paragraph", "children": [
                { "type": "link", "url": target.to_string(), "children": [{ "type": "text", "value": "note" }] },
                { "type": "text", "value": " " },
                { "type": "link", "url": labelled.to_string(), "children": [{ "type": "text", "value": "heading" }] },
                { "type": "text", "value": " " },
                { "type": "link", "url": plain.to_string(), "children": [{ "type": "text", "value": "claim" }] },
                { "type": "text", "value": " " },
                { "type": "link", "url": NodeId::new().to_string(), "children": [{ "type": "text", "value": "gone" }] },
            ]},
        ]}),
    );
    // A second note with the same title gets its own file.
    w.import("Slow reading", json!({ "type": "root", "children": [] }));

    let files = project_markdown(w.seq.state());
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["slow-reading.md", "notes-a-draft.md", "slow-reading-2.md"]);

    let anchor = format!("node-{}", plain.to_string().trim_start_matches("node_"));
    let from = &files.iter().find(|f| f.doc == source).unwrap().markdown;
    assert!(from.starts_with("---\ntitle: \"Notes: a draft?\"\n---\n\n"), "{from}");
    assert!(from.contains("[note](slow-reading.md)"), "{from}");
    assert!(from.contains("[heading](slow-reading.md#slow)"), "{from}");
    assert!(from.contains(&format!("[claim](slow-reading.md#{anchor})")), "{from}");
    assert!(from.contains("[gone](node_"), "a dangling link keeps its url: {from}");

    // The plain paragraph gets a target to link to; the labelled heading doesn't need one.
    let to = &files.iter().find(|f| f.doc == target).unwrap().markdown;
    assert_eq!(to, &format!("---\ntitle: Slow reading\n---\n\n# Slow\n\n({anchor})=\nA claim.\n"));
}

#[test]
fn escapes_text_that_would_read_as_markup() {
    let mut w = World::new();
    let doc = w.import(
        "x",
        json!({ "type": "root", "children": [
            { "type": "paragraph", "children": [{ "type": "text", "value": "# not a heading, *not* [a link], a < b, <b>, $5" }] },
            { "type": "paragraph", "children": [{ "type": "text", "value": "1. not a list" }] },
            { "type": "paragraph", "children": [{ "type": "text", "value": "(see below) and 2020 was a year" }] },
            { "type": "paragraph", "children": [{ "type": "inlineCode", "value": "a`b" }] },
        ]}),
    );
    let md = document_markdown(w.seq.state(), doc).unwrap().markdown;
    let body = md.split("---\n\n").nth(1).unwrap();
    assert_eq!(
        body,
        "\\# not a heading, \\*not\\* \\[a link\\], a < b, \\<b>, \\$5\n\n\\1. not a list\n\n(see below) and 2020 was a year\n\n``a`b``\n"
    );
}

#[test]
fn slugs() {
    assert_eq!(slug("  On Reading, Slowly!  "), "on-reading-slowly");
    assert_eq!(slug("Gödel’s proof"), "gödel-s-proof");
    assert_eq!(slug("???"), "untitled");
    assert_eq!(slug(""), "untitled");
}
