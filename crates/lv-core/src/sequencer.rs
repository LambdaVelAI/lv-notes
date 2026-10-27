//! The sequencer: the one place ops are accepted into a project.
//!
//! Clients submit batches of ops, each with the version it was based on.
//! The sequencer checks each op (project, base, duplicates, the permission
//! check, then [`ProjectState::apply`] against the head), assigns positions,
//! writes the batch to the log, and only then advances. A batch is accepted
//! or refused as a whole.
//!
//! Because ops address IDs, an op made against an older base usually applies
//! unchanged at the head. That is the whole of rebasing for now: re-checking
//! at the head. The checks that depend on the base are `STALE_TEXT` and the
//! state checks (deleted targets, cycles) evaluated at the head.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use crate::auth::authorize;
use crate::ids::{GrantId, OpId, ProjectId, Version};
use crate::op::{Op, Sequenced};
use crate::principal::Grant;
use crate::refusal::{Refusal, RefusalCode};
use crate::state::ProjectState;

/// Where accepted ops go, and where grants are looked up.
pub trait ProjectLog {
    type Error;

    fn grant(&self, id: GrantId) -> Result<Option<Grant>, Self::Error>;

    /// Persists the batch atomically. The sequencer only advances after this succeeds.
    fn append(&mut self, batch: &[Sequenced]) -> Result<(), Self::Error>;
}

/// An in-memory log, for tests and for clients without storage.
#[derive(Clone, Debug, Default)]
pub struct MemoryLog {
    pub grants: HashMap<GrantId, Grant>,
    pub ops: Vec<Sequenced>,
}

impl MemoryLog {
    pub fn add_grant(&mut self, grant: Grant) {
        self.grants.insert(grant.id, grant);
    }
}

impl ProjectLog for MemoryLog {
    type Error = Infallible;

    fn grant(&self, id: GrantId) -> Result<Option<Grant>, Infallible> {
        Ok(self.grants.get(&id).cloned())
    }

    fn append(&mut self, batch: &[Sequenced]) -> Result<(), Infallible> {
        self.ops.extend_from_slice(batch);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError<E> {
    /// The batch was refused; nothing was written.
    #[error(transparent)]
    Refused(#[from] Refusal),
    /// The log failed to persist the batch; the sequencer did not advance.
    #[error("op log failed: {0}")]
    Log(E),
}

/// A logged op that this build cannot apply: the log and the code disagree.
#[derive(Debug, thiserror::Error)]
#[error("op at {seq} in the log does not apply: {refusal}")]
pub struct ReplayError {
    pub seq: Version,
    pub refusal: Refusal,
}

pub struct Sequencer {
    project: ProjectId,
    head: Version,
    state: ProjectState,
    accepted: HashSet<OpId>,
}

impl Sequencer {
    /// A sequencer for an empty project.
    pub fn new(project: ProjectId) -> Sequencer {
        Sequencer { project, head: Version::EMPTY, state: ProjectState::new(), accepted: HashSet::new() }
    }

    /// Rebuilds the sequencer from a project's log, in order.
    pub fn replay(project: ProjectId, log: impl IntoIterator<Item = Sequenced>) -> Result<Sequencer, ReplayError> {
        let mut sequencer = Sequencer::new(project);
        for s in log {
            let refuse = |refusal| ReplayError { seq: s.seq, refusal };
            if s.seq != sequencer.head.next() {
                return Err(refuse(Refusal::new(RefusalCode::InvalidOp, format!("expected {}", sequencer.head.next()))));
            }
            sequencer.state.apply(&s).map_err(refuse)?;
            sequencer.accepted.insert(s.op.id);
            sequencer.head = s.seq;
        }
        Ok(sequencer)
    }

    pub fn project(&self) -> ProjectId {
        self.project
    }

    /// The position of the last accepted op.
    pub fn head(&self) -> Version {
        self.head
    }

    /// The project as of [`Self::head`].
    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    /// Checks, sequences and logs `batch`. Returns the ops with their positions.
    pub fn submit<L: ProjectLog>(&mut self, log: &mut L, batch: Vec<Op>) -> Result<Vec<Sequenced>, SubmitError<L::Error>> {
        if batch.is_empty() {
            return Err(Refusal::new(RefusalCode::InvalidOp, "empty batch").into());
        }

        let mut next = self.state.clone();
        let mut seen = HashSet::new();
        let mut sequenced = Vec::with_capacity(batch.len());
        let mut seq = self.head;
        for op in batch {
            let refuse = |code, message: String| SubmitError::Refused(Refusal::new(code, message).for_op(op.id));
            if op.project != self.project {
                return Err(refuse(RefusalCode::WrongProject, format!("this is project {}", self.project)));
            }
            if op.base > self.head {
                return Err(refuse(RefusalCode::BaseInFuture, format!("base {} is after head {}", op.base, self.head)));
            }
            if self.accepted.contains(&op.id) || !seen.insert(op.id) {
                return Err(refuse(RefusalCode::DuplicateOp, format!("{} was already submitted", op.id)));
            }
            let grant = log.grant(op.grant).map_err(SubmitError::Log)?;
            authorize(&op, grant.as_ref()).map_err(|r| r.for_op(op.id))?;

            seq = seq.next();
            let s = Sequenced { seq, op };
            next.apply(&s)?;
            sequenced.push(s);
        }

        log.append(&sequenced).map_err(SubmitError::Log)?;
        self.state = next;
        self.head = seq;
        self.accepted.extend(sequenced.iter().map(|s| s.op.id));
        Ok(sequenced)
    }
}

#[cfg(feature = "sqlite")]
impl ProjectLog for crate::store::Store {
    type Error = crate::store::StoreError;

    fn grant(&self, id: GrantId) -> Result<Option<Grant>, Self::Error> {
        crate::store::Store::grant(self, id)
    }

    fn append(&mut self, batch: &[Sequenced]) -> Result<(), Self::Error> {
        crate::store::Store::append(self, batch)
    }
}
