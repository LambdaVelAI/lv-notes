//! MyST documents into and out of a project.
//!
//! Importing turns a MyST AST into the ops that build it: `create_document`,
//! then one `insert_node` per node in document order, each placed after its
//! previous sibling. Nodes that already carry an lv-notes `id` keep it; the
//! rest get new IDs. Materializing reads a document back out of a
//! [`ProjectState`] as a MyST AST.
//!
//! A document's tree is the MyST tree, root included: an imported document
//! has one top-level `root` node, whose own fields (frontmatter, ...) are
//! edited with `set_attrs` like any other node's. A document built by ops
//! without a `root` materializes under a synthesized one.
//!
//! Saving an edited document goes the other way: [`update_document`]
//! compares an edited native-MyST tree with the document and returns the
//! ops between them, so editors send trees and the core decides the ops.
//!
//! Exported MyST comes in two flavors. [`Flavor::Plain`] is standard MyST.
//! [`Flavor::Native`] adds the lv-notes fields: `id` on every node except
//! text runs ([`TEXT_RUN_TYPES`]), and `relation` on typed links. Importing
//! reads both fields back, so a native export re-imports with the same IDs
//! and relations.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::ids::{DocumentId, NodeId};
use crate::myst::{ChildrenPolicy, Node, TEXT_RUN_TYPES, children_policy};
use crate::op::{DocKind, NodeContent, OpBody, Parent, Relation, TextEdit};
use crate::state::{EDGE_TYPES, ProjectState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Standard MyST, without lv-notes fields.
    Plain,
    /// MyST plus `id` on blocks and anchors and `relation` on typed links.
    Native,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("a document is imported from a `root` node, not a `{0}`")]
    NotARoot(String),
    #[error("{0} appears more than once")]
    DuplicateId(NodeId),
    #[error("`{0}` is not an lv-notes node ID")]
    BadId(String),
    #[error("a `{0}` node has children that are not MyST nodes")]
    BadChildren(String),
    #[error("{0} does not exist")]
    UnknownDocument(DocumentId),
    /// An edited tree must carry the ID of every node but text runs.
    #[error("a `{0}` node has no `id`")]
    MissingId(String),
    #[error("a `{0}` node's `value` must be a string")]
    BadValue(String),
    /// A parsed file did not form a MyST tree.
    #[error("not a MyST document: {0}")]
    Invalid(String),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MaterializeError {
    #[error("{0} does not exist")]
    UnknownDocument(DocumentId),
    /// The state holds content that does not read as MyST. Op checks are
    /// meant to make this impossible.
    #[error("{0} does not materialize as MyST: {1}")]
    Invalid(DocumentId, String),
}

/// The ops that create document `doc` with the content of `root`.
pub fn import_document(
    doc: DocumentId,
    kind: DocKind,
    title: impl Into<String>,
    root: &Node,
) -> Result<Vec<OpBody>, ImportError> {
    if root.type_name() != "root" {
        return Err(ImportError::NotARoot(root.type_name().to_owned()));
    }
    let mut importer = Importer::default();
    importer.ops.push(OpBody::CreateDocument { doc, kind, title: title.into() });
    importer.node(to_json(root), Parent::DocumentRoot(doc), None, "root")?;
    Ok(importer.ops)
}

/// The ops that insert `nodes`, in order, under `parent` after `after`:
/// pasting a fragment into an existing document.
pub fn import_nodes(parent: Parent, after: Option<NodeId>, nodes: &[Node]) -> Result<Vec<OpBody>, ImportError> {
    let mut importer = Importer::default();
    let mut after = after;
    for node in nodes {
        after = Some(importer.node(to_json(node), parent, after, node.type_name())?);
    }
    Ok(importer.ops)
}

#[derive(Default)]
struct Importer {
    ops: Vec<OpBody>,
    seen: HashSet<NodeId>,
}

impl Importer {
    /// Emits the ops for `value` and its subtree; returns its ID.
    /// `parent_type` is only for error messages.
    fn node(&mut self, value: Value, parent: Parent, after: Option<NodeId>, parent_type: &str) -> Result<NodeId, ImportError> {
        let parts = Parts::split(value, parent_type)?;
        let id = parts.id.unwrap_or_default();
        if !self.seen.insert(id) {
            return Err(ImportError::DuplicateId(id));
        }
        let own_type = parts.content.node_type.clone();
        self.ops.push(OpBody::InsertNode { node: id, parent, after, content: parts.content });
        if parts.relation.is_some() {
            self.ops.push(OpBody::SetRelation { node: id, relation: parts.relation });
        }
        let mut previous = None;
        for child in parts.children {
            previous = Some(self.node(child, Parent::Node(id), previous, &own_type)?);
        }
        Ok(id)
    }
}

