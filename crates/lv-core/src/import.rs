//! Importing a folder of Markdown: a Jupyter Book 2 project or an Obsidian
//! vault, as one batch of ops that creates a note per file.
//!
//! Files are parsed with [`crate::markdown`], then:
//!
//! - every block gets a node ID. Blocks that carry one from an lv-notes
//!   export keep it: a `(node-<uuid>)=` target (MyST) or a `^n-<uuid>`
//!   block ID (Obsidian) names the block's old ID, so links made before the
//!   export still point at it after the round trip. An ID the project
//!   already uses is replaced, so importing twice makes two copies;
//! - links between files become ID links: `[x](other.md#label)` and
//!   `[[Other#Heading]]`, `[[Other#^block]]` point at `doc_…` or `node_…`.
//!   A wikilink to a note that isn't in the folder stays text.
//!
//! Nothing here touches the file system: the caller reads the folder.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::convert::{ImportError, import_document};
use crate::ids::{DocumentId, NodeId};
use crate::markdown::{Dialect, identifier, parse, yaml_to_json};
use crate::myst::{Node, TEXT_RUN_TYPES};
use crate::op::{DocKind, OpBody};
use crate::schema::declare_unknown;
use crate::state::ProjectState;

/// Which kind of folder is being imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderFormat {
    /// A Jupyter Book 2 (mystmd) project: MyST Markdown, ordered by `myst.yml`.
    Book,
    /// An Obsidian vault.
    Obsidian,
}

/// A file read from the folder. `path` is relative to the folder, with `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedNote {
    pub doc: DocumentId,
    pub title: String,
    pub path: String,
}

/// What importing a folder would do.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderImport {
    /// One batch: provisional declarations for unknown kinds, then each note.
    pub ops: Vec<OpBody>,
    pub notes: Vec<ImportedNote>,
    /// Files that were not imported (notebooks, for now).
    pub skipped: Vec<String>,
    /// Links to notes or blocks that aren't in the folder.
    pub unresolved: usize,
}

/// The ops that import `files` as new notes in `state`'s project.
pub fn import_folder(state: &ProjectState, format: FolderFormat, files: &[SourceFile]) -> Result<FolderImport, ImportError> {
    let dialect = match format {
        FolderFormat::Book => Dialect::Myst,
        FolderFormat::Obsidian => Dialect::Obsidian,
    };
    let is_markdown = |p: &str| p.ends_with(".md") || p.ends_with(".markdown");
    let config = files.iter().find(|f| f.path == "myst.yml").and_then(|f| yaml_to_json(&f.text));
    let mut skipped: Vec<String> = files.iter().filter(|f| !is_markdown(&f.path) && f.path != "myst.yml").map(|f| f.path.clone()).collect();
    skipped.sort();

    // Order: the book's table of contents first, then the rest by path.
    let mut order: Vec<&SourceFile> = Vec::new();
    if let Some(toc) = config.as_ref().and_then(|c| c.pointer("/project/toc")) {
        let mut paths = Vec::new();
        toc_files(toc, &mut paths);
        for p in paths {
            let p = if p.contains('.') { p } else { format!("{p}.md") };
            if let Some(f) = files.iter().find(|f| f.path == p.trim_start_matches("./")) && !order.iter().any(|o| o.path == f.path) {
                order.push(f);
            }
        }
    }
    let mut rest: Vec<&SourceFile> = files.iter().filter(|f| is_markdown(&f.path) && !order.iter().any(|o| o.path == f.path)).collect();
    rest.sort_by(|a, b| a.path.cmp(&b.path));
    order.extend(rest);

    let mut notes = Vec::new();
    let mut trees = Vec::new();
    for file in &order {
        let mut tree = parse(&file.text, dialect);
        let stem = stem(&file.path).to_owned();
        let title = match format {
            // Obsidian names a note by its file.
            FolderFormat::Obsidian => stem,
            FolderFormat::Book => {
                let title = tree.pointer("/frontmatter/title").and_then(Value::as_str).map(str::to_owned);
                if let Some(fm) = tree.get_mut("frontmatter").and_then(Value::as_object_mut) {
                    fm.remove("title");
                    if fm.is_empty() {
                        tree.as_object_mut().unwrap().remove("frontmatter");
                    }
                }
                title.unwrap_or(stem)
            }
        };
        notes.push(ImportedNote { doc: DocumentId::new(), title, path: file.path.clone() });
        trees.push(tree);
    }

    let mut ids = Ids { state, used: HashSet::new(), anchors: HashMap::new() };
    for (note, tree) in notes.iter().zip(trees.iter_mut()) {
        ids.assign(note.doc, tree);
    }
    let resolver = Resolver { notes: &notes, anchors: &ids.anchors, known: &ids.used };
    let mut unresolved = 0;
    for (i, tree) in trees.iter_mut().enumerate() {
        unresolved += resolver.links(i, tree);
    }

    let mut ops = Vec::new();
    for (note, tree) in notes.iter().zip(trees) {
        let root: Node = serde_json::from_value(tree).map_err(|e| ImportError::Invalid(format!("{}: {e}", note.path)))?;
        ops.extend(import_document(note.doc, DocKind::note(), &note.title, &root)?);
    }
    let mut batch = declare_unknown(state, &ops);
    batch.extend(ops);
    Ok(FolderImport { ops: batch, notes, skipped, unresolved })
}

