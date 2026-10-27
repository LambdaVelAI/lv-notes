//! Why the sequencer turned an op down.
//!
//! Every refusal carries a stable code. The agent `guide` explains each code
//! and how to fix the call, so codes are part of the public contract: add new
//! ones freely, but never rename or reuse one.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::OpId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RefusalCode {
    /// The op names a different project than this sequencer's.
    WrongProject,
    /// The op's base version is ahead of the project's head.
    BaseInFuture,
    /// An op with this ID was already accepted, or appears twice in the batch.
    DuplicateOp,
    /// The permission check refused the op.
    Forbidden,
    /// The batch is empty, or the op is malformed (see the message).
    InvalidOp,
    UnknownDocument,
    DocumentExists,
    DocumentDeleted,
    UnknownNode,
    NodeExists,
    NodeDeleted,
    /// A move would put a node inside its own subtree.
    Cycle,
    /// Someone else replaced this node's text after the op's base version.
    StaleText,
    UnknownChangeSet,
    ChangeSetExists,
    /// The change set was already accepted or rejected.
    ChangeSetClosed,
    /// The change set no longer applies on top of the current project.
    ChangeSetConflict,
}

impl RefusalCode {
    /// The code as it appears on the wire, e.g. `NODE_DELETED`.
    pub fn as_str(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: RefusalCode,
    /// The op that was refused, when the refusal concerns one op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<OpId>,
    pub message: String,
}

impl Refusal {
    pub fn new(code: RefusalCode, message: impl Into<String>) -> Refusal {
        Refusal { code, op: None, message: message.into() }
    }

    pub fn for_op(mut self, op: OpId) -> Refusal {
        self.op.get_or_insert(op);
        self
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for Refusal {}
