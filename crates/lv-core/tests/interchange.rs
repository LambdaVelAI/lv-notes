//! Jupyter Book 2 and Obsidian, both ways. Exporting, importing the result
//! into a new project and exporting again must give the same files, and
//! linked blocks must keep their IDs across the trip.

use lv_core::convert::import_document;
use lv_core::export::{ExportedFile, jupyter_book, obsidian_vault};
use lv_core::ids::{DocumentId, NodeId, OpId, PersonId, ProjectId};
use lv_core::import::{FolderFormat, SourceFile, import_folder};
use lv_core::myst::Node;
use lv_core::op::{DocKind, Op, OpBody, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::schema::builtins;
use lv_core::sequencer::{MemoryLog, Sequencer};
use lv_core::state::ProjectState;
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
    /// A project with the built-in vocabulary, as the app makes them.
    fn new() -> World {
        let person = Grant::new(Principal::Person { person: PersonId::new() }, GrantScope::Owner);
        let mut log = MemoryLog::default();
        log.add_grant(person.clone());
        let mut w = World { seq: Sequencer::new(ProjectId::new()), log, person };
        w.submit(builtins());
        w
    }

    fn state(&self) -> &ProjectState {
        self.seq.state()
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
        self.seq.submit(&mut self.log, batch).unwrap_or_else(|e| panic!("{e:?}"));
    }

    fn import(&mut self, title: &str, root: Value) -> DocumentId {
        let doc = DocumentId::new();
        let root: Node = serde_json::from_value(root).unwrap();
        let ops = import_document(doc, DocKind::note(), title, &root).unwrap();
        let mut batch = lv_core::schema::declare_unknown(self.state(), &ops);
        batch.extend(ops);
        self.submit(batch);
        doc
    }

    fn import_folder(&mut self, format: FolderFormat, files: &[ExportedFile]) {
        let files: Vec<SourceFile> = files.iter().map(|f| SourceFile { path: f.name.clone(), text: f.markdown.clone() }).collect();
        let imported = import_folder(self.state(), format, &files).unwrap();
        assert_eq!(imported.unresolved, 0, "every link resolves");
        self.submit(imported.ops);
    }
}

/// A project with every fixture, plus notes linking to each other: to a
/// whole note, to a labelled heading, to an unlabelled paragraph, and to a
/// block in the same note.
fn sample() -> (World, NodeId) {
    let mut w = World::new();
    for name in FIXTURES {
        w.import(name, fixture(name));
    }
    let (heading, claim) = (NodeId::new(), NodeId::new());
    let target = w.import(
        "Slow reading",
        json!({ "type": "root", "children": [
            { "type": "heading", "id": heading, "depth": 1, "identifier": "slow", "label": "slow", "children": [{ "type": "text", "value": "Slow" }] },
            { "type": "paragraph", "id": claim, "children": [{ "type": "text", "value": "A claim, with " }, { "type": "emphasis", "children": [{ "type": "text", "value": "weight" }] }, { "type": "text", "value": "." }] },
            { "type": "paragraph", "children": [{ "type": "link", "url": claim.to_string(), "children": [{ "type": "text", "value": "see above" }] }] },
        ]}),
    );
    w.import(
        "Notes: a draft?",
        json!({ "type": "root", "children": [
            { "type": "paragraph", "children": [
                { "type": "link", "url": target.to_string(), "children": [{ "type": "text", "value": "Slow reading" }] },
                { "type": "text", "value": ", " },
                { "type": "link", "url": heading.to_string(), "children": [{ "type": "text", "value": "the heading" }] },
                { "type": "text", "value": " and " },
                { "type": "link", "url": claim.to_string(), "children": [{ "type": "text", "value": "the claim" }] },
                { "type": "text", "value": "." },
            ]},
        ]}),
    );
    (w, claim)
}