/// A MyST JSON node taken apart into what the ops need.
struct Parts {
    id: Option<NodeId>,
    content: NodeContent,
    relation: Option<Relation>,
    children: Vec<Value>,
}

impl Parts {
    fn split(value: Value, parent_type: &str) -> Result<Parts, ImportError> {
        let bad_children = || ImportError::BadChildren(parent_type.to_owned());
        let Value::Object(mut fields) = value else { return Err(bad_children()) };
        let node_type = match fields.remove("type") {
            Some(Value::String(t)) => t,
            _ => return Err(bad_children()),
        };
        let id = match fields.remove("id") {
            None => None,
            Some(Value::String(s)) => Some(s.parse().map_err(|_| ImportError::BadId(s))?),
            Some(other) => return Err(ImportError::BadId(other.to_string())),
        };
        let children = match fields.remove("children") {
            None => Vec::new(),
            Some(Value::Array(children)) => children,
            Some(_) => return Err(ImportError::BadChildren(node_type)),
        };
        let relation = match fields.remove("relation") {
            Some(Value::String(r)) if EDGE_TYPES.contains(&node_type.as_str()) => Some(Relation(r)),
            Some(other) => {
                fields.insert("relation".to_owned(), other);
                None
            }
            None => None,
        };
        Ok(Parts { id, content: NodeContent { node_type, fields }, relation, children })
    }
}

/// The ops that turn a document into an edited version of itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update {
    pub ops: Vec<OpBody>,
    /// `(old, new)`: IDs in the edited tree that could not be reused, because
    /// the node was deleted (or its type changed to or from a text run). The
    /// node was inserted under the new ID instead; the editor should rename
    /// it.
    pub renamed: Vec<(NodeId, NodeId)>,
}

/// Compares `root`, an edited native-MyST copy of document `doc`, with the
/// document in `state`, and returns the ops that make them the same.
///
/// Nodes match by `id`; every node except text runs must carry one, and an
/// unknown `id` is a new node. Text runs match the parent's old text runs of
/// the same type in order. Matched nodes that kept their relative order stay
/// put (the longest such run), the rest move; changed fields become
/// `set_attrs`, `edit_text` and `set_relation`, and a changed type becomes
/// `set_type`, so the node keeps its ID; nodes missing from `root` are
/// deleted. Applying the ops to `state` and materializing gives `root`
/// back.
pub fn update_document(state: &ProjectState, doc: DocumentId, root: &Node) -> Result<Update, ImportError> {
    state.document(doc).ok_or(ImportError::UnknownDocument(doc))?;
    let root = to_json(root);
    let mut claimed = HashSet::new();
    claim_ids(&root, &mut claimed)?;
    let mut r = Reconciler { state, claimed, matched: HashSet::new(), deleted: HashSet::new(), update: Update::default() };
    r.children(Parent::DocumentRoot(doc), vec![root], "document")?;

    // Delete what is no longer there, outermost first; a deleted node takes
    // its unmatched descendants with it.
    let mut stack = state.children(Parent::DocumentRoot(doc));
    while let Some(id) = stack.pop() {
        if !r.matched.contains(&id) {
            let parent_goes = matches!(state.node(id).expect("child").parent, Parent::Node(p) if !r.matched.contains(&p));
            if !parent_goes && !r.deleted.contains(&id) {
                r.update.ops.push(OpBody::DeleteNode { node: id });
            }
        }
        stack.extend(state.children(Parent::Node(id)));
    }
    Ok(r.update)
}

fn claim_ids(value: &Value, claimed: &mut HashSet<NodeId>) -> Result<(), ImportError> {
    let Value::Object(map) = value else { return Ok(()) };
    match map.get("id") {
        None => {}
        Some(Value::String(s)) => {
            let id = s.parse().map_err(|_| ImportError::BadId(s.clone()))?;
            if !claimed.insert(id) {
                return Err(ImportError::DuplicateId(id));
            }
        }
        Some(other) => return Err(ImportError::BadId(other.to_string())),
    }
    if let Some(Value::Array(children)) = map.get("children") {
        children.iter().try_for_each(|c| claim_ids(c, claimed))?;
    }
    Ok(())
}

enum Target {
    Existing(NodeId),
    New(NodeId),
}

struct Reconciler<'a> {
    state: &'a ProjectState,
    /// IDs the edited tree names.
    claimed: HashSet<NodeId>,
    /// Old nodes that are still in the tree.
    matched: HashSet<NodeId>,
    /// Old nodes already deleted (the children of a node that became a leaf).
    deleted: HashSet<NodeId>,
    update: Update,
}

