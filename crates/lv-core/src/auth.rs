//! The single permission check (invariant 3).
//!
//! Every op passes through [`authorize`] before it is sequenced. Today it
//! only checks that the op's grant exists and belongs to its principal; any
//! grant then allows any op. Scoped permissions will be added here, and only
//! here.

use crate::op::Op;
use crate::principal::Grant;
use crate::refusal::{Refusal, RefusalCode};

/// Decides whether `op` may be applied. `grant` is the grant the op names,
/// looked up by the caller (`None` if no such grant is registered).
pub fn authorize(op: &Op, grant: Option<&Grant>) -> Result<(), Refusal> {
    let Some(grant) = grant else {
        return Err(Refusal::new(RefusalCode::Forbidden, format!("unknown grant {}", op.grant)));
    };
    if grant.holder != op.principal {
        return Err(Refusal::new(
            RefusalCode::Forbidden,
            format!("grant {} is not held by this principal", grant.id),
        ));
    }
    Ok(())
}