fn toc_files(toc: &Value, out: &mut Vec<String>) {
    for entry in toc.as_array().into_iter().flatten() {
        if let Some(file) = entry.get("file").and_then(Value::as_str) {
            out.push(file.to_owned());
        }
        if let Some(children) = entry.get("children") {
            toc_files(children, out);
        }
    }
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// A path without its extension, lowercased: how links name files.
fn path_key(path: &str) -> String {
    let p = path.trim_start_matches("./");
    let p = p.strip_suffix(".md").or_else(|| p.strip_suffix(".markdown")).unwrap_or(p);
    p.to_lowercase()
}

/// `node-<uuid>` (MyST target) or `n-<uuid>` (Obsidian block ID) → the ID.
fn exported_id(label: &str) -> Option<NodeId> {
    let uuid = label.strip_prefix("node-").or_else(|| label.strip_prefix("n-"))?;
    format!("node_{uuid}").parse().ok()
}

fn ty(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

fn plain(node: &Value) -> String {
    match node.get("value").and_then(Value::as_str) {
        Some(v) => v.to_owned(),
        None => node.get("children").and_then(Value::as_array).map_or(String::new(), |c| c.iter().map(plain).collect()),
    }
}

struct Ids<'a> {
    state: &'a ProjectState,
    used: HashSet<NodeId>,
    /// (document, key) → node: `#identifier` for labels, `^id` for
    /// Obsidian block IDs, `h:heading text` for headings.
    anchors: HashMap<(DocumentId, String), NodeId>,
}

impl Ids<'_> {
    fn fresh(&mut self, wanted: Option<NodeId>) -> NodeId {
        let id = wanted.filter(|id| self.state.node(*id).is_none() && !self.used.contains(id)).unwrap_or_else(NodeId::new);
        self.used.insert(id);
        id
    }

    /// Gives `node` and its descendants IDs, recording what links can name.
    fn assign(&mut self, doc: DocumentId, node: &mut Value) {
        let t = ty(node).to_owned();
        if !TEXT_RUN_TYPES.contains(&t.as_str()) {
            let block_id = node.as_object_mut().and_then(|m| m.remove("_block_id")).and_then(|v| v.as_str().map(str::to_owned));
            let synthetic = node.as_object_mut().and_then(|m| m.remove("_exported_id")).and_then(|v| v.as_str().and_then(|s| s.parse().ok()));
            let wanted = synthetic.or_else(|| block_id.as_deref().and_then(exported_id));
            let id = self.fresh(wanted);
            node["id"] = json!(id.to_string());
            if let Some(b) = block_id {
                self.anchors.insert((doc, format!("^{b}")), id);
            }
            if let Some(label) = node.get("identifier").and_then(Value::as_str).filter(|_| !matches!(t.as_str(), "crossReference" | "cite" | "footnoteReference")) {
                self.anchors.insert((doc, format!("#{label}")), id);
            }
            if t == "heading" {
                self.anchors.entry((doc, format!("h:{}", plain(node).trim().to_lowercase()))).or_insert(id);
            }
        }
        let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) else { return };
        // A target names the block after it. Ours (`node-<uuid>`) only
        // carried an ID across the export, so it goes.
        let mut i = 0;
        while i < children.len() {
            if ty(&children[i]) == "mystTarget" && i + 1 < children.len() {
                let label = children[i].get("label").and_then(Value::as_str).unwrap_or("").to_owned();
                if let Some(id) = exported_id(&label).filter(|_| label.starts_with("node-")) {
                    children.remove(i);
                    children[i]["_exported_id"] = json!(id.to_string());
                    continue;
                }
                children[i + 1]["_target"] = json!(label);
            }
            i += 1;
        }
        for child in children.iter_mut() {
            let target = child.as_object_mut().and_then(|m| m.remove("_target")).and_then(|v| v.as_str().map(str::to_owned));
            self.assign(doc, child);
            if let (Some(label), Some(id)) = (target, child.get("id").and_then(Value::as_str).and_then(|s| s.parse().ok())) {
                self.anchors.insert((doc, format!("#{}", identifier(&label))), id);
            }
        }
    }
}

struct Resolver<'a> {
    notes: &'a [ImportedNote],
    anchors: &'a HashMap<(DocumentId, String), NodeId>,
    /// Every node ID this import gives out.
    known: &'a HashSet<NodeId>,
}

