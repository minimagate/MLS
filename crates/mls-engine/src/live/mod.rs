//! Live mode (M4): incremental formalization of an open document, independent of any transport.
//!
//! - [`Document`] re-analyzes a source and reports what changed, by block identity.
//! - [`SirCache`] and [`LeanCache`] hold reusable results under content-hash keys.
//! - [`Gate`] decides whether a dirty block may be formalized now.
//! - [`Engine`] ties these together with the review store and the scheduler. Executing a job is
//!   the caller's business: [`PipelineRunner`] runs one with the batch pipeline's own steps.

mod cache;
mod document;
mod engine;
mod gate;
mod runner;
mod scheduler;

pub use cache::{LeanCache, LeanCacheEntry, SirCache, SirCacheEntry, SirProvenance, sir_cache_key};
pub use document::{BlockState, ChangeSet, Document, DocumentError};
pub use engine::{ActionError, Engine, EngineConfig, ReviewAction, UpdateHints};
pub use gate::{
    Clock, Finished, FinishedOracle, FinishedQuery, Gate, GateConfig, ManualClock, OracleAnswer,
    SystemClock,
};
pub use runner::{Dependency, JobContext, JobOutcome, JobResult, JobRunner, PipelineRunner};
pub use scheduler::{Completion, Job, Priority, Stale};
