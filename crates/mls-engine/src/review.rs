//! Human review decisions for block SIRs, and the Lean declarations unlocked by confirmed SIRs.
//!
//! A `ReviewStore` is pure data plus logic: it never touches the filesystem, and its fields are
//! private so that only the decision methods below can change it. Without a store entry, no block
//! can reach `statement_confirmed`.

use crate::{
    BatchReport, BlockReport, BlockStatus, CheckScope, CheckState, StatusCounts, StructureCheck,
    cache_key_from_hashes, source_kind, validate_sir_identity,
};
use mls_core::{Block, BlockId, BlockKind};
use mls_sir::Sir;
use mls_tm::Analysis;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Version of the serialized review store. Loading any other version is refused.
pub const REVIEW_SCHEMA_VERSION: u32 = 1;

/// Human decisions about block SIRs, plus the Lean declarations accepted for confirmed SIRs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewStore {
    schema_version: u32,
    entries: BTreeMap<BlockId, ReviewEntry>,
    lean_cache: BTreeMap<BlockId, CachedLean>,
}

/// The latest human decision for one block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewEntry {
    /// The SIR is in effect for the block text it was confirmed against.
    Confirmed {
        /// The confirmed or hand-edited SIR.
        sir: Sir,
        /// Semantic hash of `sir`.
        sir_hash: String,
        /// Block text hash at the time of the decision.
        text_hash: String,
        /// Whether the reviewer wrote the SIR by hand instead of confirming a generated one.
        edited: bool,
    },
    /// The SIR was rejected. The next generation for the block receives it as a note.
    Rejected {
        /// The rejected SIR.
        sir: Sir,
        /// Semantic hash of `sir`.
        sir_hash: String,
        /// Block text hash at the time of the decision.
        text_hash: String,
    },
}

/// A Lean declaration accepted for a confirmed SIR. It is reused only while `cache_key` matches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedLean {
    /// Cache key of the run that accepted the declaration.
    pub cache_key: String,
    /// The accepted Lean declaration source.
    pub declaration: String,
}

/// A review decision that was refused. A refused decision leaves the store unchanged.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReviewError {
    /// The report was produced for a different block.
    #[error("report belongs to block {report}, not block {block}")]
    ReportForOtherBlock {
        /// The block being reviewed.
        block: BlockId,
        /// The block the report describes.
        report: BlockId,
    },
    /// The block text changed after the report was produced.
    #[error(
        "report for block {block} is stale: its text changed after the report was produced; rerun before reviewing"
    )]
    StaleReport {
        /// The block whose report is stale.
        block: BlockId,
    },
    /// The report has no SIR that can be confirmed.
    #[error("block {block} has no usable SIR to review: {}", .errors.join("; "))]
    NoUsableSir {
        /// The block being reviewed.
        block: BlockId,
        /// The report's errors, which explain why the SIR is not usable.
        errors: Vec<String>,
    },
    /// The edited text is not a valid SIR under the strict schema.
    #[error("edited SIR is not valid: {0}")]
    InvalidSirJson(String),
    /// The SIR does not match the block's kind, label, dependency labels or variable names.
    #[error("edited SIR does not match the block: {}", .0.join("; "))]
    IdentityMismatch(Vec<String>),
    /// A Lean declaration can only be cached for a block with a confirmed SIR.
    #[error("block {block} has no confirmed SIR")]
    NotConfirmed {
        /// The block without a confirmed SIR.
        block: BlockId,
    },
    /// A stored review is malformed or was written by an unsupported schema.
    #[error("review store is invalid: {0}")]
    InvalidStore(String),
}

