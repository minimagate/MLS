//! The live engine: the document, caches, review store, gate and scheduler behind one
//! deterministic state machine. It performs no I/O and starts no process. The caller supplies the
//! source, the active block and the time, and runs the jobs the engine hands out.

use super::cache::{LeanCache, SirCache, SirProvenance, sir_cache_key};
use super::document::{ChangeSet, Document, DocumentError, is_formal};
use super::gate::{FinishedOracle, Gate, GateConfig};
use super::runner::{Dependency, JobContext, JobOutcome, JobResult, JobRunner};
use super::scheduler::{Candidate, Completion, Job, Priority, Stale};
use crate::{
    BatchReport, BlockReport, BlockStatus, ReviewEntry, ReviewError, ReviewStore, ReviewView,
    UsageSummary, declaration_name, empty_report, review_view, validate_sir_identity,
};
use mls_core::{Block, BlockId};
use mls_sir::Sir;
use std::collections::{BTreeMap, BTreeSet};

/// Settings of a live engine.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Name of the document, shown in the view.
    pub document: String,
    /// Generation model ID. It is part of every SIR cache key.
    pub model: String,
    /// Debounce and oracle settings.
    pub gate: GateConfig,
    /// Maximum number of cached SIRs.
    pub sir_cache_capacity: usize,
    /// Maximum number of cached Lean declarations.
    pub lean_cache_capacity: usize,
}

/// A human decision on one block's SIR, as the CLI's `review-action` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    /// Confirm the SIR in the block's last report.
    Confirm(BlockId),
    /// Reject the SIR in the block's last report. The block is formalized again, with the rejection.
    Reject(BlockId),
    /// Replace the SIR with this strict JSON. The block is formalized again.
    Edit {
        /// The block whose SIR is replaced.
        block: BlockId,
        /// The replacement SIR, as JSON.
        sir_json: String,
    },
}

/// Optional decision-layer hints for one complete source update. These are scheduling inputs;
/// they cannot confirm a SIR, preserve a checked statement, or alter a structural check.
#[derive(Clone, Debug, Default)]
pub struct UpdateHints {
    /// Complete implicit-link overlay: source block to one earlier labeled target.
    pub implicit_links: BTreeMap<BlockId, BlockId>,
    /// Confident Jev classifications that an untagged prose block is a mathematical claim.
    /// Proof steps, commentary and fallback results are omitted and remain non-formal.
    pub prose_claims: BTreeSet<BlockId>,
    /// Blocks whose edit was judged cosmetic. Unsafe or inapplicable hints are ignored.
    pub cosmetic_edits: BTreeSet<BlockId>,
}

/// A review action that was refused. A refused action leaves the engine unchanged.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    /// The block is not a formal block of the document.
    #[error("{0} is not a formal block of this document")]
    UnknownBlock(BlockId),
    /// The block has no report from a finished formalization yet.
    #[error("block {0} has no finished formalization yet; wait for it and retry")]
    NotInRun(BlockId),
    /// The review store refused the decision.
    #[error(transparent)]
    Review(#[from] ReviewError),
}

/// What the last applied job of a block settled, which its dependents read.
#[derive(Clone, Debug)]
struct Settled {
    /// The canonical text hash the job was run for.
    text_hash: String,
    /// The block has an accepted Lean statement for its SIR.
    accepted: bool,
    /// The SIR the job decided.
    sir: Option<Sir>,
    /// Semantic hash of `sir`.
    sir_hash: Option<String>,
    /// The accepted declaration, when `accepted`.
    declaration: Option<String>,
}

