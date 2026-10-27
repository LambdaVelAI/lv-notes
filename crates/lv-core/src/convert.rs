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
//! Exported MyST comes in two flavors. [`Flavor::Plain`] is standard MyST.
//! [`Flavor::Native`] adds the lv-notes fields: `id` on every node except
//! text runs ([`TEXT_RUN_TYPES`]), and `relation` on typed links. Importing
//! reads both fields back, so a native export re-imports with the same IDs
//! and relations.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::ids::{DocumentId, NodeId};
use crate::myst::{ChildrenPolicy, Node, TEXT_RUN_TYPES, children_policy};
use crate::op::{DocKind, NodeContent, OpBody, Parent, Relation};
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
        let bad_children = || ImportError::BadChildren(parent_type.to_owned());
        let Value::Object(mut fields) = value else { return Err(bad_children()) };
        let node_type = match fields.remove("type") {
            Some(Value::String(t)) => t,
            _ => return Err(bad_children()),
        };
        let id = match fields.remove("id") {
            None => NodeId::new(),
            Some(Value::String(s)) => s.parse().map_err(|_| ImportError::BadId(s))?,
            Some(other) => return Err(ImportError::BadId(other.to_string())),
        };
        if !self.seen.insert(id) {
            return Err(ImportError::DuplicateId(id));
        }
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

        let own_type = node_type.clone();
        self.ops.push(OpBody::InsertNode { node: id, parent, after, content: NodeContent { node_type, fields } });
        if relation.is_some() {
            self.ops.push(OpBody::SetRelation { node: id, relation });
        }
        let mut previous = None;
        for child in children {
            previous = Some(self.node(child, Parent::Node(id), previous, &own_type)?);
        }
        Ok(id)
    }
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