impl ReviewStore {
    /// An empty store with no human decisions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: REVIEW_SCHEMA_VERSION,
            entries: BTreeMap::new(),
            lean_cache: BTreeMap::new(),
        }
    }

    /// Parse a stored review. Unknown fields, other schema versions and SIR hashes that do not
    /// match their SIR are refused.
    pub fn from_json(json: &str) -> Result<Self, ReviewError> {
        let store: Self = serde_json::from_str(json)
            .map_err(|error| ReviewError::InvalidStore(error.to_string()))?;
        if store.schema_version != REVIEW_SCHEMA_VERSION {
            return Err(ReviewError::InvalidStore(format!(
                "unsupported schema_version {}",
                store.schema_version
            )));
        }
        for (block, entry) in &store.entries {
            let (sir, sir_hash) = match entry {
                ReviewEntry::Confirmed { sir, sir_hash, .. }
                | ReviewEntry::Rejected { sir, sir_hash, .. } => (sir, sir_hash),
            };
            if *sir_hash != sir.semantic_hash() {
                return Err(ReviewError::InvalidStore(format!(
                    "block {block}: sir_hash does not match its SIR"
                )));
            }
        }
        Ok(store)
    }

    /// Serialize the store as pretty JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("review store serialization cannot fail")
    }

    /// The latest decision for a block, if any.
    #[must_use]
    pub fn entry(&self, block_id: BlockId) -> Option<&ReviewEntry> {
        self.entries.get(&block_id)
    }

    /// The cached Lean declaration for a block, if any. The caller must still compare its
    /// `cache_key` with the key of the current run.
    #[must_use]
    pub fn cached_lean(&self, block_id: BlockId) -> Option<&CachedLean> {
        self.lean_cache.get(&block_id)
    }

    /// Confirm the SIR of a current report, pinning it until the block's text changes.
    pub fn confirm(&mut self, block: &Block, report: &BlockReport) -> Result<(), ReviewError> {
        let sir = report_sir(block, report)?;
        if report.status == BlockStatus::NotFormalized {
            return Err(ReviewError::NoUsableSir {
                block: block.id,
                errors: report.errors.clone(),
            });
        }
        let entry = ReviewEntry::Confirmed {
            sir: sir.clone(),
            sir_hash: sir.semantic_hash(),
            text_hash: block.text_hash.clone(),
            edited: false,
        };
        self.store(block.id, entry);
        Ok(())
    }

    /// Reject the SIR of a current report. The next generation for the block is told about it.
    pub fn reject(&mut self, block: &Block, report: &BlockReport) -> Result<(), ReviewError> {
        let sir = report_sir(block, report)?;
        let entry = ReviewEntry::Rejected {
            sir: sir.clone(),
            sir_hash: sir.semantic_hash(),
            text_hash: block.text_hash.clone(),
        };
        self.store(block.id, entry);
        Ok(())
    }

    /// Replace the SIR of a block with a hand-written one. The SIR must be valid under the strict
    /// schema and must match the block's kind, label, dependency labels and variable names.
    pub fn edit(
        &mut self,
        block: &Block,
        expected_uses: &[String],
        sir_json: &str,
    ) -> Result<(), ReviewError> {
        let sir = Sir::from_json(sir_json)
            .map_err(|error| ReviewError::InvalidSirJson(error.to_string()))?;
        let problems = validate_sir_identity(block, &sir, expected_uses);
        if !problems.is_empty() {
            return Err(ReviewError::IdentityMismatch(problems));
        }
        let entry = ReviewEntry::Confirmed {
            sir_hash: sir.semantic_hash(),
            sir,
            text_hash: block.text_hash.clone(),
            edited: true,
        };
        self.store(block.id, entry);
        Ok(())
    }

    /// Drop every decision and cached declaration for a block, for example when it was removed.
    pub fn forget(&mut self, block_id: BlockId) {
        self.entries.remove(&block_id);
        self.lean_cache.remove(&block_id);
    }

    /// Cache a Lean declaration accepted for a block's confirmed SIR under its cache key. The
    /// caller passes only declarations the checker accepted in a run with a confirmed SIR.
    pub fn record_accepted_lean(
        &mut self,
        block_id: BlockId,
        cache_key: String,
        declaration: String,
    ) -> Result<(), ReviewError> {
        if !matches!(
            self.entries.get(&block_id),
            Some(ReviewEntry::Confirmed { .. })
        ) {
            return Err(ReviewError::NotConfirmed { block: block_id });
        }
        self.lean_cache.insert(
            block_id,
            CachedLean {
                cache_key,
                declaration,
            },
        );
        Ok(())
    }

    /// Every block with a stored decision or cached declaration, in ID order.
    #[must_use]
    pub fn block_ids(&self) -> Vec<BlockId> {
        self.entries
            .keys()
            .chain(self.lean_cache.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Re-anchor a confirmation after a cosmetic edit. The regenerated SIR has the confirmed
    /// semantic hash, so the decision still applies to the new text (spec section 8, step 6).
    ///
    /// Re-anchoring needs three facts: the report is for this block's current text, its SIR has
    /// the confirmed semantic hash, and the run passed identity validation against the current
    /// document. A report has a cache key only when that validation passed, and the caller runs
    /// this right after the run that produced the report. Returns whether the decision changed.
    pub fn anchor_text(&mut self, block: &Block, report: &BlockReport) -> bool {
        if !report.cosmetic_reconfirmation
            || report.block_id != block.id
            || report.text_hash != block.text_hash
            || report.cache_key.is_none()
        {
            return false;
        }
        let Some(report_sir) = report.sir.as_ref() else {
            return false;
        };
        match self.entries.get_mut(&block.id) {
            Some(ReviewEntry::Confirmed {
                sir_hash,
                text_hash,
                ..
            }) if report_sir.semantic_hash() == *sir_hash && *text_hash != block.text_hash => {
                *text_hash = block.text_hash.clone();
                true
            }
            _ => false,
        }
    }

    fn store(&mut self, block_id: BlockId, entry: ReviewEntry) {
        self.entries.insert(block_id, entry);
        self.lean_cache.remove(&block_id);
    }
}

impl Default for ReviewStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The SIR of a report, after checking that it belongs to this block and this block's text.
fn report_sir<'a>(block: &Block, report: &'a BlockReport) -> Result<&'a Sir, ReviewError> {
    if report.block_id != block.id {
        return Err(ReviewError::ReportForOtherBlock {
            block: block.id,
            report: report.block_id,
        });
    }
    if report.text_hash != block.text_hash {
        return Err(ReviewError::StaleReport { block: block.id });
    }
    report.sir.as_ref().ok_or_else(|| ReviewError::NoUsableSir {
        block: block.id,
        errors: report.errors.clone(),
    })
}

