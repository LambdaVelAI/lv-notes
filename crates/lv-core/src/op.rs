//! Ops: the only way project state changes.
//!
//! Ops address nodes by ID, never by path or offset (invariant 4), carry the
//! principal and grant behind them (invariant 1), belong to one project
//! (invariant 2), record the version they were made against (invariant 5),
//! and may belong to a change set (invariant 6).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{ChangeSetId, DocumentId, GrantId, NodeId, OpId, ProjectId, Version};
use crate::myst::Node;
use crate::principal::Principal;
use crate::schema::{KindEntry, Namespace};

/// Milliseconds since the Unix epoch, from the submitting client's clock.
/// Informational only: ordering comes from [`Version`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Timestamp(ms)
    }
}

/// An op as submitted by a client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub id: OpId,
    pub project: ProjectId,
    pub principal: Principal,
    pub grant: GrantId,
    /// The project version the client saw when making this op.
    pub base: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set: Option<ChangeSetId>,
    pub created_at: Timestamp,
    pub body: OpBody,
}

/// An op accepted by the sequencer, at its position in the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequenced {
    pub seq: Version,
    pub op: Op,
}

/// A document kind, declared in the project's registry (`note`, `draft`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocKind(pub String);

impl DocKind {
    pub fn new(name: impl Into<String>) -> DocKind {
        DocKind(name.into())
    }

    pub fn note() -> DocKind {
        DocKind::new("note")
    }

    pub fn draft() -> DocKind {
        DocKind::new("draft")
    }
}

/// Where a node sits in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Parent {
    DocumentRoot(DocumentId),
    Node(NodeId),
}

/// A node's own fields, without children or ID: its MyST `type` plus
/// attributes. Children are inserted by their own ops, so every node has an
/// ID-addressed history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeContent {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl NodeContent {
    /// The content of `node` itself, dropping `children` and `id`.
    pub fn from_node(node: &Node) -> NodeContent {
        let Ok(Value::Object(mut fields)) = serde_json::to_value(node) else {
            unreachable!("MyST nodes always serialize to objects")
        };
        fields.remove("children");
        fields.remove("id");
        let node_type = match fields.remove("type") {
            Some(Value::String(t)) => t,
            _ => unreachable!("MyST nodes always serialize a string `type`"),
        };
        NodeContent { node_type, fields }
    }

    /// The node as a MyST JSON object (`type` plus fields), without children.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::with_capacity(self.fields.len() + 1);
        map.insert("type".to_owned(), Value::String(self.node_type.clone()));
        map.extend(self.fields.iter().map(|(k, v)| (k.clone(), v.clone())));
        map
    }

    /// Whether this content reads as a MyST node of its type, e.g. a
    /// `heading` whose `depth` is a number.
    pub fn check(&self) -> Result<(), String> {
        serde_json::from_value::<Node>(Value::Object(self.to_json()))
            .map(drop)
            .map_err(|e| format!("not a valid `{}` node: {e}", self.node_type))
    }
}

/// An edit to the text value of one node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextEdit {
    /// Replace the node's whole text. A v1 placeholder until the text CRDT
    /// is chosen; CRDT updates will be added as further variants.
    Replace { value: String },
}

/// A relation type on a link, from an extensible vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Relation(pub String);

impl Relation {
    pub const CITES: &'static str = "cites";
    pub const SUPPORTS: &'static str = "supports";
    pub const CONTRADICTS: &'static str = "contradicts";

    pub fn new(name: impl Into<String>) -> Self {
        Relation(name.into())
    }
}

