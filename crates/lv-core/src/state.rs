//! The project as of some version: what the op log means.
//!
//! [`ProjectState::apply`] is the one definition of op semantics. The
//! sequencer uses it to validate ops against the head, and replaying a log
//! through it rebuilds the project. It is pure and deterministic: the same
//! log always gives the same state.
//!
//! Semantics worth knowing:
//! - Deleted documents and nodes are tombstones. A deleted node keeps its
//!   place among its siblings, so an op that inserts "after" it lands right
//!   after its nearest surviving left sibling.
//! - An `after` anchor that is not (or no longer) a child of the target
//!   parent, e.g. because it was moved, means "first child".
//! - Of two inserts after the same anchor, the later-sequenced one comes first.
//! - Deleting something already deleted is accepted and changes nothing.
//! - Ops in an open change set are checked against the project with that
//!   change set applied, but only reach the project when it is accepted.
//!   Acceptance replays them on the current project; if any no longer
//!   applies, the accept is refused.

use std::collections::HashMap;

use serde_json::Value;

use crate::ids::{ChangeSetId, DocumentId, NodeId, Version};
use crate::op::{DocKind, NodeContent, OpBody, Parent, Relation, Sequenced, TextEdit};
use crate::principal::Principal;
use crate::refusal::{Refusal, RefusalCode as Code};