/// Version of the review view written to `OUT.json`.
pub const VIEW_PROTOCOL: u32 = 1;

/// The review view of one document. Clients render it and never recompute statuses.
#[derive(Clone, Debug, Serialize)]
pub struct ReviewView {
    /// Protocol version, always `VIEW_PROTOCOL`.
    pub protocol: u32,
    /// Path of the document the view describes.
    pub document: String,
    /// Blocks per view status. All four keys are always present.
    pub counts: StatusCounts,
    /// Counts by proof result, present after at least one proof attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_counts: Option<ProofStatusCounts>,
    /// Whether any block needs a new run or review, or the last report lists a removed block.
    pub needs_rerun: bool,
    /// The last report's formal blocks in report order, then formal blocks the report does not list.
    pub blocks: Vec<BlockView>,
}

/// Number of confirmed statements in each proof state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ProofStatusCounts {
    /// Proofs accepted by Lean.
    pub verified: usize,
    /// Statements without a checked proof or refutation.
    pub unproven: usize,
    /// Statements with separately checked counterexample evidence.
    pub refuted: usize,
}

/// The view of one formal block.
#[derive(Clone, Debug, Serialize)]
pub struct BlockView {
    /// Persistent block identity.
    pub block_id: BlockId,
    /// Source label, when present.
    pub label: Option<String>,
    /// Source kind, for example `theorem`.
    pub kind: BlockKind,
    /// View status.
    pub status: BlockStatus,
    /// Stored human decision: `none`, `confirmed`, `edited` or `rejected`.
    pub review: &'static str,
    /// Whether the stored decision cannot be reflected without a new run or review.
    pub needs_rerun: bool,
    /// One-line summary for the marker tooltip.
    pub message: String,
    /// The SIR in effect, when there is one.
    pub sir: Option<Sir>,
    /// The Lean statement accepted for the SIR shown, when there is one.
    pub lean_statement: Option<String>,
    /// Structure checks of the last run.
    pub checks: Vec<StructureCheck>,
    /// Errors of the last run.
    pub errors: Vec<String>,
    /// Compact status for a badge, when proof search has run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_status: Option<crate::proof::ProofState>,
    /// Proof state and block-local Lean diagnostics for a confirmed statement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<crate::proof::ProofReport>,
    /// The last run regenerated the confirmed SIR from a cosmetic edit.
    pub cosmetic_reconfirmation: bool,
    /// How a regenerated SIR differs from the confirmed SIR it replaces.
    pub superseded_confirmation: Option<String>,
    /// A reviewer rejected an earlier SIR for this block.
    pub previously_rejected: bool,
}