/// Exports, imports into a new project, exports again: the files match.
fn round_trip(format: FolderFormat, export: impl Fn(&ProjectState) -> Vec<ExportedFile>) -> World {
    let (original, claim) = sample();
    let first = export(original.state());
    let mut copy = World::new();
    copy.import_folder(format, &first);
    let second = export(copy.state());
    // A vault has no order of its own, so compare by name.
    let sorted = |files: &[ExportedFile]| {
        let mut files = files.to_vec();
        if format == FolderFormat::Obsidian {
            files.sort_by(|a, b| a.name.cmp(&b.name));
        }
        files
    };
    let (first, second) = (sorted(&first), sorted(&second));
    let names = |files: &[ExportedFile]| files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&second), names(&first));
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(b.markdown, a.markdown, "{} changed on the round trip", a.name);
    }
    // The linked paragraph kept its ID, so links to it still resolve.
    let node = copy.state().node(claim).unwrap_or_else(|| panic!("{claim} was not kept"));
    assert_eq!(node.content.node_type, "paragraph");
    let links_to_claim = copy
        .state()
        .nodes()
        .filter(|(_, n)| n.content.fields.get("url").and_then(Value::as_str) == Some(&claim.to_string()))
        .count();
    assert_eq!(links_to_claim, 2);
    copy
}

#[test]
fn jupyter_book_round_trips() {
    let copy = round_trip(FolderFormat::Book, |s| jupyter_book(s, "Thesis"));
    let titles: Vec<String> = {
        let mut docs: Vec<_> = copy.state().documents().map(|(_, d)| (d.created, d.title.clone())).collect();
        docs.sort();
        docs.into_iter().map(|(_, t)| t).collect()
    };
    assert_eq!(titles, ["prose", "directives", "references", "unknown", "Slow reading", "Notes: a draft?"], "titles and order");
}

#[test]
fn obsidian_round_trips() {
    round_trip(FolderFormat::Obsidian, obsidian_vault);
}

#[test]
fn obsidian_export_reads_like_obsidian() {
    let (w, claim) = sample();
    let files = obsidian_vault(w.state());
    let file = |name: &str| &files.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("{name}")).markdown;
    let uuid = claim.to_string().trim_start_matches("node_").to_owned();
    let notes = file("Notes a draft.md");
    assert_eq!(notes, &format!("[[Slow reading]], [[Slow reading#Slow|the heading]] and [[Slow reading#^n-{uuid}|the claim]].\n"));
    let target = file("Slow reading.md");
    assert!(target.contains(&format!("A claim, with *weight*. ^n-{uuid}\n")), "{target}");
    assert!(target.contains(&format!("[[#^n-{uuid}|see above]]")), "{target}");
    let directives = file("directives.md");
    assert!(directives.starts_with("> [!note] Read this first\n> Arguments are cheap.") || directives.contains("```{note}"), "{directives}");
    assert!(directives.contains("%% TODO: tighten this section %%"), "{directives}");
}