/// The live formalization state of one document.
///
/// Typical use: call [`Engine::on_change`] on every edit, [`Engine::on_cursor`] on every cursor
/// move, and [`Engine::run_next`] (or [`Engine::next_job`] and [`Engine::complete`]) whenever the
/// runner is idle. Read [`Engine::view`] after any of them.
pub struct Engine {
    config: EngineConfig,
    document: Document,
    review: ReviewStore,
    sir_cache: SirCache,
    lean_cache: LeanCache,
    gate: Gate,
    /// Formal blocks that must be formalized again.
    dirty: BTreeSet<BlockId>,
    /// Confirmed blocks with settled statements whose proof has not been attempted yet.
    proof_pending: BTreeSet<BlockId>,
    /// When each dirty block last changed or was dirtied, in milliseconds.
    touched: BTreeMap<BlockId, u64>,
    /// Blocks the cursor has just left, which skip the debounce.
    left_ready: BTreeSet<BlockId>,
    active: Option<BlockId>,
    /// The block the cursor has most recently left. It sets the first two priorities.
    just_left: Option<BlockId>,
    in_flight: BTreeMap<BlockId, u64>,
    next_ticket: u64,
    /// Invalidation epochs include transitive changes even if their SIR hashes are unchanged.
    generations: BTreeMap<BlockId, u64>,
    settled: BTreeMap<BlockId, Settled>,
    /// The report of the last applied job of each formal block.
    reports: BTreeMap<BlockId, BlockReport>,
    /// Review epoch of each block. A review action bumps it, which makes in-flight jobs stale.
    review_marks: BTreeMap<BlockId, u64>,
    usage: UsageSummary,
}

impl Engine {
    /// An engine with an empty document. The first [`Engine::on_change`] loads the document.
    #[must_use]
    pub fn new(config: EngineConfig, review: ReviewStore) -> Self {
        let sir_cache = SirCache::new(config.sir_cache_capacity);
        let lean_cache = LeanCache::new(config.lean_cache_capacity);
        let gate = Gate::new(config.gate);
        Self {
            config,
            document: Document::new(),
            review,
            sir_cache,
            lean_cache,
            gate,
            dirty: BTreeSet::new(),
            proof_pending: BTreeSet::new(),
            touched: BTreeMap::new(),
            left_ready: BTreeSet::new(),
            active: None,
            just_left: None,
            in_flight: BTreeMap::new(),
            next_ticket: 0,
            generations: BTreeMap::new(),
            settled: BTreeMap::new(),
            reports: BTreeMap::new(),
            review_marks: BTreeMap::new(),
            usage: UsageSummary::default(),
        }
    }

    /// Install the "is this block finished?" oracle. Without one, the debounce alone decides.
    pub fn set_oracle(&mut self, oracle: Box<dyn FinishedOracle>) {
        self.gate.set_oracle(oracle);
    }

    /// The live document.
    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// The human decisions in effect.
    #[must_use]
    pub fn review(&self) -> &ReviewStore {
        &self.review
    }

    /// The human decisions. The caller persists them, and forgets the decisions of blocks listed in
    /// [`ChangeSet::removed`], because the engine leaves the store alone.
    pub fn review_mut(&mut self) -> &mut ReviewStore {
        &mut self.review
    }

    /// The SIR cache.
    #[must_use]
    pub fn sir_cache(&self) -> &SirCache {
        &self.sir_cache
    }

    /// The Lean cache.
    #[must_use]
    pub fn lean_cache(&self) -> &LeanCache {
        &self.lean_cache
    }

    /// Model usage of every returned outcome, including failures and stale results.
    #[must_use]
    pub fn usage(&self) -> &UsageSummary {
        &self.usage
    }

    /// The formal blocks that must be formalized again.
    #[must_use]
    pub fn dirty(&self) -> &BTreeSet<BlockId> {
        &self.dirty
    }

    /// Apply a new source. The active block is the one that holds the cursor, if any.
    ///
    /// An invalid source is refused and leaves the engine unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentError`] when the source is refused.
    pub fn on_change(
        &mut self,
        source: &[u8],
        active: Option<BlockId>,
        now: u64,
    ) -> Result<ChangeSet, DocumentError> {
        self.on_change_with_hints(source, active, now, &UpdateHints::default())
    }

    /// Whether this block presently has no current, settled, reviewed or cached SIR and no
    /// outstanding job. This is only a preliminary check: [`Self::on_change_with_hints`] also
    /// requires a strictly text-only edit without upstream invalidation.
    #[must_use]
    pub fn cosmetic_hint_eligible(&self, id: BlockId) -> bool {
        let Some(block) = self.document.block(id) else {
            return false;
        };
        if !is_formal(block.kind)
            || self.in_flight.contains_key(&id)
            || self.review.entry(id).is_some()
            || self
                .reports
                .get(&id)
                .is_some_and(|report| report.sir.is_some())
            || self
                .settled
                .get(&id)
                .is_some_and(|settled| settled.sir.is_some())
        {
            return false;
        }
        let key = sir_cache_key(
            &self.config.model,
            &block.text_hash,
            &applied_dependencies(&self.dependency_hashes(id)),
        );
        self.sir_cache.lookup(&key).is_none()
    }