/// Whether a formal block lacks a persisted identity, so `review-run` must wait for
/// `mls --write-ids`. Generated transient identities appear only in `Analysis::insertions`.
#[must_use]
pub fn lacks_persisted_ids(analysis: &Analysis) -> bool {
    analysis.insertions.iter().any(|insertion| {
        analysis
            .blocks
            .iter()
            .any(|block| block.id == insertion.id && is_formal(block.kind))
    })
}

/// The formal block with `id`, when the document has one.
#[must_use]
pub fn formal_block(analysis: &Analysis, id: BlockId) -> Option<&Block> {
    analysis
        .blocks
        .iter()
        .find(|block| block.id == id && is_formal(block.kind))
}

/// The formal dependencies of `block`, in `block.deps` order.
pub(crate) fn formal_dependencies(
    block: &Block,
    blocks: &HashMap<BlockId, &Block>,
) -> Vec<BlockId> {
    block
        .deps
        .iter()
        .copied()
        .filter(|dependency| {
            blocks
                .get(dependency)
                .is_some_and(|candidate| is_formal(candidate.kind))
        })
        .collect()
}

/// Keeps `statement_confirmed` only where every formal dependency is `statement_confirmed` too.
/// `reports` must be in dependency order. A block that loses the status keeps its confirmation
/// and waits in `sir_drafted`, the same rule the review view applies.
pub(crate) fn settle_dependency_confirmations(
    reports: &mut [BlockReport],
    blocks: &HashMap<BlockId, &Block>,
) {
    let mut confirmed = BTreeSet::new();
    for report in reports.iter_mut() {
        if report.status != BlockStatus::StatementConfirmed {
            continue;
        }
        let ready = blocks.get(&report.block_id).is_some_and(|block| {
            formal_dependencies(block, blocks)
                .iter()
                .all(|dependency| confirmed.contains(dependency))
        });
        if ready {
            confirmed.insert(report.block_id);
        } else {
            report.status = BlockStatus::SirDrafted;
        }
    }
}

/// Build the review view from the analysis of the current document, the last report and the
/// stored decisions. It calls no model and no Lean, and the same inputs always give the same JSON.
///
/// Every formal block is compared with the current analysis, whether or not it has a decision.
/// Blocks are resolved in dependency order, so a block's status depends on the view status of its
/// formal dependencies.
#[must_use]
pub fn review_view(
    document: &str,
    analysis: &Analysis,
    report: &BatchReport,
    store: &ReviewStore,
) -> ReviewView {
    let blocks_by_id: HashMap<BlockId, &Block> = analysis
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();
    let labels: HashMap<BlockId, String> = analysis
        .blocks
        .iter()
        .filter_map(|block| block.label.clone().map(|label| (block.id, label)))
        .collect();
    let mut report_by_id: HashMap<BlockId, &BlockReport> = HashMap::new();
    for block_report in &report.blocks {
        report_by_id
            .entry(block_report.block_id)
            .or_insert(block_report);
    }
    let order = crate::formal_order(analysis).unwrap_or_else(|_| {
        analysis
            .blocks
            .iter()
            .filter(|block| is_formal(block.kind))
            .map(|block| block.id)
            .collect()
    });
    let mut settled: HashMap<BlockId, Settled> = HashMap::new();
    for id in &order {
        let Some(block) = blocks_by_id.get(id).copied() else {
            continue;
        };
        let block_report = report_by_id.get(id).copied();
        let entry = store.entry(*id);
        let env = Env {
            blocks: &blocks_by_id,
            labels: &labels,
            settled: &settled,
        };
        let resolution = resolve(block, block_report, entry, &env);
        let sir_hash = resolution.sir.as_ref().map(Sir::semantic_hash);
        let view = block_view(block, block_report, entry, resolution);
        settled.insert(*id, Settled { view, sir_hash });
    }
    // Report order first, then the formal blocks the report does not list, in dependency order.
    let mut blocks = Vec::new();
    for block_report in &report.blocks {
        if let Some(settled) = settled.remove(&block_report.block_id) {
            blocks.push(settled.view);
        }
    }
    for id in &order {
        if let Some(settled) = settled.remove(id) {
            blocks.push(settled.view);
        }
    }
    let mut counts = StatusCounts::default();
    let mut proof_counts = ProofStatusCounts::default();
    let mut has_proof = false;
    for block in &blocks {
        count_status(&mut counts, block.status);
        if let Some(proof_status) = block.proof_status {
            has_proof = true;
            let slot = match proof_status {
                crate::proof::ProofState::Verified => &mut proof_counts.verified,
                crate::proof::ProofState::Unproven => &mut proof_counts.unproven,
                crate::proof::ProofState::Refuted => &mut proof_counts.refuted,
            };
            *slot += 1;
        }
    }
    let removed = report
        .blocks
        .iter()
        .any(|block| !blocks_by_id.contains_key(&block.block_id));
    let needs_rerun = removed || blocks.iter().any(|block| block.needs_rerun);
    ReviewView {
        protocol: VIEW_PROTOCOL,
        document: document.to_owned(),
        counts,
        proof_counts: has_proof.then_some(proof_counts),
        needs_rerun,
        blocks,
    }
}