/// Attribute keys `SetAttrs` may not touch: they have dedicated ops or are structural.
const RESERVED_ATTRS: [&str; 4] = ["type", "id", "children", "value"];
/// Node types that are graph edges and can carry a relation.
const EDGE_TYPES: [&str; 3] = ["link", "crossReference", "cite"];

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentState {
    pub kind: DocKind,
    pub title: String,
    pub deleted: bool,
    /// Top-level nodes in order, tombstones included.
    children: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeState {
    pub document: DocumentId,
    pub parent: Parent,
    /// The node's MyST type and attributes; its text is `content.fields["value"]`.
    pub content: NodeContent,
    pub relation: Option<Relation>,
    pub deleted: bool,
    /// Children in order, tombstones included.
    children: Vec<NodeId>,
    /// The last whole-text replacement: when, and by whom.
    last_text_edit: Option<(Version, Principal)>,
}

impl NodeState {
    pub fn text(&self) -> Option<&str> {
        self.content.fields.get("value").and_then(Value::as_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSetStatus {
    Open,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChangeSetState {
    pub author: Principal,
    pub base: Version,
    pub rationale: String,
    pub status: ChangeSetStatus,
    /// The proposed ops, in log order (not including `OpenChangeSet`).
    pub ops: Vec<Sequenced>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectState {
    documents: HashMap<DocumentId, DocumentState>,
    nodes: HashMap<NodeId, NodeState>,
    change_sets: HashMap<ChangeSetId, ChangeSetState>,
}

impl ProjectState {
    pub fn new() -> ProjectState {
        ProjectState::default()
    }

    pub fn document(&self, id: DocumentId) -> Option<&DocumentState> {
        self.documents.get(&id)
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeState> {
        self.nodes.get(&id)
    }

    pub fn change_set(&self, id: ChangeSetId) -> Option<&ChangeSetState> {
        self.change_sets.get(&id)
    }

    /// Live (not deleted) children of `parent`, in order.
    pub fn children(&self, parent: Parent) -> Vec<NodeId> {
        self.siblings(parent)
            .iter()
            .copied()
            .filter(|id| self.nodes.get(id).is_some_and(|n| !n.deleted))
            .collect()
    }

    /// The project with open change set `id` applied, as its author sees it.
    pub fn with_change_set(&self, id: ChangeSetId) -> Result<ProjectState, Refusal> {
        let cs = self.open_change_set(id)?;
        let mut view = self.clone();
        for s in &cs.ops {
            view.apply_now(s).map_err(|r| conflict(id, s, r))?;
        }
        Ok(view)
    }

    /// Applies an accepted op. On refusal the state is unchanged.
    pub fn apply(&mut self, s: &Sequenced) -> Result<(), Refusal> {
        let result = match (s.op.change_set, &s.op.body) {
            (Some(_), OpBody::OpenChangeSet { .. }) | (None, _) => self.apply_now(s),
            (Some(id), _) => {
                // Checked against the change set's own view; recorded, not applied.
                self.with_change_set(id)?.apply_now(s)?;
                self.change_sets.get_mut(&id).expect("checked open").ops.push(s.clone());
                Ok(())
            }
        };
        result.map_err(|r| r.for_op(s.op.id))
    }

    /// Applies `s` to this state directly, ignoring its change set.
    fn apply_now(&mut self, s: &Sequenced) -> Result<(), Refusal> {
        let op = &s.op;
        match &op.body {
            OpBody::CreateDocument { doc, kind, title } => {
                if self.documents.contains_key(doc) {
                    return Err(Refusal::new(Code::DocumentExists, format!("{doc} already exists")));
                }
                self.documents.insert(
                    *doc,
                    DocumentState { kind: *kind, title: title.clone(), deleted: false, children: Vec::new() },
                );
            }
            OpBody::SetDocumentTitle { doc, title } => {
                self.live_document(*doc)?;
                self.documents.get_mut(doc).expect("live").title = title.clone();
            }
            OpBody::DeleteDocument { doc } => {
                let d = self.documents.get_mut(doc).ok_or_else(|| unknown_document(*doc))?;
                d.deleted = true;
            }

            OpBody::InsertNode { node, parent, after, content } => {
                if self.nodes.contains_key(node) {
                    return Err(Refusal::new(Code::NodeExists, format!("{node} already exists")));
                }
                check_content(content)?;
                let document = self.live_parent(*parent)?;
                self.known_anchor(*after)?;
                self.nodes.insert(
                    *node,
                    NodeState {
                        document,
                        parent: *parent,
                        content: content.clone(),
                        relation: None,
                        deleted: false,
                        children: Vec::new(),
                        last_text_edit: None,
                    },
                );
                self.place(*node, *parent, *after);
            }
            OpBody::MoveNode { node, parent, after } => {
                self.live_node(*node)?;
                let document = self.live_parent(*parent)?;
                self.known_anchor(*after)?;
                if *after == Some(*node) {
                    return Err(Refusal::new(Code::InvalidOp, "a node cannot be placed after itself"));
                }
                if self.is_in_subtree(*parent, *node) {
                    return Err(Refusal::new(Code::Cycle, format!("{node} cannot move into its own subtree")));
                }
                let old = self.nodes[node].parent;
                self.siblings_mut(old).retain(|id| id != node);
                self.nodes.get_mut(node).expect("live").parent = *parent;
                self.place(*node, *parent, *after);
                for id in self.subtree(*node) {
                    self.nodes.get_mut(&id).expect("in subtree").document = document;
                }
            }
            OpBody::DeleteNode { node } => {
                let n = self.nodes.get(node).ok_or_else(|| unknown_node(*node))?;
                if !n.deleted {
                    for id in self.subtree(*node) {
                        self.nodes.get_mut(&id).expect("in subtree").deleted = true;
                    }
                }
            }

            OpBody::SetAttrs { node, set, unset } => {
                self.live_node(*node)?;
                if let Some(key) = set.keys().chain(unset).find(|k| RESERVED_ATTRS.contains(&k.as_str())) {
                    return Err(Refusal::new(Code::InvalidOp, format!("`{key}` cannot be changed with set_attrs")));
                }
                let fields = &mut self.nodes.get_mut(node).expect("live").content.fields;
                for key in unset {
                    fields.remove(key);
                }
                fields.extend(set.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            OpBody::EditText { node, edit } => {
                let n = self.live_node(*node)?;
                if let Some((seq, by)) = n.last_text_edit
                    && seq > op.base
                    && by != op.principal
                {
                    return Err(Refusal::new(
                        Code::StaleText,
                        format!("{node} text was replaced at {seq}, after base {}", op.base),
                    ));
                }
                let n = self.nodes.get_mut(node).expect("live");
                match edit {
                    TextEdit::Replace { value } => {
                        n.content.fields.insert("value".to_owned(), Value::String(value.clone()));
                    }
                }
                n.last_text_edit = Some((s.seq, op.principal));
            }

            OpBody::SetRelation { node, relation } => {
                let n = self.live_node(*node)?;
                if !EDGE_TYPES.contains(&n.content.node_type.as_str()) {
                    return Err(Refusal::new(
                        Code::InvalidOp,
                        format!("{node} is a `{}`; only links, citations and references take a relation", n.content.node_type),
                    ));
                }
                self.nodes.get_mut(node).expect("live").relation = relation.clone();
            }

            OpBody::OpenChangeSet { change_set, rationale } => {
                if op.change_set != Some(*change_set) {
                    return Err(Refusal::new(Code::InvalidOp, "open_change_set must belong to the change set it opens"));
                }
                if self.change_sets.contains_key(change_set) {
                    return Err(Refusal::new(Code::ChangeSetExists, format!("{change_set} already exists")));
                }
                self.change_sets.insert(
                    *change_set,
                    ChangeSetState {
                        author: op.principal,
                        base: op.base,
                        rationale: rationale.clone(),
                        status: ChangeSetStatus::Open,
                        ops: Vec::new(),
                    },
                );
            }
            OpBody::AcceptChangeSet { change_set } => {
                not_in_change_set(op.change_set)?;
                let mut next = self.with_change_set(*change_set)?;
                next.change_sets.get_mut(change_set).expect("open").status = ChangeSetStatus::Accepted;
                *self = next;
            }
            OpBody::RejectChangeSet { change_set, .. } => {
                not_in_change_set(op.change_set)?;
                self.open_change_set(*change_set)?;
                self.change_sets.get_mut(change_set).expect("open").status = ChangeSetStatus::Rejected;
            }
        }
        Ok(())
    }

    // ---- Checks --------------------------------------------------------

    fn live_document(&self, id: DocumentId) -> Result<&DocumentState, Refusal> {
        let d = self.documents.get(&id).ok_or_else(|| unknown_document(id))?;
        if d.deleted {
            return Err(Refusal::new(Code::DocumentDeleted, format!("{id} is deleted")));
        }
        Ok(d)
    }

    fn live_node(&self, id: NodeId) -> Result<&NodeState, Refusal> {
        let n = self.nodes.get(&id).ok_or_else(|| unknown_node(id))?;
        if n.deleted {
            return Err(Refusal::new(Code::NodeDeleted, format!("{id} is deleted")));
        }
        self.live_document(n.document)?;
        Ok(n)
    }

    /// Checks `parent` can take children; returns the document it is in.
    fn live_parent(&self, parent: Parent) -> Result<DocumentId, Refusal> {
        match parent {
            Parent::DocumentRoot(doc) => self.live_document(doc).map(|_| doc),
            Parent::Node(node) => self.live_node(node).map(|n| n.document),
        }
    }

    /// An anchor must have existed, but may since be deleted or moved.
    fn known_anchor(&self, after: Option<NodeId>) -> Result<(), Refusal> {
        match after {
            Some(id) if !self.nodes.contains_key(&id) => Err(unknown_node(id)),
            _ => Ok(()),
        }
    }

    fn open_change_set(&self, id: ChangeSetId) -> Result<&ChangeSetState, Refusal> {
        let cs = self
            .change_sets
            .get(&id)
            .ok_or_else(|| Refusal::new(Code::UnknownChangeSet, format!("{id} does not exist")))?;
        if cs.status != ChangeSetStatus::Open {
            return Err(Refusal::new(Code::ChangeSetClosed, format!("{id} is {:?}", cs.status).to_lowercase()));
        }
        Ok(cs)
    }

    /// Whether `parent` is `node` or lies inside its subtree.
    fn is_in_subtree(&self, parent: Parent, node: NodeId) -> bool {
        let mut current = parent;
        while let Parent::Node(id) = current {
            if id == node {
                return true;
            }
            current = self.nodes[&id].parent;
        }
        false
    }

    // ---- Tree mechanics --------------------------------------------------

    fn siblings(&self, parent: Parent) -> &[NodeId] {
        match parent {
            Parent::DocumentRoot(doc) => self.documents.get(&doc).map_or(&[], |d| &d.children),
            Parent::Node(id) => self.nodes.get(&id).map_or(&[], |n| &n.children),
        }
    }

    fn siblings_mut(&mut self, parent: Parent) -> &mut Vec<NodeId> {
        match parent {
            Parent::DocumentRoot(doc) => &mut self.documents.get_mut(&doc).expect("checked").children,
            Parent::Node(id) => &mut self.nodes.get_mut(&id).expect("checked").children,
        }
    }

    fn place(&mut self, node: NodeId, parent: Parent, after: Option<NodeId>) {
        let siblings = self.siblings_mut(parent);
        let index = after
            .and_then(|a| siblings.iter().position(|&id| id == a))
            .map_or(0, |i| i + 1);
        siblings.insert(index, node);
    }

    /// `node` and all its descendants.
    fn subtree(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(&self.nodes[&id].children);
        }
        out
    }
}

fn check_content(content: &NodeContent) -> Result<(), Refusal> {
    if content.node_type.is_empty() {
        return Err(Refusal::new(Code::InvalidOp, "node content needs a `type`"));
    }
    if let Some(key) = ["id", "children"].into_iter().find(|k| content.fields.contains_key(*k)) {
        return Err(Refusal::new(Code::InvalidOp, format!("node content may not contain `{key}`")));
    }
    Ok(())
}

fn not_in_change_set(change_set: Option<ChangeSetId>) -> Result<(), Refusal> {
    match change_set {
        Some(_) => Err(Refusal::new(Code::InvalidOp, "accepting or rejecting a change set cannot itself be part of one")),
        None => Ok(()),
    }
}

fn conflict(id: ChangeSetId, s: &Sequenced, cause: Refusal) -> Refusal {
    Refusal::new(
        Code::ChangeSetConflict,
        format!("{id} no longer applies: its op at {} was refused ({cause})", s.seq),
    )
}

fn unknown_document(id: DocumentId) -> Refusal {
    Refusal::new(Code::UnknownDocument, format!("{id} does not exist"))
}

fn unknown_node(id: NodeId) -> Refusal {
    Refusal::new(Code::UnknownNode, format!("{id} does not exist"))
}