impl Reconciler<'_> {
    /// Makes `parent`'s children `new`. Parents are placed before their
    /// children, so a move never goes into its own subtree.
    fn children(&mut self, parent: Parent, new: Vec<Value>, parent_type: &str) -> Result<(), ImportError> {
        let state = self.state;
        let old = state.children(parent);
        let type_of = |id: NodeId| state.node(id).expect("live").content.node_type.as_str();
        let mut anonymous: Vec<NodeId> = old
            .iter()
            .copied()
            .filter(|&id| TEXT_RUN_TYPES.contains(&type_of(id)) && !self.claimed.contains(&id))
            .collect();

        let mut resolved = Vec::with_capacity(new.len());
        for value in new {
            let parts = Parts::split(value, parent_type)?;
            let node_type = parts.content.node_type.as_str();
            let target = match parts.id {
                Some(id) => match state.node(id) {
                    None => Target::New(id),
                    Some(n)
                        if n.deleted
                            || (n.content.node_type != node_type && !retypable(&n.content.node_type, node_type))
                            || state.document(n.document).is_none_or(|d| d.deleted) =>
                    {
                        let fresh = NodeId::new();
                        self.update.renamed.push((id, fresh));
                        Target::New(fresh)
                    }
                    Some(_) => Target::Existing(id),
                },
                None if TEXT_RUN_TYPES.contains(&node_type) => {
                    match anonymous.iter().position(|&a| type_of(a) == node_type) {
                        Some(i) => Target::Existing(anonymous.remove(i)),
                        None => Target::New(NodeId::new()),
                    }
                }
                None => return Err(ImportError::MissingId(node_type.to_owned())),
            };
            resolved.push((parts, target));
        }

        let positions: Vec<Option<usize>> = resolved
            .iter()
            .map(|(_, target)| match target {
                Target::Existing(id) => old.iter().position(|o| o == id),
                Target::New(_) => None,
            })
            .collect();
        let stay = longest_increasing(&positions);

        let mut previous = None;
        for (i, (parts, target)) in resolved.into_iter().enumerate() {
            let own_type = parts.content.node_type.clone();
            let id = match target {
                Target::Existing(id) => {
                    if !stay.contains(&i) {
                        self.update.ops.push(OpBody::MoveNode { node: id, parent, after: previous });
                    }
                    self.content(id, &parts)?;
                    self.matched.insert(id);
                    id
                }
                Target::New(id) => {
                    self.update.ops.push(OpBody::InsertNode { node: id, parent, after: previous, content: parts.content });
                    if parts.relation.is_some() {
                        self.update.ops.push(OpBody::SetRelation { node: id, relation: parts.relation });
                    }
                    id
                }
            };
            self.children(Parent::Node(id), parts.children, &own_type)?;
            previous = Some(id);
        }
        Ok(())
    }

    /// The ops that give existing node `id` the type and fields of `parts`.
    fn content(&mut self, id: NodeId, parts: &Parts) -> Result<(), ImportError> {
        let n = self.state.node(id).expect("matched nodes exist");
        let (old, new) = (&n.content.fields, &parts.content.fields);
        let new_type = &parts.content.node_type;
        if *new_type != n.content.node_type {
            return Ok(self.retype(id, parts));
        }
        let set: Map<String, Value> = new
            .iter()
            .filter(|&(k, v)| k != "value" && old.get(k) != Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let unset: Vec<String> = old.keys().filter(|k| *k != "value" && !new.contains_key(*k)).cloned().collect();
        if !set.is_empty() || !unset.is_empty() {
            self.update.ops.push(OpBody::SetAttrs { node: id, set, unset });
        }
        match (old.get("value"), new.get("value")) {
            (old, Some(Value::String(value))) if old != new.get("value") => {
                self.update.ops.push(OpBody::EditText { node: id, edit: TextEdit::Replace { value: value.clone() } });
            }
            (old, new) if old != new => return Err(ImportError::BadValue(n.content.node_type.clone())),
            _ => {}
        }
        if n.is_edge() && n.relation != parts.relation {
            self.update.ops.push(OpBody::SetRelation { node: id, relation: parts.relation.clone() });
        }
        Ok(())
    }

    /// `set_type` for a node whose type changed, with what it needs around
    /// it: a relation the new type can't carry is cleared first, and the
    /// children of a node becoming a leaf (paragraph → code) are deleted
    /// first.
    fn retype(&mut self, id: NodeId, parts: &Parts) {
        let n = self.state.node(id).expect("matched nodes exist");
        let (old, new) = (&n.content.fields, &parts.content.fields);
        let new_type = &parts.content.node_type;
        let new_is_edge = EDGE_TYPES.contains(&new_type.as_str());
        if n.relation.is_some() && !new_is_edge {
            self.update.ops.push(OpBody::SetRelation { node: id, relation: None });
        }
        if children_policy(new_type) == ChildrenPolicy::Never {
            for child in self.state.children(Parent::Node(id)) {
                self.update.ops.push(OpBody::DeleteNode { node: child });
                self.deleted.insert(child);
            }
        }
        let set = new.iter().filter(|&(k, v)| old.get(k) != Some(v)).map(|(k, v)| (k.clone(), v.clone())).collect();
        let unset = old.keys().filter(|k| !new.contains_key(*k)).cloned().collect();
        self.update.ops.push(OpBody::SetType { node: id, node_type: new_type.clone(), set, unset });
        let relation = if n.relation.is_some() && new_is_edge { n.relation.clone() } else { None };
        if new_is_edge && relation != parts.relation {
            self.update.ops.push(OpBody::SetRelation { node: id, relation: parts.relation.clone() });
        }
    }
}