fn is_formal(kind: BlockKind) -> bool {
    source_kind(kind).is_some()
}

fn count_status(counts: &mut StatusCounts, status: BlockStatus) {
    let slot = match status {
        BlockStatus::NotFormalized => &mut counts.not_formalized,
        BlockStatus::SirDrafted => &mut counts.sir_drafted,
        BlockStatus::StatementConfirmed => &mut counts.statement_confirmed,
        BlockStatus::FormalizationError => &mut counts.formalization_error,
    };
    *slot += 1;
}

/// A resolved block, plus what the blocks that depend on it need.
struct Settled {
    view: BlockView,
    /// Semantic hash of the SIR the view shows for this block.
    sir_hash: Option<String>,
}

/// What a block's resolution reads from the rest of the document.
struct Env<'a> {
    blocks: &'a HashMap<BlockId, &'a Block>,
    labels: &'a HashMap<BlockId, String>,
    settled: &'a HashMap<BlockId, Settled>,
}

impl Env<'_> {
    /// The label of a block, or its identifier when it has no label.
    fn name(&self, id: BlockId) -> String {
        self.labels
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    /// Whether a formal dependency has a view status of `statement_confirmed`.
    fn confirmed(&self, id: BlockId) -> bool {
        self.settled
            .get(&id)
            .is_some_and(|settled| settled.view.status == BlockStatus::StatementConfirmed)
    }
}

const CHANGED: &str = "block changed since the last run; run review-run";
const STALE_DECISION: &str = "the stored decision does not match the last run; run review-run";
const IDENTITY_CHANGED: &str = "the SIR identity changed since the last run; run review-run";
const KEY_CHANGED: &str =
    "the cache key no longer matches the SIR and its formal dependencies; run review-run";
const UNANCHORED: &str = "the confirmation does not cover the current text; run review-run";
const NOT_IN_RUN: &str = "block is not in the last run; run review-run";

fn block_view(
    block: &Block,
    report: Option<&BlockReport>,
    entry: Option<&ReviewEntry>,
    resolution: Resolution,
) -> BlockView {
    let review = review_label(entry);
    BlockView {
        block_id: block.id,
        label: block.label.clone(),
        kind: block.kind,
        status: resolution.status,
        review,
        needs_rerun: resolution.needs_rerun,
        message: summarize(
            resolution.status,
            resolution.reason.as_deref(),
            report,
            review,
        ),
        sir: resolution.sir,
        lean_statement: resolution.lean_statement,
        checks: report
            .map(|report| report.checks.clone())
            .unwrap_or_default(),
        errors: report
            .map(|report| report.errors.clone())
            .unwrap_or_default(),
        proof_status: report
            .and_then(|report| report.proof.as_ref())
            .map(|proof| crate::proof::ProofState::from(&proof.status)),
        proof: report.and_then(|report| report.proof.clone()),
        cosmetic_reconfirmation: report.is_some_and(|report| report.cosmetic_reconfirmation),
        superseded_confirmation: report.and_then(|report| report.superseded_confirmation.clone()),
        previously_rejected: matches!(entry, Some(ReviewEntry::Rejected { .. })),
    }
}

/// The status of one block against the current analysis, its last report and its decision.
struct Resolution {
    status: BlockStatus,
    sir: Option<Sir>,
    lean_statement: Option<String>,
    needs_rerun: bool,
    reason: Option<String>,
}

