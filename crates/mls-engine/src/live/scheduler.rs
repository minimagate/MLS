//! Scheduling vocabulary: jobs, their priority, and how a finished job settles.
//!
//! The engine chooses jobs in this order: the block the cursor has just left, then its formal
//! dependents, then every other dirty block, each group in dependency order. A block's dependents
//! wait until the block itself has settled.

use mls_core::BlockId;
use serde::{Deserialize, Serialize};

/// Why a dirty block is scheduled before others. Earlier variants run first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// The block the cursor has just left.
    JustLeft,
    /// A formal dependent, direct or transitive, of the block the cursor has just left.
    Dependent,
    /// Any other dirty block.
    Other,
}

/// One unit of formalization work, issued by the engine and returned to it with an outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub(crate) ticket: u64,
    pub(crate) generation: u64,
    /// The block to formalize.
    pub block: BlockId,
    /// The job only generates/checks a proof for the already settled confirmed statement.
    pub proof_only: bool,
    /// The document revision when the job was issued.
    pub revision: u64,
    /// The block's canonical text hash when the job was issued.
    pub text_hash: String,
    /// The in-effect SIR hash of each formal dependency, in reference order. `None` means the
    /// dependency has no accepted statement, so this block is blocked.
    pub dependency_hashes: Vec<(BlockId, Option<String>)>,
    /// The block's review epoch when the job was issued.
    pub review_mark: u64,
    /// Why the job was chosen.
    pub priority: Priority,
}

/// Why a finished job's result was discarded instead of applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    /// The job was abandoned, already completed, or replaced by another issuance.
    NotInFlight,
    /// The block left the document.
    BlockRemoved,
    /// The block's canonical text changed after the job was issued.
    TextChanged,
    /// An in-effect dependency SIR changed after the job was issued.
    DependencyChanged,
    /// The block's review decision changed after the job was issued.
    ReviewChanged,
    /// The block's kind, labels, references, or transitive inputs were invalidated.
    InputsChanged,
}

/// What became of a finished job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    /// The result was applied and the block settled.
    Applied,
    /// The result was discarded; current dirty work, if any, remains scheduled.
    Stale(Stale),
}

/// A dirty block in the order the engine would run it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Candidate {
    pub(crate) priority: Priority,
    pub(crate) rank: usize,
    pub(crate) block: BlockId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_order_by_priority_then_dependency_rank() {
        let mut candidates = [
            Candidate {
                priority: Priority::Other,
                rank: 0,
                block: BlockId::new(),
            },
            Candidate {
                priority: Priority::Dependent,
                rank: 5,
                block: BlockId::new(),
            },
            Candidate {
                priority: Priority::JustLeft,
                rank: 9,
                block: BlockId::new(),
            },
        ];
        candidates.sort();
        let priorities: Vec<Priority> = candidates.iter().map(|c| c.priority).collect();
        assert_eq!(
            priorities,
            [Priority::JustLeft, Priority::Dependent, Priority::Other]
        );
    }
}
