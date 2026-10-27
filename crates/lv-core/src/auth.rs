//! The single permission check (invariant 3).
//!
//! Every op passes through [`authorize`] before it is sequenced. Scoped
//! permissions will be added here, and only here.
//!
//! Current policy:
//! - The op's grant must exist and be held by the op's principal.
//! - A person may do anything.
//! - An agent works in change sets: every op it submits must belong to one,
//!   except accepting or rejecting a change set.
//! - An agent may reject (withdraw) its own change sets, and may accept them
//!   only if its grant has `auto_accept` on. It may never accept or reject
//!   anyone else's.
//!
//! Auto-accepted work stays attributable: the accept op carries the agent as
//! its principal, so the log shows the agent approved its own change set.

use crate::ids::ChangeSetId;
use crate::op::{Op, OpBody};
use crate::principal::{Grant, Principal};
use crate::refusal::{Refusal, RefusalCode};
use crate::state::ProjectState;

/// Decides whether `op` may be applied to `state`. `grant` is the grant the
/// op names, looked up by the caller (`None` if no such grant is registered).
pub fn authorize(op: &Op, grant: Option<&Grant>, state: &ProjectState) -> Result<(), Refusal> {
    let Some(grant) = grant else {
        return Err(forbidden(format!("unknown grant {}", op.grant)));
    };
    if grant.holder != op.principal {
        return Err(forbidden(format!("grant {} is not held by this principal", grant.id)));
    }
    if let Principal::Person { .. } = op.principal {
        return Ok(());
    }

    match &op.body {
        OpBody::AcceptChangeSet { change_set } => {
            own_change_set(state, *change_set, &op.principal)?;
            if !grant.auto_accept {
                return Err(forbidden(format!(
                    "{change_set} needs the person's approval: auto-accept is off for grant {}",
                    grant.id
                )));
            }
            Ok(())
        }
        OpBody::RejectChangeSet { change_set, .. } => own_change_set(state, *change_set, &op.principal),
        _ if op.change_set.is_none() => Err(forbidden("agent edits must belong to a change set".into())),
        _ => Ok(()),
    }
}

/// An agent may only decide on change sets it authored. Unknown change sets
/// pass here so that applying the op reports `UNKNOWN_CHANGE_SET`.
fn own_change_set(state: &ProjectState, id: ChangeSetId, principal: &Principal) -> Result<(), Refusal> {
    match state.change_set(id) {
        Some(cs) if cs.author != *principal => Err(forbidden(format!("{id} was proposed by someone else"))),
        _ => Ok(()),
    }
}

fn forbidden(message: String) -> Refusal {
    Refusal::new(RefusalCode::Forbidden, message)
}