fn not_in_run() -> Resolution {
    Resolution {
        status: BlockStatus::NotFormalized,
        sir: None,
        lean_statement: None,
        needs_rerun: true,
        reason: Some(NOT_IN_RUN.into()),
    }
}

/// Resolve one formal block. Every drift from the last run is reported through `needs_rerun`,
/// and a `statement_confirmed` status survives only when nothing has drifted.
fn resolve(
    block: &Block,
    report: Option<&BlockReport>,
    entry: Option<&ReviewEntry>,
    env: &Env<'_>,
) -> Resolution {
    let Some(report) = report else {
        return not_in_run();
    };
    let report_hash = report.sir_hash.as_deref();
    if let Some(ReviewEntry::Rejected { sir_hash, .. }) = entry
        && Some(sir_hash.as_str()) == report_hash
    {
        return Resolution {
            status: BlockStatus::NotFormalized,
            sir: None,
            lean_statement: None,
            needs_rerun: true,
            reason: Some("SIR was rejected; run review-run to draft a replacement".into()),
        };
    }
    let text_drift = report.text_hash != block.text_hash;
    // An edited SIR that no run has seen yet is the SIR in effect.
    let pending_edit = match entry {
        Some(ReviewEntry::Confirmed {
            sir,
            sir_hash,
            edited: true,
            ..
        }) if Some(sir_hash.as_str()) != report_hash => Some(sir),
        _ => None,
    };
    let uses = expected_uses(block, env.labels);
    let shown = pending_edit.or(report.sir.as_ref());
    let problems = shown
        .map(|sir| validate_sir_identity(block, sir, &uses))
        .unwrap_or_default();
    // The run recorded its identity result: a cache key exists only when validation passed, and
    // otherwise its errors hold the problems it found. A different result now is a drift.
    let identity_drift = match shown {
        None => false,
        Some(_) if pending_edit.is_some() => true,
        Some(_) if report.cache_key.is_some() => !problems.is_empty(),
        Some(_) => {
            problems.is_empty()
                || problems
                    .iter()
                    .any(|problem| !report.errors.contains(problem))
        }
    };
    if !problems.is_empty() {
        return Resolution {
            status: BlockStatus::NotFormalized,
            sir: shown.cloned(),
            lean_statement: None,
            needs_rerun: text_drift || identity_drift,
            reason: Some(format!(
                "SIR does not match the block: {}",
                problems.join("; ")
            )),
        };
    }
    if let Some(sir) = pending_edit {
        return Resolution {
            status: BlockStatus::SirDrafted,
            sir: Some(sir.clone()),
            lean_statement: None,
            needs_rerun: true,
            reason: Some("edited SIR has no Lean statement yet; run review-run".into()),
        };
    }
    // A confirmation in effect applies only to the SIR of the last run.
    let confirmed_here = matches!(
        entry,
        Some(ReviewEntry::Confirmed { sir_hash, .. }) if Some(sir_hash.as_str()) == report_hash
    );
    let decision_open = matches!(entry, Some(ReviewEntry::Confirmed { .. })) && !confirmed_here;
    let mut needs_rerun = text_drift || decision_open || identity_drift;
    let mut reason = if text_drift {
        Some(CHANGED)
    } else if identity_drift {
        Some(IDENTITY_CHANGED)
    } else if decision_open {
        Some(STALE_DECISION)
    } else {
        None
    };
    let Some(report_sir) = report.sir.as_ref() else {
        return Resolution {
            status: BlockStatus::NotFormalized,
            sir: None,
            lean_statement: None,
            needs_rerun,
            reason: reason.map(Into::into),
        };
    };

    // The section 4 key must be the one the run computed, from this SIR and the dependency SIRs
    // the view now shows. A missing dependency SIR makes the key unavailable, which is a drift.
    let sir_hash = report_sir.semantic_hash();
    let formal = formal_dependencies(block, env.blocks);
    let dependency_hashes: Option<Vec<(BlockId, String)>> = formal
        .iter()
        .map(|dependency| {
            env.settled
                .get(dependency)
                .and_then(|settled| settled.sir_hash.clone())
                .map(|hash| (*dependency, hash))
        })
        .collect();
    let current_key = dependency_hashes.map(|hashes| cache_key_from_hashes(&sir_hash, &hashes));
    let dependency_drift = report.cache_key.is_some() && current_key != report.cache_key;
    if dependency_drift {
        needs_rerun = true;
        reason.get_or_insert(KEY_CHANGED);
    }

    let anchored = matches!(
        entry,
        Some(ReviewEntry::Confirmed { text_hash, .. }) if *text_hash == block.text_hash
    );
    if confirmed_here && !anchored {
        needs_rerun = true;
        reason.get_or_insert(UNANCHORED);
    }
    let mut status = report.status;
    let confirmed_now = confirmed_here
        && anchored
        && report.accepted_speculatively
        && !text_drift
        && !dependency_drift;
    if confirmed_now {
        // The confirmation is in effect and nothing drifted, so `reason` is still empty here.
        let waiting: Vec<String> = formal
            .iter()
            .filter(|dependency| !env.confirmed(**dependency))
            .map(|dependency| env.name(*dependency))
            .collect();
        if !waiting.is_empty() {
            return Resolution {
                status: BlockStatus::SirDrafted,
                sir: Some(report_sir.clone()),
                lean_statement: report.lean_statement.clone(),
                needs_rerun,
                reason: Some(format!(
                    "confirmed; waiting for dependency confirmation: {}",
                    waiting.join(", ")
                )),
            };
        }
        status = BlockStatus::StatementConfirmed;
    } else if status == BlockStatus::StatementConfirmed {
        status = BlockStatus::SirDrafted;
        needs_rerun = true;
        reason.get_or_insert(STALE_DECISION);
    }
    Resolution {
        status,
        sir: Some(report_sir.clone()),
        lean_statement: report.lean_statement.clone(),
        needs_rerun,
        reason: reason.map(Into::into),
    }
}