    /// Apply strict implicit links and conservative cosmetic scheduling hints. Link errors refuse
    /// the entire update; unsafe cosmetic hints fall back to normal invalidation. An accepted
    /// cosmetic hint suppresses only that block's work and clears its local report/settlement;
    /// the new text is always installed and downstream invalidation still runs normally.
    pub fn on_change_with_hints(
        &mut self,
        source: &[u8],
        active: Option<BlockId>,
        now: u64,
        hints: &UpdateHints,
    ) -> Result<ChangeSet, DocumentError> {
        let eligible: BTreeMap<_, _> = hints
            .cosmetic_edits
            .iter()
            .filter(|id| self.cosmetic_hint_eligible(**id))
            .filter_map(|id| self.document.block(*id).cloned().map(|state| (*id, state)))
            .collect();
        let change = self.document.update_with_links_and_prose_claims(
            source,
            &hints.implicit_links,
            &hints.prose_claims,
        )?;
        for id in &change.removed {
            self.forget_block(*id);
        }
        for id in &change.changed {
            if self
                .document
                .block(*id)
                .is_some_and(|block| !is_formal(block.kind))
            {
                self.forget_block(*id);
            }
        }
        for id in self.document.dirty(&change) {
            self.mark_dirty(id, now);
        }
        for (id, old) in eligible {
            let Some(new) = self.document.block(id) else {
                continue;
            };
            if old.kind == new.kind
                && old.label == new.label
                && old.deps == new.deps
                && old.position == new.position
                && old.section == new.section
                && old.text_hash != new.text_hash
                && change.changed.contains(&id)
                && !change.previous_dependents.contains(&id)
                && self.document.canonical(id).is_some()
                && self.cosmetic_hint_eligible(id)
                && self.upstream_ready(id)
            {
                self.dirty.remove(&id);
                self.touched.remove(&id);
                self.left_ready.remove(&id);
                self.reports.remove(&id);
                self.settled.remove(&id);
            }
        }
        self.block_dependency_cycles();
        self.move_cursor(active);
        Ok(change)
    }

    /// Move the cursor. Leaving a block makes it eligible at once, and first in line.
    pub fn on_cursor(&mut self, active: Option<BlockId>) {
        self.move_cursor(active);
    }

    /// Issue the next job the gate admits, or `None` when nothing is eligible now.
    ///
    /// The block is marked in flight until [`Engine::complete`] or [`Engine::abandon`].
    pub fn next_job(&mut self, now: u64) -> Option<Job> {
        let downstream = self
            .just_left
            .map(|left| self.document.downstream([left]))
            .unwrap_or_default();
        let mut candidates = Vec::new();
        for id in self.dirty.union(&self.proof_pending) {
            if self.active == Some(*id)
                || self.in_flight.contains_key(id)
                || !self.upstream_ready(*id)
            {
                continue;
            }
            let Some(rank) = self.document.dependency_rank(*id) else {
                continue;
            };
            let priority = if self.just_left == Some(*id) {
                Priority::JustLeft
            } else if downstream.contains(id) {
                Priority::Dependent
            } else {
                Priority::Other
            };
            candidates.push(Candidate {
                priority,
                rank,
                block: *id,
            });
        }
        candidates.sort();
        for candidate in candidates {
            let id = candidate.block;
            let Some(state) = self.document.block(id) else {
                continue;
            };
            let text_hash = state.text_hash.clone();
            let text = self
                .document
                .canonical(id)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let touched = self.touched.get(&id).copied().unwrap_or(now);
            let left = self.left_ready.contains(&id);
            if !self.gate.admits(id, &text_hash, &text, touched, left, now) {
                continue;
            }
            let job = Job {
                ticket: self.next_ticket,
                generation: self.generations.get(&id).copied().unwrap_or(0),
                block: id,
                proof_only: !self.dirty.contains(&id) && self.proof_pending.contains(&id),
                revision: self.document.revision(),
                text_hash,
                dependency_hashes: self.dependency_hashes(id),
                review_mark: self.review_mark(id),
                priority: candidate.priority,
            };
            self.next_ticket += 1;
            self.in_flight.insert(id, job.ticket);
            self.left_ready.remove(&id);
            return Some(job);
        }
        None
    }

