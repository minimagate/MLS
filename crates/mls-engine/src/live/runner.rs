//! Executing one job: the context the engine hands to a runner, the runner trait, and the runner
//! built from the batch pipeline's own formalization step.

use super::cache::LeanCache;
use super::scheduler::Job;
use crate::{
    AcceptedDependency, BlockInput, BlockReport, PipelineConfig, ReviewStore, StatementChecker,
    UsageSummary, formalize_block,
};
use mls_core::{Block, BlockId};
use mls_models::ModelClient;
use mls_sir::Sir;

/// A formal dependency whose statement is accepted in effect.
#[derive(Clone, Debug)]
pub struct Dependency {
    /// The dependency's identity.
    pub block: BlockId,
    /// The declaration name the dependency's Lean statement uses.
    pub name: String,
    /// The accepted Lean declaration.
    pub declaration: String,
    /// The SIR in effect for the dependency.
    pub sir: Sir,
}

/// Everything a runner needs to execute one job. The engine builds it from its current state.
pub struct JobContext<'a> {
    /// The block to formalize.
    pub block: &'a Block,
    /// Canonical text of the block, or `None` when canonicalization failed.
    pub canonical: Option<&'a [u8]>,
    /// The labels the block's SIR must list in `uses`.
    pub expected_uses: Vec<String>,
    /// A formal dependency has no accepted statement, so the block is blocked.
    pub failed_dependency: bool,
    /// The accepted formal dependencies, in reference order.
    pub dependencies: Vec<Dependency>,
    /// A SIR generated earlier for the same inputs. The runner may use it instead of a model call.
    pub sir_candidate: Option<Sir>,
    /// Human decisions in effect.
    pub review: &'a ReviewStore,
    /// Accepted declarations from earlier runs.
    pub lean_cache: &'a LeanCache,
}

impl JobContext<'_> {
    /// A declaration accepted earlier under `cache_key`, from the confirmed-SIR cache of the review
    /// store or from the live Lean cache. The runner must re-check it before accepting it.
    #[must_use]
    pub fn lean_candidate(&self, cache_key: &str) -> Option<String> {
        if let Some(cached) = self.review.cached_lean(self.block.id)
            && cached.cache_key == cache_key
        {
            return Some(cached.declaration.clone());
        }
        self.lean_cache
            .lookup(self.block.id, cache_key)
            .map(str::to_owned)
    }
}

/// The result of a job that ran to the end of the formalization.
#[derive(Clone, Debug)]
pub struct JobResult {
    /// The block's report, with `text_hash` set to the job's text hash.
    pub report: BlockReport,
    /// Model usage of the job.
    pub usage: UsageSummary,
    /// The SIR came from a model call that was not conditioned on a rejection, so it may be cached.
    pub sir_cacheable: bool,
}

/// What a runner returns for a job.
#[derive(Clone, Debug)]
pub enum JobOutcome {
    /// The formalization ran.
    Completed(Box<JobResult>),
    /// The runner failed. Record the error and usage, and settle without an accepted statement.
    /// Failures are terminal until an edit, review action, or explicit [`super::Engine::retry`].
    /// Include every attempted model call, retaining unknown usage fields as unknown.
    Failed {
        /// Why the runner failed.
        message: String,
        /// Usage incurred before the failure; use an empty summary only when no model was called.
        usage: UsageSummary,
    },
}

/// Executes jobs. The live engine calls it; the server supplies the implementation.
pub trait JobRunner {
    /// Run one job against its context and return the outcome.
    fn run(&mut self, job: &Job, context: &JobContext<'_>) -> JobOutcome;
}

/// A runner that formalizes a block with the batch pipeline's own steps: SIR from the cache or the
/// model, Lean from a re-checked cache entry or the model with bounded repair, and structure checks.
pub struct PipelineRunner<'a, M, C> {
    models: &'a mut M,
    checker: &'a mut C,
    config: PipelineConfig,
}

impl<'a, M, C> PipelineRunner<'a, M, C> {
    /// A runner over one model client and one Lean checker.
    pub fn new(models: &'a mut M, checker: &'a mut C, config: PipelineConfig) -> Self {
        Self {
            models,
            checker,
            config,
        }
    }
}

impl<M: ModelClient, C: StatementChecker> JobRunner for PipelineRunner<'_, M, C> {
    fn run(&mut self, job: &Job, context: &JobContext<'_>) -> JobOutcome {
        debug_assert_eq!(job.block, context.block.id);
        let dependencies: Vec<AcceptedDependency> = context
            .dependencies
            .iter()
            .map(|dependency| AcceptedDependency {
                id: dependency.block,
                name: dependency.name.clone(),
                declaration: dependency.declaration.clone(),
                sir: dependency.sir.clone(),
            })
            .collect();
        let lookup = |key: &str| context.lean_candidate(key);
        let mut usage = UsageSummary::default();
        let output = formalize_block(
            &mut *self.models,
            &mut *self.checker,
            self.config,
            &mut usage,
            BlockInput {
                block: context.block,
                canonical: context.canonical,
                failed_dependency: context.failed_dependency,
                dependencies: &dependencies,
                expected_uses: &context.expected_uses,
                review: context.review,
                cached_sir: context.sir_candidate.as_ref(),
                lean_lookup: &lookup,
                reuse_generated_lean: true,
                initial_premises: None,
                repair_premises: None,
            },
        );
        JobOutcome::Completed(Box::new(JobResult {
            report: output.report,
            usage,
            sir_cacheable: output.sir_cacheable,
        }))
    }
}
