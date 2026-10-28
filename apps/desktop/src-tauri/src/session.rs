//! An open project: its file, sequencer and the local person's grant.
//!
//! This is the app's side of the client protocol. Every change goes through
//! [`Session::submit`], which stamps the ops with the person, grant and base
//! and hands them to the sequencer; nothing writes the store directly. It has
//! no Tauri types, so it is tested with plain `cargo test`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lv_core::convert::{
    Flavor, ImportError, MaterializeError, enclosing_block, import_document, materialize, plain_text, update_document,
};
use lv_core::ids::{DocumentId, NodeId, OpId, PersonId, ProjectId, Version};
use lv_core::myst::Node;
use lv_core::op::{DocKind, Op, OpBody, Parent, Relation, Sequenced, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::refusal::{Refusal, RefusalCode};
use lv_core::schema::{self, KindEntry, Namespace};
use lv_core::sequencer::{LoadError, Sequencer, SubmitError};
use lv_core::state::ProjectState;
use lv_core::store::{Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The person using this app, the same across projects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub person: PersonId,
    pub name: String,
}

impl Identity {
    pub fn new(name: impl Into<String>) -> Identity {
        Identity { person: PersonId::new(), name: name.into() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Materialize(#[from] MaterializeError),
    /// The editor sent something that is not a MyST tree.
    #[error("not a MyST document: {0}")]
    BadTree(String),
    /// The document changed after the version the editor based its edit on.
    #[error("{doc} changed at {changed}, after base {base}; reload it")]
    StaleBase { doc: DocumentId, base: Version, changed: Version },
    /// Writing an export failed.
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
}

impl Error {
    /// A stable code for the UI, like refusal codes for agents.
    pub fn code(&self) -> String {
        match self {
            Error::Refused(r) => serde_json::to_value(r.code).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
            Error::Store(_) | Error::Load(_) => "STORE".into(),
            Error::Import(_) | Error::Materialize(_) | Error::BadTree(_) => "INVALID_DOCUMENT".into(),
            Error::StaleBase { .. } => "STALE_BASE".into(),
            Error::Io(..) => "IO".into(),
        }
    }
}

impl From<SubmitError<StoreError>> for Error {
    fn from(e: SubmitError<StoreError>) -> Error {
        match e {
            SubmitError::Refused(r) => Error::Refused(r),
            SubmitError::Log(e) => Error::Store(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocumentSummary {
    pub id: DocumentId,
    pub kind: DocKind,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectInfo {
    pub path: PathBuf,
    pub name: String,
    pub version: Version,
    /// Live documents, oldest first.
    pub documents: Vec<DocumentSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoadedDocument {
    pub doc: DocumentId,
    pub title: String,
    pub version: Version,
    /// The document as native MyST (with node IDs).
    pub myst: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Saved {
    pub version: Version,
    pub ops: usize,
    /// `(old, new)` node IDs the editor should rename.
    pub renamed: Vec<(NodeId, NodeId)>,
}

/// A link into an open document, for its backlinks panel.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BacklinkInfo {
    /// The link node, and the document and title it sits in.
    pub source: NodeId,
    pub source_doc: DocumentId,
    pub source_title: String,
    /// The block it points at, or `None` for a link to the whole document.
    pub target: Option<NodeId>,
    pub relation: Option<Relation>,
    /// The text of the block the link sits in.
    pub context: String,
}

/// A block, with where it lives and its text: a search hit or a link target.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockInfo {
    pub node: NodeId,
    pub doc: DocumentId,
    pub title: String,
    /// The MyST type (`paragraph`, `heading`, ...).
    pub kind: String,
    pub text: String,
}

/// One entry of the project's registry, for UIs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RegistryEntry {
    pub namespace: Namespace,
    pub name: String,
    #[serde(flatten)]
    pub entry: KindEntry,
}

/// Where a link target (`doc_…` or `node_…`) is now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Located {
    pub doc: DocumentId,
    pub title: String,
    /// Set when the target is a block.
    pub block: Option<BlockInfo>,
}

/// Turns what someone typed into an FTS5 query: every word, as a prefix.
fn fts_query(text: &str) -> String {
    text.split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where an export went.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Exported {
    /// The folder (whole project) or file (one note) written.
    pub path: PathBuf,
    pub files: usize,
}

/// What a submit changed, for listeners (other editors, later the agent).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub version: Version,
    pub documents: Vec<DocumentId>,
}

pub struct Session {
    path: PathBuf,
    store: Store,
    seq: Sequencer,
    grant: Grant,
    /// When each document's content last changed in this session. Documents
    /// not listed have not changed since the project was opened.
    content_changed: HashMap<DocumentId, Version>,
}

impl Session {
    /// Creates a project file at `path`, owned by `me`.
    pub fn create(path: &Path, name: &str, me: &Identity) -> Result<Session> {
        let store = Store::create(path, ProjectId::new(), name)?;
        Session::start(path, store, me)
    }

    /// Opens a project file, registering `me` as an owner if it does not
    /// know them yet (v1 is single-user; sharing will replace this).
    pub fn open(path: &Path, me: &Identity) -> Result<Session> {
        let store = Store::open(path)?;
        Session::start(path, store, me)
    }

    fn start(path: &Path, mut store: Store, me: &Identity) -> Result<Session> {
        let holder = Principal::Person { person: me.person };
        if !store.has_person(me.person)? {
            store.add_person(me.person, &me.name)?;
        }
        let grant = match store.grants_held_by(holder)?.into_iter().find(|g| g.scope == GrantScope::Owner) {
            Some(grant) => grant,
            None => {
                let grant = Grant::new(holder, GrantScope::Owner);
                store.add_grant(&grant)?;
                grant
            }
        };
        let seq = Sequencer::load(&mut store)?;
        let mut session = Session { path: path.to_owned(), store, seq, grant, content_changed: HashMap::new() };
        // New projects, and projects from before the registry, get the
        // built-in vocabulary (and what their content already uses) once.
        let vocabulary = schema::bootstrap(session.state());
        if !vocabulary.is_empty() {
            session.submit(vocabulary)?;
        }
        Ok(session)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> &ProjectState {
        self.seq.state()
    }

    pub fn info(&self) -> Result<ProjectInfo> {
        let mut documents: Vec<_> = self
            .state()
            .documents()
            .filter(|(_, d)| !d.deleted)
            .map(|(id, d)| (d.created, DocumentSummary { id, kind: d.kind.clone(), title: d.title.clone() }))
            .collect();
        documents.sort_by_key(|(created, _)| *created);
        Ok(ProjectInfo {
            path: self.path.clone(),
            name: self.store.project_name()?,
            version: self.seq.head(),
            documents: documents.into_iter().map(|(_, d)| d).collect(),
        })
    }

    /// Creates a document holding one empty paragraph.
    pub fn create_document(&mut self, title: &str, kind: DocKind) -> Result<(DocumentId, Change)> {
        let doc = DocumentId::new();
        let root: Node = serde_json::from_value(json!({
            "type": "root",
            "children": [{ "type": "paragraph", "children": [] }]
        }))
        .expect("a valid MyST root");
        let change = self.submit(import_document(doc, kind, title, &root)?)?;
        Ok((doc, change))
    }

    /// Imports a MyST AST as a new document.
    pub fn import_document(&mut self, title: &str, kind: DocKind, root: &Node) -> Result<(DocumentId, Change)> {
        let doc = DocumentId::new();
        let ops = import_document(doc, kind, title, root)?;
        // Import is lenient: kinds the project doesn't know yet are declared
        // provisional, for review, rather than refused.
        let mut batch = schema::declare_unknown(self.state(), &ops);
        batch.extend(ops);
        let change = self.submit(batch)?;
        Ok((doc, change))
    }

    pub fn load_document(&self, doc: DocumentId) -> Result<LoadedDocument> {
        let title = self
            .state()
            .document(doc)
            .ok_or_else(|| Refusal::new(RefusalCode::UnknownDocument, format!("{doc} does not exist")))?
            .title
            .clone();
        let root = materialize(self.state(), doc, Flavor::Native)?;
        Ok(LoadedDocument { doc, title, version: self.seq.head(), myst: serde_json::to_value(root).expect("serializes") })
    }

    /// Saves an edited native-MyST tree over `doc`. `base` is the version
    /// the editor loaded or last saved; if the document's content changed
    /// after it, the save is refused rather than undoing that change.
    pub fn save_document(&mut self, doc: DocumentId, base: Version, myst: Value) -> Result<(Saved, Option<Change>)> {
        if let Some(&changed) = self.content_changed.get(&doc)
            && changed > base
        {
            return Err(Error::StaleBase { doc, base, changed });
        }
        let root: Node = serde_json::from_value(myst).map_err(|e| Error::BadTree(e.to_string()))?;
        let update = update_document(self.state(), doc, &root)?;
        let ops = update.ops.len();
        let change = if ops == 0 { None } else { Some(self.submit(update.ops)?) };
        Ok((Saved { version: self.seq.head(), ops, renamed: update.renamed }, change))
    }

    /// Links, references and citations pointing into `doc` (at it, or at
    /// one of its blocks), from any live document including itself.
    pub fn backlinks(&self, doc: DocumentId) -> Result<Vec<BacklinkInfo>> {
        let state = self.state();
        Ok(self
            .store
            .document_backlinks(doc)?
            .into_iter()
            .map(|b| BacklinkInfo {
                source: b.source,
                source_doc: b.source_doc,
                source_title: state.document(b.source_doc).map(|d| d.title.clone()).unwrap_or_default(),
                target: b.target,
                relation: b.relation,
                context: enclosing_block(state, b.source).map(|block| plain_text(state, block)).unwrap_or_default(),
            })
            .collect())
    }

    /// Blocks whose text matches every word of `text` (as prefixes), best
    /// first, at most `limit`.
    pub fn search_blocks(&self, text: &str, limit: usize) -> Result<Vec<BlockInfo>> {
        let query = fts_query(text);
        if query.is_empty() {
            return Ok(vec![]);
        }
        let mut seen = HashSet::new();
        let mut hits = vec![];
        for hit in self.store.search(&query)? {
            let Some(block) = enclosing_block(self.state(), hit) else { continue };
            if seen.insert(block)
                && let Some(info) = self.block_info(block)
            {
                hits.push(info);
                if hits.len() == limit {
                    break;
                }
            }
        }
        Ok(hits)
    }

    fn block_info(&self, node: NodeId) -> Option<BlockInfo> {
        let state = self.state();
        let n = state.node(node).filter(|n| !n.deleted)?;
        let doc = state.document(n.document).filter(|d| !d.deleted)?;
        if n.content.node_type == "root" {
            return None;
        }
        Some(BlockInfo {
            node,
            doc: n.document,
            title: doc.title.clone(),
            kind: n.content.node_type.clone(),
            text: plain_text(state, node),
        })
    }

    /// Where the target of a link (`doc_…` or `node_…`) is now; `None` if it
    /// is not an ID, or names something deleted or unknown.
    pub fn locate(&self, target: &str) -> Option<Located> {
        if let Ok(doc) = target.parse::<DocumentId>() {
            let d = self.state().document(doc).filter(|d| !d.deleted)?;
            return Some(Located { doc, title: d.title.clone(), block: None });
        }
        let block = self.block_info(target.parse::<NodeId>().ok()?)?;
        Some(Located { doc: block.doc, title: block.title.clone(), block: Some(block) })
    }

    /// The project's vocabulary, by namespace then name.
    pub fn registry(&self) -> Vec<RegistryEntry> {
        self.state()
            .registry()
            .entries()
            .map(|(namespace, name, entry)| RegistryEntry { namespace, name: name.to_owned(), entry: entry.clone() })
            .collect()
    }

    /// Writes every live document as MyST Markdown into a new folder inside
    /// `dir`, named after the project (`-2`, `-3`, ... if taken), so an
    /// export never overwrites anything.
    pub fn export_project(&self, dir: &Path) -> Result<Exported> {
        let base = lv_core::export::slug(&self.store.project_name()?);
        let mut folder = dir.join(&base);
        let mut n = 2;
        while folder.exists() {
            folder = dir.join(format!("{base}-{n}"));
            n += 1;
        }
        std::fs::create_dir_all(&folder).map_err(|e| Error::Io(folder.clone(), e))?;
        let files = lv_core::export::project_markdown(self.state());
        for file in &files {
            let path = folder.join(&file.name);
            std::fs::write(&path, &file.markdown).map_err(|e| Error::Io(path, e))?;
        }
        Ok(Exported { path: folder, files: files.len() })
    }

    /// Writes `doc` as MyST Markdown to `path` (the save dialog has already
    /// asked before replacing a file).
    pub fn export_document(&self, doc: DocumentId, path: &Path) -> Result<Exported> {
        let file = lv_core::export::document_markdown(self.state(), doc)
            .ok_or_else(|| Refusal::new(RefusalCode::UnknownDocument, format!("{doc} does not exist")))?;
        std::fs::write(path, file.markdown).map_err(|e| Error::Io(path.to_owned(), e))?;
        Ok(Exported { path: path.to_owned(), files: 1 })
    }

    pub fn rename_document(&mut self, doc: DocumentId, title: &str) -> Result<Change> {
        self.submit(vec![OpBody::SetDocumentTitle { doc, title: title.to_owned() }])
    }

    pub fn delete_document(&mut self, doc: DocumentId) -> Result<Change> {
        self.submit(vec![OpBody::DeleteDocument { doc }])
    }

    /// Stamps `bodies` as the local person's ops against the head, and
    /// submits them as one batch.
    pub fn submit(&mut self, bodies: Vec<OpBody>) -> Result<Change> {
        let base = self.seq.head();
        let batch = bodies
            .into_iter()
            .map(|body| Op {
                id: OpId::new(),
                project: self.seq.project(),
                principal: self.grant.holder,
                grant: self.grant.id,
                base,
                change_set: None,
                created_at: Timestamp::now(),
                body,
            })
            .collect();
        let before = self.seq.state().clone();
        let accepted = self.seq.submit(&mut self.store, batch)?;
        let version = self.seq.head();
        let documents = touched_documents(&before, self.seq.state(), &accepted);
        for &doc in &documents {
            self.content_changed.insert(doc, version);
        }
        let mut documents: Vec<_> = documents.into_iter().collect();
        documents.sort();
        Ok(Change { version, documents })
    }
}

/// Documents whose content (or existence) `ops` changed, looking at where
/// the nodes they address were before and after.
fn touched_documents(before: &ProjectState, after: &ProjectState, ops: &[Sequenced]) -> HashSet<DocumentId> {
    let mut docs = HashSet::new();
    let node_docs = |id: NodeId| [before.node(id), after.node(id)].into_iter().flatten().map(|n| n.document);
    let mut bodies: Vec<&OpBody> = ops.iter().filter(|s| s.op.change_set.is_none()).map(|s| &s.op.body).collect();
    while let Some(body) = bodies.pop() {
        match body {
            OpBody::CreateDocument { doc, .. } | OpBody::DeleteDocument { doc } => {
                docs.insert(*doc);
            }
            OpBody::SetDocumentTitle { .. }
            | OpBody::OpenChangeSet { .. }
            | OpBody::RejectChangeSet { .. }
            | OpBody::DeclareKind { .. }
            | OpBody::UpdateKind { .. } => {}
            // Renames and merges rewrite `kind` / `relation` on every use.
            OpBody::RenameKind { namespace, from, .. } | OpBody::MergeKind { namespace, from, .. } => {
                let (_, nodes) = before.kind_uses(*namespace, from);
                docs.extend(nodes.into_iter().flat_map(node_docs));
            }
            OpBody::InsertNode { node, parent, .. } | OpBody::MoveNode { node, parent, .. } => {
                match parent {
                    Parent::DocumentRoot(doc) => {
                        docs.insert(*doc);
                    }
                    Parent::Node(p) => docs.extend(node_docs(*p)),
                }
                docs.extend(node_docs(*node));
            }
            OpBody::DeleteNode { node }
            | OpBody::SetAttrs { node, .. }
            | OpBody::EditText { node, .. }
            | OpBody::SetType { node, .. }
            | OpBody::SetRelation { node, .. } => docs.extend(node_docs(*node)),
            OpBody::AcceptChangeSet { change_set } => {
                bodies.extend(before.change_set(*change_set).into_iter().flat_map(|cs| cs.ops.iter().map(|s| &s.op.body)));
            }
        }
    }
    docs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, PathBuf, Identity) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.lv");
        (dir, path, Identity::new("Ada"))
    }

    #[test]
    fn create_edit_save_and_reopen() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "Thesis", &me).unwrap();
        let (doc, change) = s.create_document("Reading notes", DocKind::note()).unwrap();
        assert_eq!(change.documents, [doc]);

        let mut loaded = s.load_document(doc).unwrap();
        assert_eq!(loaded.myst["type"], "root");
        assert!(loaded.myst["id"].is_string(), "native MyST carries IDs");
        let paragraph = &mut loaded.myst["children"][0];
        assert_eq!(paragraph["type"], "paragraph");
        paragraph["children"] = json!([{ "type": "text", "value": "Slow reading." }]);

        let (saved, change) = s.save_document(doc, loaded.version, loaded.myst.clone()).unwrap();
        assert_eq!((saved.ops, saved.renamed.len()), (1, 0));
        assert_eq!(change.unwrap().documents, [doc]);
        let again = s.save_document(doc, saved.version, loaded.myst.clone()).unwrap();
        assert_eq!((again.0.ops, again.1), (0, None), "saving the same tree is a no-op");
        drop(s);

        let s = Session::open(&path, &me).unwrap();
        assert_eq!(s.load_document(doc).unwrap().myst, loaded.myst);
        let info = s.info().unwrap();
        assert_eq!((info.name.as_str(), info.documents.len()), ("Thesis", 1));
    }

    #[test]
    fn saves_based_on_an_old_version_of_the_document_are_refused() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let (doc, _) = s.create_document("A", DocKind::note()).unwrap();
        let (other, _) = s.create_document("B", DocKind::note()).unwrap();
        let mut stale = s.load_document(doc).unwrap();

        // Other documents and titles do not make this one stale.
        let mut b = s.load_document(other).unwrap();
        b.myst["children"][0]["children"] = json!([{ "type": "text", "value": "elsewhere" }]);
        s.save_document(other, b.version, b.myst).unwrap();
        s.rename_document(doc, "A, renamed").unwrap();

        let mut fresh = s.load_document(doc).unwrap();
        fresh.myst["children"][0]["children"] = json!([{ "type": "text", "value": "first" }]);
        s.save_document(doc, stale.version, fresh.myst.clone()).unwrap();

        stale.myst["children"][0]["children"] = json!([{ "type": "text", "value": "second" }]);
        let err = s.save_document(doc, stale.version, stale.myst).unwrap_err();
        assert_eq!(err.code(), "STALE_BASE");
        assert_eq!(s.load_document(doc).unwrap().myst, fresh.myst, "nothing was undone");
    }

    #[test]
    fn reopening_reuses_the_owner_grant() {
        let (_dir, path, me) = project();
        drop(Session::create(&path, "P", &me).unwrap());
        drop(Session::open(&path, &me).unwrap());
        let store = Store::open(&path).unwrap();
        assert_eq!(store.grants_held_by(Principal::Person { person: me.person }).unwrap().len(), 1);

        // Someone else opening the file is registered as a second owner (v1).
        let other = Identity::new("Grace");
        drop(Session::open(&path, &other).unwrap());
        assert_eq!(Store::open(&path).unwrap().grants_held_by(Principal::Person { person: other.person }).unwrap().len(), 1);
    }

    #[test]
    fn refusals_carry_their_code() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let err = s.rename_document(DocumentId::new(), "x").unwrap_err();
        assert_eq!(err.code(), "UNKNOWN_DOCUMENT");
        let (doc, _) = s.create_document("A", DocKind::note()).unwrap();
        let err = s.save_document(doc, s.seq.head(), json!({ "children": [] })).unwrap_err();
        assert_eq!(err.code(), "INVALID_DOCUMENT");
    }

    #[test]
    fn links_to_notes_and_blocks_show_as_backlinks() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let (target, _) = s.create_document("Target", DocKind::note()).unwrap();
        let (source, _) = s.create_document("Source", DocKind::note()).unwrap();
        let mut t = s.load_document(target).unwrap();
        t.myst["children"][0]["children"] = json!([{ "type": "text", "value": "Marginalia are arguments." }]);
        s.save_document(target, t.version, t.myst.clone()).unwrap();
        let block: NodeId = t.myst["children"][0]["id"].as_str().unwrap().parse().unwrap();

        // Find the block by searching, as the link picker does.
        let hits = s.search_blocks("margin argu", 10).unwrap();
        assert_eq!(hits.iter().map(|h| (h.node, h.doc, h.kind.as_str())).collect::<Vec<_>>(), [(block, target, "paragraph")]);
        assert!(s.search_blocks("  ", 10).unwrap().is_empty());
        assert!(s.search_blocks("\"unbalanced", 10).is_ok(), "quotes are escaped");

        let mut src = s.load_document(source).unwrap();
        src.myst["children"][0]["children"] = json!([
            { "type": "text", "value": "See " },
            { "type": "link", "id": NodeId::new().to_string(), "url": target.to_string(), "children": [{ "type": "text", "value": "the note" }] },
            { "type": "text", "value": " and " },
            { "type": "link", "id": NodeId::new().to_string(), "url": block.to_string(), "children": [{ "type": "text", "value": "this claim" }] },
        ]);
        s.save_document(source, src.version, src.myst.clone()).unwrap();
        drop(s);

        let s = Session::open(&path, &me).unwrap();
        let links = s.backlinks(target).unwrap();
        let mut got: Vec<_> = links.iter().map(|b| (b.target, b.source_title.as_str(), b.context.as_str())).collect();
        got.sort();
        assert_eq!(got, [(None, "Source", "See the note and this claim"), (Some(block), "Source", "See the note and this claim")]);
        assert!(s.backlinks(source).unwrap().is_empty());

        assert_eq!(s.locate(&target.to_string()).unwrap().title, "Target");
        let located = s.locate(&block.to_string()).unwrap();
        assert_eq!((located.doc, located.block.unwrap().text.as_str()), (target, "Marginalia are arguments."));
        assert_eq!(s.locate("https://example.org"), None);
        assert_eq!(s.locate(&NodeId::new().to_string()), None);
    }

    #[test]
    fn links_to_a_block_survive_changing_its_type() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let (doc, _) = s.create_document("A", DocKind::note()).unwrap();
        let mut a = s.load_document(doc).unwrap();
        let block: NodeId = a.myst["children"][0]["id"].as_str().unwrap().parse().unwrap();
        a.myst["children"].as_array_mut().unwrap().push(json!({
            "type": "paragraph", "id": NodeId::new(),
            "children": [{ "type": "link", "id": NodeId::new(), "url": block.to_string(), "children": [] }]
        }));
        let (saved, _) = s.save_document(doc, a.version, a.myst.clone()).unwrap();

        // Paragraph → heading, as the editor's block menu sends it.
        a.myst["children"][0]["type"] = json!("heading");
        a.myst["children"][0]["depth"] = json!(1);
        let (saved, _) = s.save_document(doc, saved.version, a.myst.clone()).unwrap();
        assert_eq!((saved.ops, saved.renamed.len()), (1, 0));
        assert_eq!(s.backlinks(doc).unwrap()[0].target, Some(block));
        assert_eq!(s.locate(&block.to_string()).unwrap().block.unwrap().kind, "heading");
    }

    #[test]
    fn exports_never_overwrite_a_folder() {
        let (dir, path, me) = project();
        let mut s = Session::create(&path, "My Thesis", &me).unwrap();
        let (doc, _) = s.create_document("Reading notes", DocKind::note()).unwrap();
        s.create_document("Reading notes", DocKind::note()).unwrap();

        let first = s.export_project(dir.path()).unwrap();
        assert_eq!((first.path.file_name().unwrap().to_str(), first.files), (Some("my-thesis"), 2));
        let second = s.export_project(dir.path()).unwrap();
        assert_eq!(second.path.file_name().unwrap().to_str(), Some("my-thesis-2"));
        let mut names: Vec<_> = std::fs::read_dir(&first.path).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["reading-notes-2.md", "reading-notes.md"]);

        let one = dir.path().join("one.md");
        s.export_document(doc, &one).unwrap();
        assert!(std::fs::read_to_string(&one).unwrap().starts_with("---\ntitle: Reading notes\n---"));
        assert_eq!(s.export_document(DocumentId::new(), &one).unwrap_err().code(), "UNKNOWN_DOCUMENT");
        assert_eq!(s.export_document(doc, &dir.path().join("missing/x.md")).unwrap_err().code(), "IO");
    }

    #[test]
    fn projects_get_the_builtin_vocabulary_once() {
        let (_dir, path, me) = project();
        let s = Session::create(&path, "P", &me).unwrap();
        let relations: Vec<_> = s.registry().into_iter().filter(|e| e.namespace == Namespace::Relation).map(|e| e.name).collect();
        assert!(relations.contains(&"supports".to_owned()));
        let head = s.seq.head();
        drop(s);
        let s = Session::open(&path, &me).unwrap();
        assert_eq!(s.seq.head(), head, "nothing new on reopening");
    }

    #[test]
    fn relations_must_be_declared() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let (doc, _) = s.create_document("A", DocKind::note()).unwrap();
        let mut a = s.load_document(doc).unwrap();
        a.myst["children"][0]["children"] = json!([
            { "type": "link", "id": NodeId::new(), "url": "https://example.org", "relation": "supports", "children": [] }
        ]);
        let (saved, _) = s.save_document(doc, a.version, a.myst.clone()).unwrap();
        a.myst["children"][0]["children"][0]["relation"] = json!("vibes-with");
        let err = s.save_document(doc, saved.version, a.myst.clone()).unwrap_err();
        assert_eq!(err.code(), "UNDECLARED_KIND");

        // Imports declare what they bring, provisionally.
        let root: Node = serde_json::from_value(json!({ "type": "root", "children": [{ "type": "paragraph", "children": [
            { "type": "link", "url": "https://example.org", "relation": "vibes-with", "children": [] }
        ]}]}))
        .unwrap();
        s.import_document("Imported", DocKind::note(), &root).unwrap();
        let entry = s.registry().into_iter().find(|e| e.name == "vibes-with").unwrap();
        assert_eq!(entry.entry.status, schema::KindStatus::Provisional);
    }

    #[test]
    fn moves_touch_both_documents() {
        let (_dir, path, me) = project();
        let mut s = Session::create(&path, "P", &me).unwrap();
        let (a, _) = s.create_document("A", DocKind::note()).unwrap();
        let (b, _) = s.create_document("B", DocKind::note()).unwrap();
        let mut from = s.load_document(a).unwrap();
        let mut to = s.load_document(b).unwrap();
        let paragraph = from.myst["children"].as_array_mut().unwrap().remove(0);
        to.myst["children"].as_array_mut().unwrap().push(paragraph);

        let (_, change) = s.save_document(b, to.version, to.myst).unwrap();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(change.unwrap().documents, expected);
        // The source changed under its editor, which must reload it.
        assert_eq!(s.save_document(a, from.version, from.myst).unwrap_err().code(), "STALE_BASE");
        assert_eq!(s.load_document(a).unwrap().myst["children"], json!([]));
    }
}