    /// Release a job that the caller cannot finish. The block stays dirty and may be issued again.
    /// A late outcome for this issuance is stale and cannot release its replacement.
    /// Return `false` if this is no longer the block's current issuance.
    pub fn abandon(&mut self, job: &Job) -> bool {
        if self.in_flight.get(&job.block) != Some(&job.ticket) {
            return false;
        }
        self.in_flight.remove(&job.block);
        true
    }

    /// Retry a terminal failure (or any settled formal block) and its dependents after debounce.
    /// Return `false` when the identity is not a current formal block.
    pub fn retry(&mut self, id: BlockId, now: u64) -> bool {
        if !self
            .document
            .block(id)
            .is_some_and(|block| is_formal(block.kind))
        {
            return false;
        }
        let mut affected = self.document.downstream([id]);
        affected.insert(id);
        for affected_id in affected {
            self.mark_dirty(affected_id, now);
        }
        self.block_dependency_cycles();
        true
    }

    /// Everything a runner needs for `job`, or `None` when the block left the document.
    #[must_use]
    pub fn context(&self, job: &Job) -> Option<JobContext<'_>> {
        let block = self.document.block_info(job.block)?;
        let mut failed_dependency = false;
        let mut dependencies = Vec::new();
        for dependency in self.document.formal_dependencies(job.block) {
            match self.accepted_dependency(dependency) {
                Some(found) => dependencies.push(found),
                None => failed_dependency = true,
            }
        }
        Some(JobContext {
            block,
            proof_only: job.proof_only,
            current_report: self.reports.get(&job.block),
            canonical: self.document.canonical(job.block),
            expected_uses: expected_uses(&self.document, block),
            failed_dependency,
            dependencies,
            sir_candidate: self.sir_candidate(job),
            review: &self.review,
            lean_cache: &self.lean_cache,
        })
    }

    /// Apply the outcome of a job, unless it is stale.
    ///
    /// A stale result is discarded: its inputs changed, an upstream job is pending, or its issuance
    /// was abandoned. Return each outcome once, including outcomes of abandoned jobs that still ran,
    /// so usage is counted exactly once. Failed outcomes settle until [`Engine::retry`] or an edit.
    pub fn complete(&mut self, job: Job, outcome: JobOutcome) -> Completion {
        // Cost is incurred regardless of whether its mathematical result is still usable.
        let usage = match &outcome {
            JobOutcome::Completed(result) => &result.usage,
            JobOutcome::Failed { usage, .. } => usage,
        };
        add_usage(&mut self.usage, usage);
        if self.in_flight.get(&job.block) != Some(&job.ticket) {
            return Completion::Stale(self.staleness(&job).unwrap_or(Stale::NotInFlight));
        }
        self.in_flight.remove(&job.block);
        if let Some(stale) = self.staleness(&job) {
            return Completion::Stale(stale);
        }
        if job.proof_only {
            self.proof_pending.remove(&job.block);
        }
        let Some(block) = self.document.block_info(job.block) else {
            return Completion::Stale(Stale::BlockRemoved);
        };
        let (report, sir_cacheable, completed) = match outcome {
            JobOutcome::Completed(result) => {
                let JobResult {
                    report,
                    sir_cacheable,
                    ..
                } = *result;
                (report, sir_cacheable, true)
            }
            JobOutcome::Failed { message, .. } => {
                let mut report = empty_report(block);
                report.errors.push(message);
                (report, false, false)
            }
        };
        let accepted = completed && report.accepted_speculatively;
        let dependencies = applied_dependencies(&job.dependency_hashes);
        if sir_cacheable && let Some(sir) = report.sir.clone() {
            let key = sir_cache_key(&self.config.model, &job.text_hash, &dependencies);
            let provenance = SirProvenance {
                model: self.config.model.clone(),
                schema_version: crate::FORMALIZATION_SCHEMA_VERSION.into(),
                text_hash: job.text_hash.clone(),
                dependencies: dependencies.clone(),
                revision: self.document.revision(),
            };
            self.sir_cache.record(key, sir, provenance);
        }
        if accepted
            && report.lean_origin.as_deref() == Some("generated")
            && let (Some(key), Some(declaration)) =
                (report.cache_key.clone(), report.lean_statement.clone())
        {
            self.lean_cache
                .record(job.block, key, declaration, self.document.revision());
        }
        cache_confirmed_lean(&mut self.review, block, &report);
        self.settled.insert(
            job.block,
            Settled {
                text_hash: job.text_hash,
                accepted,
                sir: report.sir.clone(),
                sir_hash: report.sir_hash.clone(),
                declaration: if accepted {
                    report.lean_statement.clone()
                } else {
                    None
                },
            },
        );
        self.reports.insert(job.block, report);
        self.dirty.remove(&job.block);
        Completion::Applied
    }

    /// Issue the next eligible job, run it with `runner`, and apply the outcome. Returns `None`
    /// when no job is eligible.
    pub fn run_next(&mut self, runner: &mut impl JobRunner, now: u64) -> Option<Completion> {
        let job = self.next_job(now)?;
        let outcome = match self.context(&job) {
            Some(context) => runner.run(&job, &context),
            None => JobOutcome::Failed {
                message: "block is not in the document".into(),
                usage: UsageSummary::default(),
            },
        };
        Some(self.complete(job, outcome))
    }

    /// Apply a human decision, with the preconditions of the CLI's `review-action`: the block must
    /// be formal and must have a report from the last finished formalization. A refused action
    /// changes nothing.
    ///
    /// A reject or edit changes the SIR that dependents read, so the block and its formal
    /// dependents become dirty. A confirmation keeps the SIR, so nothing becomes dirty.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError`] when the action is refused.
    pub fn on_review(&mut self, action: ReviewAction, now: u64) -> Result<(), ActionError> {
        let id = match &action {
            ReviewAction::Confirm(id) | ReviewAction::Reject(id) => *id,
            ReviewAction::Edit { block, .. } => *block,
        };
        let block = self
            .document
            .block_info(id)
            .filter(|block| is_formal(block.kind))
            .ok_or(ActionError::UnknownBlock(id))?;
        let report = self.reports.get(&id).ok_or(ActionError::NotInRun(id))?;
        let reformalize = !matches!(action, ReviewAction::Confirm(_));
        match action {
            ReviewAction::Confirm(_) => {
                self.review.confirm(block, report)?;
                cache_confirmed_lean(&mut self.review, block, report);
                if report.proof.is_none()
                    && report.accepted_speculatively
                    && report
                        .sir
                        .as_ref()
                        .is_some_and(|sir| sir.kind() != mls_sir::StatementKind::Definition)
                {
                    self.proof_pending.insert(id);
                }
            }
            ReviewAction::Reject(_) => {
                self.review.reject(block, report)?;
                self.proof_pending.remove(&id);
                if let Some(report) = self.reports.get_mut(&id) {
                    report.proof = None;
                }
            }
            ReviewAction::Edit { sir_json, .. } => {
                let uses = expected_uses(&self.document, block);
                self.review.edit(block, &uses, &sir_json)?;
                self.proof_pending.remove(&id);
                if let Some(report) = self.reports.get_mut(&id) {
                    report.proof = None;
                }
            }
        }
        self.review_marks
            .entry(id)
            .and_modify(|mark| *mark += 1)
            .or_insert(1);
        if reformalize {
            if let Some(settled) = self.settled.get_mut(&id) {
                settled.accepted = false;
            }
            let mut affected = self.document.downstream([id]);
            affected.insert(id);
            for affected_id in affected {
                self.mark_dirty(affected_id, now);
            }
            self.block_dependency_cycles();
        }
        Ok(())
    }

    /// The review view of the document, from the last applied reports and the decisions in effect.
    /// Blocks without a finished report appear as not formalized and need a rerun. Dirty blocks
    /// expose no accepted Lean statement or current checks until a new outcome is applied.
    #[must_use]
    pub fn view(&self) -> ReviewView {
        let blocks = self
            .document
            .order()
            .iter()
            .filter_map(|id| {
                let mut report = self.reports.get(id)?.clone();
                if self.dirty.contains(id) {
                    // Retain the SIR for review, but never present an invalidated declaration as
                    // accepted. Resolve dependencies against these pending reports as well.
                    report.accepted_speculatively = false;
                    report.elaborated = false;
                    report.complete_structure = false;
                    report.lean_statement = None;
                    report.proof = None;
                    report.checks.clear();
                    report.status = if report.sir.is_some() {
                        BlockStatus::SirDrafted
                    } else {
                        BlockStatus::NotFormalized
                    };
                }
                Some(report)
            })
            .collect();
        let report = BatchReport {
            diagnostics: self.document.analysis().diagnostics.clone(),
            blocks,
            usage: self.usage.clone(),
        };
        let mut view = review_view(
            &self.config.document,
            self.document.analysis(),
            &report,
            &self.review,
        );
        for block in &mut view.blocks {
            if self.proof_pending.contains(&block.block_id) && !self.dirty.contains(&block.block_id)
            {
                block.needs_rerun = true;
                block.message = "proof pending for confirmed statement".into();
            }
            if self.dirty.contains(&block.block_id) {
                // A retained usable SIR is drafted while its Lean is queued or being rechecked,
                // as for pending human edits. An actual settled failure remains an error.
                if block.status == BlockStatus::StatementConfirmed {
                    block.status = BlockStatus::SirDrafted;
                }
                block.needs_rerun = true;
                block.message = "formalization pending; Lean statement must be rechecked".into();
            }
        }
        // The live pending overlay can change statuses returned by the batch review resolver.
        view.counts = crate::StatusCounts::default();
        for block in &view.blocks {
            let count = match block.status {
                BlockStatus::NotFormalized => &mut view.counts.not_formalized,
                BlockStatus::SirDrafted => &mut view.counts.sir_drafted,
                BlockStatus::StatementConfirmed => &mut view.counts.statement_confirmed,
                BlockStatus::FormalizationError => &mut view.counts.formalization_error,
            };
            *count += 1;
        }
        view.needs_rerun |= !self.dirty.is_empty();
        view.needs_rerun |= !self.proof_pending.is_empty();
        view
    }

    fn move_cursor(&mut self, active: Option<BlockId>) {
        let active = active.filter(|id| self.document.contains(*id));
        if active == self.active {
            return;
        }
        if let Some(left) = self.active {
            self.left_ready.insert(left);
            self.just_left = Some(left);
        }
        if let Some(entered) = active {
            self.left_ready.remove(&entered);
            if self.just_left == Some(entered) {
                self.just_left = None;
            }
        }
        self.active = active;
    }

    fn forget_block(&mut self, id: BlockId) {
        self.dirty.remove(&id);
        self.proof_pending.remove(&id);
        self.touched.remove(&id);
        self.left_ready.remove(&id);
        self.settled.remove(&id);
        self.reports.remove(&id);
        self.review_marks.remove(&id);
        self.in_flight.remove(&id);
        self.generations.remove(&id);
        if self.just_left == Some(id) {
            self.just_left = None;
        }
        if self.active == Some(id) {
            self.active = None;
        }
    }

    fn mark_dirty(&mut self, id: BlockId, now: u64) {
        self.dirty.insert(id);
        self.proof_pending.remove(&id);
        self.touched.insert(id, now);
        self.left_ready.remove(&id);
        *self.generations.entry(id).or_default() += 1;
    }

    /// Cycles are deterministic structural failures, including blocks downstream of a cycle.
    /// They settle without a model call and remain visibly blocked until the graph is corrected.
    fn block_dependency_cycles(&mut self) {
        for id in self.document.cycle_blocked() {
            if !self.dirty.remove(id) {
                continue;
            }
            let block = self.document.block_info(*id).expect("cycle blocks exist");
            let mut report = empty_report(block);
            report
                .errors
                .push("dependency cycle prevents formalization".into());
            self.reports.insert(*id, report);
            self.settled.remove(id);
            self.left_ready.remove(id);
        }
    }

    /// Every transitive formal dependency has settled and has no outstanding job.
    fn upstream_ready(&self, id: BlockId) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = self.document.formal_dependencies(id);
        while let Some(dependency) = pending.pop() {
            if !seen.insert(dependency) {
                continue;
            }
            if self.dirty.contains(&dependency) || self.in_flight.contains_key(&dependency) {
                return false;
            }
            pending.extend(self.document.formal_dependencies(dependency));
        }
        true
    }

    fn dependency_hashes(&self, id: BlockId) -> Vec<(BlockId, Option<String>)> {
        self.document
            .formal_dependencies(id)
            .into_iter()
            .map(|dependency| (dependency, self.settled_hash(dependency)))
            .collect()
    }

    /// The SIR hash in effect for a formal block: the hash its last job settled, if that job
    /// accepted it and its text is still current.
    fn settled_hash(&self, id: BlockId) -> Option<String> {
        let settled = self.settled.get(&id)?;
        if settled.accepted && self.is_current(id, &settled.text_hash) {
            settled.sir_hash.clone()
        } else {
            None
        }
    }

    fn accepted_dependency(&self, id: BlockId) -> Option<Dependency> {
        let settled = self.settled.get(&id)?;
        if !settled.accepted || !self.is_current(id, &settled.text_hash) {
            return None;
        }
        Some(Dependency {
            block: id,
            name: declaration_name(id),
            declaration: settled.declaration.clone()?,
            sir: settled.sir.clone()?,
        })
    }

    fn is_current(&self, id: BlockId, text_hash: &str) -> bool {
        self.document
            .block(id)
            .is_some_and(|state| state.text_hash == text_hash)
    }

    fn review_mark(&self, id: BlockId) -> u64 {
        self.review_marks.get(&id).copied().unwrap_or(0)
    }

    fn sir_candidate(&self, job: &Job) -> Option<Sir> {
        if matches!(
            self.review.entry(job.block),
            Some(ReviewEntry::Rejected { .. })
        ) {
            return None;
        }
        let key = sir_cache_key(
            &self.config.model,
            &job.text_hash,
            &applied_dependencies(&job.dependency_hashes),
        );
        let block = self.document.block_info(job.block)?;
        self.sir_cache
            .lookup(&key)
            // Non-formal label aliases can change expected `uses` without changing this key.
            .filter(|entry| {
                validate_sir_identity(block, &entry.sir, &expected_uses(&self.document, block))
                    .is_empty()
            })
            .map(|entry| entry.sir.clone())
    }

    fn staleness(&self, job: &Job) -> Option<Stale> {
        let Some(state) = self.document.block(job.block) else {
            return Some(Stale::BlockRemoved);
        };
        if state.text_hash != job.text_hash {
            return Some(Stale::TextChanged);
        }
        if self.review_mark(job.block) != job.review_mark {
            return Some(Stale::ReviewChanged);
        }
        if !self.upstream_ready(job.block) {
            return Some(Stale::DependencyChanged);
        }
        if self.dependency_hashes(job.block) != job.dependency_hashes {
            return Some(Stale::DependencyChanged);
        }
        if self.generations.get(&job.block).copied().unwrap_or(0) != job.generation {
            return Some(Stale::InputsChanged);
        }
        None
    }
}