/// The `uses` a SIR of `block` must carry: the labels of its dependencies, in dependency order.
fn expected_uses(block: &Block, labels: &HashMap<BlockId, String>) -> Vec<String> {
    block
        .deps
        .iter()
        .filter_map(|dependency| labels.get(dependency).cloned())
        .collect()
}

fn review_label(entry: Option<&ReviewEntry>) -> &'static str {
    match entry {
        None => "none",
        Some(ReviewEntry::Confirmed { edited: false, .. }) => "confirmed",
        Some(ReviewEntry::Confirmed { edited: true, .. }) => "edited",
        Some(ReviewEntry::Rejected { .. }) => "rejected",
    }
}

fn status_name(status: BlockStatus) -> &'static str {
    match status {
        BlockStatus::NotFormalized => "not_formalized",
        BlockStatus::SirDrafted => "sir_drafted",
        BlockStatus::StatementConfirmed => "statement_confirmed",
        BlockStatus::FormalizationError => "formalization_error",
    }
}

/// One deterministic line: the status, then the reason, the first error or the first
/// non-matched structure check, and "confirmed by reviewer" when nothing else applies.
fn summarize(
    status: BlockStatus,
    reason: Option<&str>,
    report: Option<&BlockReport>,
    review: &str,
) -> String {
    let mut parts: Vec<String> = reason.map(str::to_owned).into_iter().collect();
    let detail = report.and_then(|report| {
        report.errors.first().cloned().or_else(|| {
            report
                .checks
                .iter()
                .find(|check| {
                    check.scope == CheckScope::Structure && check.state != CheckState::Matched
                })
                .map(|check| format!("{}: {}", check.field, check.detail))
        })
    });
    match detail {
        Some(detail) => parts.push(detail),
        None if status == BlockStatus::StatementConfirmed => {
            parts.push(format!("{review} by reviewer"));
        }
        None => {}
    }
    let mut message = if parts.is_empty() {
        status_name(status).to_owned()
    } else {
        format!("{}: {}", status_name(status), parts.join("; "))
    };
    if let Some(proof) = report.and_then(|report| report.proof.as_ref()) {
        let proof_status = match proof.status {
            crate::proof::ProofStatus::Verified => "verified",
            crate::proof::ProofStatus::Unproven => "unproven",
            crate::proof::ProofStatus::Refuted { .. } => "refuted",
        };
        message.push_str("; proof ");
        message.push_str(proof_status);
        if let Some(diagnostic) = proof.diagnostics.first() {
            message.push_str(": ");
            message.push_str(&diagnostic.message);
        }
    }
    message
}