/// Whether a node can change from `old` to `new` type in place: anything
/// but a text run, which has no identity to keep.
fn retypable(old: &str, new: &str) -> bool {
    !TEXT_RUN_TYPES.contains(&old) && !TEXT_RUN_TYPES.contains(&new)
}

/// Indices of a longest strictly increasing subsequence of the `Some`
/// entries: the children that can stay where they are.
fn longest_increasing(positions: &[Option<usize>]) -> HashSet<usize> {
    // tails[k]: index (into `positions`) ending the best run of length k + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; positions.len()];
    for (i, p) in positions.iter().enumerate() {
        let Some(p) = *p else { continue };
        let k = tails.partition_point(|&t| positions[t].expect("tails are Some") < p);
        back[i] = k.checked_sub(1).map(|k| tails[k]);
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut stay = HashSet::new();
    let mut cursor = tails.last().copied();
    while let Some(i) = cursor {
        stay.insert(i);
        cursor = back[i];
    }
    stay
}

/// MyST types that are inline content (phrasing): text runs plus inline
/// anchors and references.
const PHRASING_TYPES: [&str; 6] = ["link", "crossReference", "cite", "citeGroup", "footnoteReference", "mystRole"];

/// The block `node` sits in: itself, or its nearest ancestor that is not
/// phrasing. A search hit in a text run, or a link, resolves to its
/// paragraph, heading, ... which has a stable, exported ID.
pub fn enclosing_block(state: &ProjectState, node: NodeId) -> Option<NodeId> {
    let mut id = node;
    loop {
        let n = state.node(id)?;
        let t = n.content.node_type.as_str();
        if !TEXT_RUN_TYPES.contains(&t) && !PHRASING_TYPES.contains(&t) {
            return Some(id);
        }
        match n.parent {
            Parent::Node(parent) => id = parent,
            Parent::DocumentRoot(_) => return None,
        }
    }
}

/// The text of `node` and its live descendants, concatenated.
pub fn plain_text(state: &ProjectState, node: NodeId) -> String {
    fn walk(state: &ProjectState, id: NodeId, out: &mut String) {
        let Some(n) = state.node(id) else { return };
        if let Some(text) = n.text() {
            out.push_str(text);
        }
        for child in state.children(Parent::Node(id)) {
            walk(state, child, out);
        }
    }
    let mut out = String::new();
    walk(state, node, &mut out);
    out
}

fn to_json(node: &Node) -> Value {
    serde_json::to_value(node).expect("MyST nodes serialize")
}

/// Document `doc` as a MyST `root`, with its live nodes in order. Deleted
/// documents materialize too: they are tombstones, not gone.
pub fn materialize(state: &ProjectState, doc: DocumentId, flavor: Flavor) -> Result<Node, MaterializeError> {
    state.document(doc).ok_or(MaterializeError::UnknownDocument(doc))?;
    let top = state.children(Parent::DocumentRoot(doc));
    let value = match top.as_slice() {
        [only] if state.node(*only).is_some_and(|n| n.content.node_type == "root") => node_json(state, *only, flavor),
        _ => {
            let mut root = Map::new();
            root.insert("type".to_owned(), Value::String("root".to_owned()));
            root.insert("children".to_owned(), top.iter().map(|&id| node_json(state, id, flavor)).collect());
            Value::Object(root)
        }
    };
    serde_json::from_value(value).map_err(|e| MaterializeError::Invalid(doc, e.to_string()))
}

fn node_json(state: &ProjectState, id: NodeId, flavor: Flavor) -> Value {
    let n = state.node(id).expect("children exist");
    let node_type = n.content.node_type.as_str();
    let mut map = n.content.to_json();
    if flavor == Flavor::Native {
        if !TEXT_RUN_TYPES.contains(&node_type) {
            map.insert("id".to_owned(), Value::String(id.to_string()));
        }
        if let Some(relation) = &n.relation {
            map.insert("relation".to_owned(), Value::String(relation.0.clone()));
        }
    }
    let children = state.children(Parent::Node(id));
    if !children.is_empty() || children_policy(node_type) == ChildrenPolicy::Always {
        map.insert("children".to_owned(), children.iter().map(|&c| node_json(state, c, flavor)).collect());
    }
    Value::Object(map)
}
