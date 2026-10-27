//! Who did something, and on whose authority (invariant 1).

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, GrantId, PersonId};

/// The actor behind an op: a person, or an agent acting for one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    Person { person: PersonId },
    Agent { agent: AgentId, on_behalf_of: PersonId },
}

impl Principal {
    /// The person ultimately responsible: the actor, or whoever the agent acts for.
    pub fn person(&self) -> PersonId {
        match *self {
            Principal::Person { person } => person,
            Principal::Agent { on_behalf_of, .. } => on_behalf_of,
        }
    }

    pub fn agent(&self) -> Option<AgentId> {
        match *self {
            Principal::Person { .. } => None,
            Principal::Agent { agent, .. } => Some(agent),
        }
    }
}

/// What a grant allows. v1 has two shapes; finer scopes come with sharing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    /// Full control of the project.
    Owner,
    /// Delegated to an agent by the person it acts for.
    Delegated,
}

impl GrantScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            GrantScope::Owner => "owner",
            GrantScope::Delegated => "delegated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(GrantScope::Owner),
            "delegated" => Some(GrantScope::Delegated),
            _ => None,
        }
    }
}

/// An authorization held by a principal. Every op names the grant it used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub holder: Principal,
    pub scope: GrantScope,
    /// For agent grants: the agent may accept its own change sets without
    /// the person's approval ("auto mode"). Ignored for person grants.
    #[serde(default)]
    pub auto_accept: bool,
}

impl Grant {
    /// A grant with auto-accept off.
    pub fn new(holder: Principal, scope: GrantScope) -> Grant {
        Grant { id: GrantId::new(), holder, scope, auto_accept: false }
    }
}