/// The labels a block's SIR must list in `uses`, in reference order.
fn expected_uses(document: &Document, block: &Block) -> Vec<String> {
    block
        .deps
        .iter()
        .filter_map(|dependency| {
            document
                .block_info(*dependency)
                .and_then(|target| target.label.clone())
        })
        .collect()
}

/// The dependency hashes that are present, as `(block, SIR hash)` pairs.
fn applied_dependencies(hashes: &[(BlockId, Option<String>)]) -> Vec<(BlockId, String)> {
    hashes
        .iter()
        .filter_map(|(id, hash)| hash.clone().map(|hash| (*id, hash)))
        .collect()
}

/// Record a confirmed block's accepted declaration in the review store, as the CLI does, when the
/// report's SIR is the one in effect.
fn cache_confirmed_lean(review: &mut ReviewStore, block: &Block, report: &BlockReport) {
    let in_effect = matches!(
        review.entry(block.id),
        Some(ReviewEntry::Confirmed { sir_hash, .. }) if Some(sir_hash) == report.sir_hash.as_ref()
    );
    if !in_effect || !report.accepted_speculatively || report.block_id != block.id {
        return;
    }
    if let (Some(key), Some(declaration)) = (&report.cache_key, &report.lean_statement) {
        let _ = review.record_accepted_lean(block.id, key.clone(), declaration.clone());
    }
}

fn add_usage(total: &mut UsageSummary, part: &UsageSummary) {
    total.calls += part.calls;
    total.input_tokens += part.input_tokens;
    total.output_tokens += part.output_tokens;
    total.unknown_input_token_calls += part.unknown_input_token_calls;
    total.unknown_output_token_calls += part.unknown_output_token_calls;
    total.known_cost += part.known_cost;
    total.unknown_cost_calls += part.unknown_cost_calls;
}