/// What an op does.
///
/// Sibling order is given by `after`, the node's left neighbour (`None` =
/// first child), never an index. If the anchor has since been deleted, the
/// node goes after the anchor's nearest surviving left sibling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpBody {
    CreateDocument { doc: DocumentId, kind: DocKind, title: String },
    SetDocumentTitle { doc: DocumentId, title: String },
    /// Tombstones the document; it stays restorable.
    DeleteDocument { doc: DocumentId },

    InsertNode { node: NodeId, parent: Parent, after: Option<NodeId>, content: NodeContent },
    /// Moves a node and its subtree, also across documents in the project.
    MoveNode { node: NodeId, parent: Parent, after: Option<NodeId> },
    /// Tombstones the node and its subtree.
    DeleteNode { node: NodeId },

    SetAttrs {
        node: NodeId,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        set: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unset: Vec<String>,
    },
    EditText { node: NodeId, edit: TextEdit },
    /// Changes the node's MyST type in place (paragraph → heading, ...),
    /// keeping its ID and so everything that points at it. `set` and
    /// `unset` adjust its fields in the same step, since the new type may
    /// need different ones; they may include `value` (paragraph → code).
    SetType {
        node: NodeId,
        node_type: String,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        set: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unset: Vec<String>,
    },

    /// Types (or untypes) the link, citation or reference node `node`.
    SetRelation { node: NodeId, relation: Option<Relation> },

    /// Adds `name` to the project's vocabulary (see `schema`).
    DeclareKind { namespace: Namespace, name: String, entry: KindEntry },
    /// Replaces an entry: its label, status (e.g. retiring it) or fields.
    UpdateKind { namespace: Namespace, name: String, entry: KindEntry },
    /// Renames `from` to `to`, rewriting every use in the project.
    RenameKind { namespace: Namespace, from: String, to: String },
    /// Folds `from` into `into`: every use of `from` becomes `into`, and
    /// `from` leaves the registry.
    MergeKind { namespace: Namespace, from: String, into: String },

    OpenChangeSet { change_set: ChangeSetId, rationale: String },
    AcceptChangeSet { change_set: ChangeSetId },
    RejectChangeSet {
        change_set: ChangeSetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl OpBody {
    /// The op's name as stored in the log, e.g. `insert_node`.
    pub fn kind(&self) -> &'static str {
        match self {
            OpBody::CreateDocument { .. } => "create_document",
            OpBody::SetDocumentTitle { .. } => "set_document_title",
            OpBody::DeleteDocument { .. } => "delete_document",
            OpBody::InsertNode { .. } => "insert_node",
            OpBody::MoveNode { .. } => "move_node",
            OpBody::DeleteNode { .. } => "delete_node",
            OpBody::SetAttrs { .. } => "set_attrs",
            OpBody::EditText { .. } => "edit_text",
            OpBody::SetType { .. } => "set_type",
            OpBody::SetRelation { .. } => "set_relation",
            OpBody::DeclareKind { .. } => "declare_kind",
            OpBody::UpdateKind { .. } => "update_kind",
            OpBody::RenameKind { .. } => "rename_kind",
            OpBody::MergeKind { .. } => "merge_kind",
            OpBody::OpenChangeSet { .. } => "open_change_set",
            OpBody::AcceptChangeSet { .. } => "accept_change_set",
            OpBody::RejectChangeSet { .. } => "reject_change_set",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::PersonId;

    #[test]
    fn kind_matches_serde_tag() {
        let bodies = [
            OpBody::DeleteNode { node: NodeId::new() },
            OpBody::SetType { node: NodeId::new(), node_type: "heading".into(), set: Map::new(), unset: vec![] },
            OpBody::SetAttrs { node: NodeId::new(), set: Map::new(), unset: vec!["lang".into()] },
            OpBody::RejectChangeSet { change_set: ChangeSetId::new(), reason: None },
            OpBody::MergeKind { namespace: Namespace::Relation, from: "backs".into(), into: "supports".into() },
        ];
        for body in bodies {
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(json["op"], body.kind());
        }
    }

    #[test]
    fn op_json_roundtrips() {
        let op = Op {
            id: OpId::new(),
            project: ProjectId::new(),
            principal: Principal::Person { person: PersonId::new() },
            grant: GrantId::new(),
            base: Version(4),
            change_set: None,
            created_at: Timestamp(1_761_560_000_000),
            body: OpBody::InsertNode {
                node: NodeId::new(),
                parent: Parent::DocumentRoot(DocumentId::new()),
                after: None,
                content: NodeContent { node_type: "paragraph".into(), fields: Map::new() },
            },
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
    }

    #[test]
    fn node_content_drops_children_and_id() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "type": "heading",
            "id": NodeId::new(),
            "depth": 2,
            "label": "intro",
            "children": [{ "type": "text", "value": "Intro" }]
        }))
        .unwrap();
        let content = NodeContent::from_node(&node);
        assert_eq!(content.node_type, "heading");
        assert_eq!(
            Value::Object(content.fields),
            serde_json::json!({ "depth": 2, "label": "intro" })
        );
    }
}