impl Resolver<'_> {
    /// Rewrites the links in note `i`'s tree; returns how many stayed
    /// unresolved.
    fn links(&self, i: usize, node: &mut Value) -> usize {
        let mut unresolved = 0;
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            for child in children.iter_mut() {
                if ty(child) == "link" {
                    if let Some(raw) = child.as_object_mut().and_then(|m| m.remove("_wikilink")).and_then(|v| v.as_str().map(str::to_owned)) {
                        let alias = child.as_object_mut().and_then(|m| m.remove("_alias")).and_then(|v| v.as_bool()).unwrap_or(false);
                        match self.wikilink(i, &raw) {
                            Some(url) => child["url"] = json!(url),
                            None => {
                                // Not in this folder: keep it as written.
                                let shown = if alias { format!("[[{raw}|{}]]", plain(child)) } else { format!("[[{raw}]]") };
                                *child = json!({ "type": "text", "value": shown });
                                unresolved += 1;
                                continue;
                            }
                        }
                    } else if let Some(url) = child.get("url").and_then(Value::as_str).map(str::to_owned) {
                        match self.relative(i, &url) {
                            Some(Some(resolved)) => child["url"] = json!(resolved),
                            Some(None) => unresolved += 1,
                            None => {}
                        }
                    }
                }
                unresolved += self.links(i, child);
            }
            merge_text(children);
        }
        unresolved
    }

    fn note_by_name(&self, from: usize, name: &str) -> Option<&ImportedNote> {
        if name.is_empty() {
            return Some(&self.notes[from]);
        }
        let key = path_key(name);
        self.notes
            .iter()
            .find(|n| path_key(&n.path) == key)
            .or_else(|| self.notes.iter().find(|n| path_key(stem(&n.path)) == path_key(key.rsplit('/').next().unwrap_or(&key))))
    }

    /// `Note`, `Note#Heading`, `Note#^block`, `#^block` → an ID url.
    fn wikilink(&self, from: usize, raw: &str) -> Option<String> {
        let (name, fragment) = raw.split_once('#').map_or((raw, None), |(n, f)| (n, Some(f)));
        let note = self.note_by_name(from, name.trim())?;
        let node = fragment.and_then(|f| {
            let f = f.trim();
            match f.strip_prefix('^') {
                Some(block) => self.anchors.get(&(note.doc, format!("^{block}"))),
                None => self
                    .anchors
                    .get(&(note.doc, format!("h:{}", f.to_lowercase())))
                    .or_else(|| self.anchors.get(&(note.doc, format!("#{}", identifier(f))))),
            }
        });
        Some(node.map_or_else(|| note.doc.to_string(), |n| n.to_string()))
    }

    /// A relative link to another file in the folder. `None`: not one (web
    /// links, anchors); `Some(None)`: one, but the file isn't there.
    fn relative(&self, from: usize, url: &str) -> Option<Option<String>> {
        if let Some(label) = url.strip_prefix("#node-") {
            return Some(exported_id(&format!("node-{label}")).filter(|id| self.known.contains(id)).map(|id| id.to_string()));
        }
        if url.is_empty() || url.starts_with('#') || url.starts_with('/') || url.contains("://") || url.starts_with("mailto:") {
            return None;
        }
        let decoded = percent_decode(url);
        let (path, fragment) = decoded.split_once('#').map_or((decoded.as_str(), None), |(p, f)| (p, Some(f)));
        if !(path.ends_with(".md") || path.ends_with(".markdown") || !path.contains('.')) {
            return None;
        }
        let here = &self.notes[from].path;
        let dir = here.rsplit_once('/').map_or("", |(d, _)| d);
        let joined = join(dir, path);
        let Some(note) = self.notes.iter().find(|n| path_key(&n.path) == path_key(&joined)) else { return Some(None) };
        let node = fragment.and_then(|f| {
            exported_id(f)
                .filter(|id| self.known.contains(id))
                .or_else(|| f.strip_prefix('^').and_then(|b| self.anchors.get(&(note.doc, format!("^{b}"))).copied()))
                .or_else(|| self.anchors.get(&(note.doc, format!("#{}", identifier(f)))).copied())
                .or_else(|| self.anchors.get(&(note.doc, format!("h:{}", f.replace('-', " ").to_lowercase()))).copied())
        });
        Some(Some(node.map_or_else(|| note.doc.to_string(), |n| n.to_string())))
    }
}

/// `dir` + a relative `path`, with `.` and `..` resolved.
fn join(dir: &str, path: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

fn merge_text(children: &mut Vec<Value>) {
    let mut out: Vec<Value> = Vec::with_capacity(children.len());
    for node in children.drain(..) {
        if ty(&node) == "text"
            && let Some(last) = out.last_mut()
            && ty(last) == "text"
            && last.as_object().is_some_and(|m| m.len() == 2)
            && node.as_object().is_some_and(|m| m.len() == 2)
        {
            let joined = format!("{}{}", plain(last), plain(&node));
            last["value"] = json!(joined);
            continue;
        }
        out.push(node);
    }
    *children = out;
}