#[test]
fn a_handwritten_vault() {
    let vault = [
        ("Reading/Slow reading.md", "---\ntags: [method]\n---\n# Why slow\n\nA claim. ^claim\n\n> [!warning] Careful\n> Not *always*.\n\n> [!lemma]\n> Small.\n"),
        ("Index.md", "See [[Slow reading]], [[slow reading#Why slow|why]], [[Slow reading#^claim]], [[Missing note]] and ![[diagram.png]].\n\n- item ^item-1\n\nBack to [[#^item-1|the item]]."),
        ("Other.md", "Links with markdown: [r](Reading/Slow%20Reading.md)."),
    ];
    let files: Vec<SourceFile> = vault.iter().map(|(p, t)| SourceFile { path: p.to_string(), text: t.to_string() }).collect();
    let mut w = World::new();
    let imported = import_folder(w.state(), FolderFormat::Obsidian, &files).unwrap();
    assert_eq!(imported.unresolved, 1, "only the missing note");
    let titles: Vec<&str> = imported.notes.iter().map(|n| n.title.as_str()).collect();
    assert_eq!(titles, ["Index", "Other", "Slow reading"]);
    let ids: Vec<DocumentId> = imported.notes.iter().map(|n| n.doc).collect();
    w.submit(imported.ops);

    let export = obsidian_vault(w.state());
    let index = &export.iter().find(|f| f.name == "Index.md").unwrap().markdown;
    // Wikilinks resolved to IDs and back: headings by text, blocks by new IDs.
    assert!(index.starts_with("See [[Slow reading]], [[Slow reading#Why slow|why]], [[Slow reading#^n-"), "{index}");
    assert!(!index.contains("|Slow reading#^claim]]"), "unaliased links stay unaliased: {index}");
    assert!(index.contains("[[Missing note]] and ![[diagram.png]]"), "{index}");
    assert!(index.contains("|the item]]"), "{index}");

    let state = w.state();
    let reading = state.document(ids[2]).unwrap();
    assert_eq!(reading.title, "Slow reading");
    let kinds: Vec<String> = state
        .nodes()
        .filter(|(_, n)| n.document == ids[2] && matches!(n.content.node_type.as_str(), "admonition" | "proof"))
        .map(|(_, n)| format!("{}:{}", n.content.node_type, n.content.fields["kind"].as_str().unwrap()))
        .collect();
    let mut kinds = kinds;
    kinds.sort();
    assert_eq!(kinds, ["admonition:warning", "proof:lemma"]);
    // The markdown link in Other.md found the file despite the case and escapes.
    let other_links: Vec<String> = state
        .nodes()
        .filter(|(_, n)| n.document == ids[1] && n.content.node_type == "link")
        .map(|(_, n)| n.content.fields["url"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(other_links, [ids[2].to_string()]);
}

#[test]
fn a_handwritten_book() {
    let book = [
        ("myst.yml", "version: 1\nproject:\n  title: Book\n  toc:\n    - file: intro.md\n    - title: Part\n      children:\n        - file: parts/a\n"),
        ("parts/a.md", "---\ntitle: Part A\n---\n\n(sec-a)=\n## A section\n\nBack to [the intro](../intro.md).\n"),
        ("intro.md", "# Intro\n\nSee [the section](parts/a.md#sec-a) and {ref}`sec-a`.\n"),
        ("zz-extra.md", "Not in the table of contents.\n"),
        ("analysis.ipynb", "{}"),
    ];
    let files: Vec<SourceFile> = book.iter().map(|(p, t)| SourceFile { path: p.to_string(), text: t.to_string() }).collect();
    let mut w = World::new();
    let imported = import_folder(w.state(), FolderFormat::Book, &files).unwrap();
    let titles: Vec<&str> = imported.notes.iter().map(|n| n.title.as_str()).collect();
    assert_eq!(titles, ["intro", "Part A", "zz-extra"], "toc order, then the rest");
    assert_eq!(imported.skipped, ["analysis.ipynb"]);
    assert_eq!(imported.unresolved, 0);
    let (intro, part) = (imported.notes[0].doc, imported.notes[1].doc);
    w.submit(imported.ops);

    let state = w.state();
    let section = state
        .nodes()
        .find(|(_, n)| n.document == part && n.content.node_type == "heading")
        .map(|(id, _)| id)
        .unwrap();
    let urls: Vec<(DocumentId, String)> = state
        .nodes()
        .filter(|(_, n)| n.content.node_type == "link")
        .map(|(_, n)| (n.document, n.content.fields["url"].as_str().unwrap().to_owned()))
        .collect();
    assert!(urls.contains(&(intro, section.to_string())), "{urls:?}");
    assert!(urls.contains(&(part, intro.to_string())), "{urls:?}");
    assert!(state.nodes().any(|(_, n)| n.content.node_type == "crossReference" && n.content.fields["identifier"] == "sec-a"));
}
