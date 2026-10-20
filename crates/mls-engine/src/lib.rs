//! Batch statement formalization with deterministic validation around model calls.

use mls_core::{Block, BlockId, BlockKind};
use mls_lean::{
    CandidateEnvironment, ExtractedDeclaration, LeanWorker, ReplDiagnostic, StatementRequest,
    WorkerError,
};
use mls_models::{
    CallContext, ChatMessage, GenerationRequest, GenerationStage, ModelClient, ModelError, Usage,
};
use mls_sir::{Sir, StatementKind};
use mls_tm::{Analysis, analyze, parse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};

pub mod decisions;
pub mod review;

pub use review::{
    BlockView, CachedLean, REVIEW_SCHEMA_VERSION, ReviewEntry, ReviewError, ReviewStore,
    ReviewView, VIEW_PROTOCOL, formal_block, lacks_persisted_ids, review_view,
};

const SIR_SCHEMA_VERSION: &str = "mls-sir-v1";
const LEAN_SCHEMA_VERSION: &str = "mls-lean-statement-v1";

/// System prompt for SIR generation. The schema is fixed; this text states its field contract.
const SIR_INSTRUCTIONS: &str = "Extract only the claim stated in the source. Return the strict SIR. assumptions_made is mandatory and lists every silent choice, with unchanged meaning. Preserve the source kind and label exactly. Do not infer expected formal content from tests or metadata. \
Field contract: \
variables are exactly the free parameters of the statement, in order: the objects a theorem universally quantifies over, or the arguments of a defined notion. Never list a variable that the conclusion or a hypothesis binds with ∃ or ∀ itself. Each variable has a semi-formal type. \
hypotheses are only propositional assumptions about the variables, one per item, in order. Never restate a variable's type or domain membership that is already its type, such as 'x is a real number', 'f : ℝ → ℝ' or 'D ⊆ ℝ'. \
conclusion is, for a theorem, the claimed proposition; for a definition, the defining condition or expression of the defined notion, never a sentence that names the notion. Prefer Unicode mathematical notation (∀ ∃ ∧ ∨ ¬ → ↔ ≤ ≥ ≠ ∈ ⊆ |x| ℝ ℕ). Use no TeX commands, and avoid prose when a formula is clear.";

/// Runtime bounds for a batch.
#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    /// Total Lean generation attempts per statement, including the initial attempt.
    pub max_attempts: u32,
    /// Maximum tokens for each SIR response.
    pub sir_max_tokens: u32,
    /// Maximum tokens for each Lean response.
    pub lean_max_tokens: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            sir_max_tokens: 2_048,
            lean_max_tokens: 2_048,
        }
    }
}

/// One deterministic comparison between the SIR and trusted Lean extraction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// The supported structure agrees.
    Matched,
    /// The supported structure disagrees.
    Mismatch,
    /// No sound deterministic comparison is implemented.
    Unchecked,
}

/// Whether a check concerns mechanically comparable structure or semantic content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckScope {
    /// Counts, names, roles, binder info, outer heads, definition bodies and admission status.
    Structure,
    /// Semantic content (types, hypothesis and conclusion text, definition body text). M2 cannot
    /// compare these mechanically, so they are always unchecked and never establish faithfulness.
    Semantics,
}

/// A reviewable structure-check item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructureCheck {
    /// Stable field identifier.
    pub field: String,
    /// Comparison result.
    pub state: CheckState,
    /// Human-readable evidence or limitation.
    pub detail: String,
    /// Whether a mismatch blocks acceptance.
    pub hard: bool,
    /// Whether this check is structural or concerns semantic content.
    pub scope: CheckScope,
}

/// Formalization status of one block (spec section 7).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    /// No usable SIR: generation failed, identities are invalid, a dependency is blocked, or a
    /// rejected SIR was not replaced.
    NotFormalized,
    /// An unconfirmed SIR and an accepted Lean statement.
    SirDrafted,
    /// A human-confirmed SIR in effect, an accepted Lean statement, and every formal dependency
    /// statement confirmed.
    StatementConfirmed,
    /// A SIR exists but no Lean statement was accepted.
    FormalizationError,
}

/// Number of blocks per status, for the document badge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct StatusCounts {
    /// Blocks with no usable SIR.
    pub not_formalized: usize,
    /// Blocks with an unconfirmed SIR and an accepted Lean statement.
    pub sir_drafted: usize,
    /// Blocks with a human-confirmed SIR and an accepted Lean statement.
    pub statement_confirmed: usize,
    /// Blocks with a SIR but no accepted Lean statement.
    pub formalization_error: usize,
}

/// Summarize block statuses for the document badge.
#[must_use]
pub fn badge(report: &BatchReport) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for block in &report.blocks {
        let count = match block.status {
            BlockStatus::NotFormalized => &mut counts.not_formalized,
            BlockStatus::SirDrafted => &mut counts.sir_drafted,
            BlockStatus::StatementConfirmed => &mut counts.statement_confirmed,
            BlockStatus::FormalizationError => &mut counts.formalization_error,
        };
        *count += 1;
    }
    counts
}

/// Per-block result of one pipeline run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockReport {
    /// Stable source identity.
    pub block_id: BlockId,
    /// Source label.
    pub label: Option<String>,
    /// Inclusive byte offset of the original source block.
    pub source_start: usize,
    /// Exclusive byte offset of the original source block.
    pub source_end: usize,
    /// Block text hash this report was produced for. Confirmation refuses a report whose text has
    /// since changed.
    pub text_hash: String,
    /// Formalization status.
    pub status: BlockStatus,
    /// The SIR in effect, when one exists. It can be present for a block whose status is not
    /// formalized, such as one with invalid identity metadata.
    pub sir: Option<Sir>,
    /// Semantic hash of `sir`, when a SIR exists.
    pub sir_hash: Option<String>,
    /// Lean cache key of spec section 4, when a SIR passed its identity checks.
    pub cache_key: Option<String>,
    /// Where `sir` came from: `generated`, `confirmed` or `edited`.
    pub sir_origin: Option<String>,
    /// Where the accepted Lean statement came from: `generated` or `cache`.
    pub lean_origin: Option<String>,
    /// The block text changed but its SIR did not, so the confirmation still applies.
    pub cosmetic_reconfirmation: bool,
    /// How a regenerated SIR differs from the confirmed SIR that it replaces.
    pub superseded_confirmation: Option<String>,
    /// A reviewer rejected an earlier SIR for this block.
    pub previously_rejected: bool,
    /// Candidate Lean declaration, when produced.
    pub lean_statement: Option<String>,
    /// Whether the candidate elaborated.
    pub elaborated: bool,
    /// Whether the candidate was accepted speculatively and no `Structure`-scoped check is
    /// unchecked. `Semantics`-scoped checks are always unchecked in M2 and do not affect this
    /// flag, so it does NOT establish semantic faithfulness.
    pub complete_structure: bool,
    /// Whether the elaborated candidate had no hard structural mismatch.
    pub accepted_speculatively: bool,
    /// Ordered mechanical comparisons, including explicit unchecked fields.
    pub checks: Vec<StructureCheck>,
    /// Attempt count consumed.
    pub attempts: u32,
    /// Bounded error messages from failed stages.
    pub errors: Vec<String>,
    /// Structured Lean errors retained with their original positions.
    pub lean_diagnostics: Vec<LeanDiagnostic>,
}

/// A Lean error associated with the source block being formalized.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LeanDiagnostic {
    /// Stable source block identity.
    pub block_id: BlockId,
    /// Original source block start byte.
    pub source_start: usize,
    /// Original source block end byte.
    pub source_end: usize,
    /// One-based generated Lean line, when supplied by Lean.
    pub line: Option<usize>,
    /// Zero-based generated Lean column, when supplied by Lean.
    pub column: Option<usize>,
    /// Lean's actual diagnostic text.
    pub message: String,
}

/// Whole-document M2 report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchReport {
    /// Parser/linker diagnostics from M1.
    pub diagnostics: Vec<mls_core::Diagnostic>,
    /// Formalizable blocks in dependency order.
    pub blocks: Vec<BlockReport>,
    /// Aggregate provider accounting for every attempted model call.
    pub usage: UsageSummary,
}

/// Aggregate model usage without treating missing provider values as zero.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Attempted model calls, including failures.
    pub calls: u64,
    /// Sum of reported input tokens.
    pub input_tokens: u64,
    /// Sum of reported output tokens.
    pub output_tokens: u64,
    /// Calls whose provider omitted input-token usage.
    pub unknown_input_token_calls: u64,
    /// Calls whose provider omitted output-token usage.
    pub unknown_output_token_calls: u64,
    /// Sum of reported costs.
    pub known_cost: f64,
    /// Calls whose provider omitted cost.
    pub unknown_cost_calls: u64,
}

/// Transactional checking boundary used by the real worker and offline tests.
pub trait StatementChecker {
    /// Check one candidate without accepting its environment.
    fn check(
        &mut self,
        block_id: BlockId,
        name: &str,
        source: &str,
    ) -> Result<ExtractedDeclaration, String>;
    /// Accept or discard the most recent candidate after mechanical checks.
    fn finish(&mut self, accept: bool) -> Result<(), String>;
    /// Return structured diagnostics from the most recent failed check.
    fn take_diagnostics(&mut self) -> Vec<ReplDiagnostic> {
        Vec::new()
    }
}

/// Per-worker transactional adapter. Pending candidates can never cross workers.
pub struct LeanStatementChecker<'a> {
    worker: &'a mut LeanWorker,
    pending: Option<CandidateEnvironment>,
    diagnostics: Vec<ReplDiagnostic>,
}

impl<'a> LeanStatementChecker<'a> {
    /// Wrap one worker for use by the batch pipeline.
    pub fn new(worker: &'a mut LeanWorker) -> Self {
        Self {
            worker,
            pending: None,
            diagnostics: Vec::new(),
        }
    }
}

impl StatementChecker for LeanStatementChecker<'_> {
    fn check(
        &mut self,
        block_id: BlockId,
        name: &str,
        source: &str,
    ) -> Result<ExtractedDeclaration, String> {
        self.pending = None;
        self.diagnostics.clear();
        let checked = self
            .worker
            .check_statement(StatementRequest {
                block_id,
                declaration_name: name,
                source,
            })
            .map_err(|error| {
                if let WorkerError::Diagnostics(diagnostics) = &error {
                    self.diagnostics = diagnostics.clone();
                    diagnostics
                        .iter()
                        .map(format_repl_diagnostic)
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    error.to_string()
                }
            })?;
        let extracted = checked.extracted.clone();
        self.pending = Some(checked.candidate);
        Ok(extracted)
    }

    fn finish(&mut self, accept: bool) -> Result<(), String> {
        let candidate = self.pending.take();
        if accept {
            self.worker
                .accept(candidate.ok_or_else(|| "Lean candidate is missing".to_owned())?)
                .map(|_| ())
                .map_err(|error| error.to_string())
        } else {
            Ok(())
        }
    }
    fn take_diagnostics(&mut self) -> Vec<ReplDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

fn format_repl_diagnostic(diagnostic: &ReplDiagnostic) -> String {
    match diagnostic.pos {
        Some(position) => format!(
            "line {}, column {}: {}",
            position.line, position.column, diagnostic.data
        ),
        None => diagnostic.data.clone(),
    }
}

/// Batch pipeline error before a per-block report can be produced.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// TeXmacs syntax is malformed.
    #[error(transparent)]
    Parse(#[from] mls_tm::ParseError),
    /// Formal block dependencies contain a cycle.
    #[error("formal statement dependency cycle")]
    DependencyCycle,
}

/// Run the M2 pipeline over a TeXmacs document.
pub fn run_batch(
    source: &[u8],
    models: &mut impl ModelClient,
    checker: &mut impl StatementChecker,
    config: PipelineConfig,
) -> Result<BatchReport, PipelineError> {
    let document = parse(source)?;
    let analysis = analyze(&document);
    run_batch_analyzed(source, &analysis, models, checker, config)
}

/// Run the M2 pipeline with the exact analysis used by the caller for IDs and shadow output.
pub fn run_batch_analyzed(
    source: &[u8],
    analysis: &Analysis,
    models: &mut impl ModelClient,
    checker: &mut impl StatementChecker,
    config: PipelineConfig,
) -> Result<BatchReport, PipelineError> {
    run_batch_reviewed(
        source,
        analysis,
        models,
        checker,
        config,
        &ReviewStore::new(),
    )
}

/// Run the pipeline with the human decisions in `review` applied.
///
/// A confirmed SIR whose block text is unchanged is used without a model call. Any other block
/// gets a generated SIR, and a rejected SIR is given to the model as a note. Only a human decision
/// can produce `statement_confirmed`, and only when every formal dependency is `statement_confirmed`
/// too. A confirmed block with an unconfirmed dependency stays `sir_drafted`. Model prompts carry
/// each block's canonical text from `analysis`, so `source` is not read; it is kept so callers
/// that pass the document bytes keep compiling.
pub fn run_batch_reviewed(
    _source: &[u8],
    analysis: &Analysis,
    models: &mut impl ModelClient,
    checker: &mut impl StatementChecker,
    config: PipelineConfig,
    review: &ReviewStore,
) -> Result<BatchReport, PipelineError> {
    let order = formal_order(analysis)?;
    let by_id: HashMap<_, _> = analysis
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();
    let labels: HashMap<_, _> = analysis
        .blocks
        .iter()
        .filter_map(|block| block.label.as_ref().map(|l| (block.id, l.clone())))
        .collect();
    let mut accepted = HashMap::<BlockId, AcceptedDependency>::new();
    let mut reports = Vec::new();
    let mut usage = UsageSummary::default();
    if analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "invalid_id" || diagnostic.code == "duplicate_id")
    {
        for id in order {
            let mut report = empty_report(by_id[&id]);
            report.errors.push(
                "invalid or duplicate block identities must be fixed before formalization".into(),
            );
            reports.push(report);
        }
        return Ok(BatchReport {
            diagnostics: analysis.diagnostics.clone(),
            blocks: reports,
            usage,
        });
    }
    for id in order {
        let block = by_id[&id];
        let mut report = empty_report(block);
        let failed_dependency = block.deps.iter().any(|dependency| {
            by_id
                .get(dependency)
                .is_some_and(|candidate| source_kind(candidate.kind).is_some())
                && !accepted.contains_key(dependency)
        });
        if failed_dependency {
            report
                .errors
                .push("a formal statement dependency was not accepted speculatively".into());
            reports.push(report);
            continue;
        }
        let dependencies: Vec<_> = block
            .deps
            .iter()
            .filter_map(|dep| accepted.get(dep))
            .collect();
        let expected_uses: Vec<_> = block
            .deps
            .iter()
            .filter_map(|dep| labels.get(dep))
            .cloned()
            .collect();
        let dependency_sirs: Vec<Sir> = dependencies
            .iter()
            .map(|dependency| dependency.sir.clone())
            .collect();
        let Some(canonical) = analysis.canonical_text(block.id) else {
            report.errors.push(
                "canonicalization_failed: the block has no canonical text; it was not sent to a model"
                    .into(),
            );
            reports.push(report);
            continue;
        };
        let context = SirContext {
            block,
            canonical,
            review,
            dependencies: &dependency_sirs,
            max_tokens: config.sir_max_tokens,
        };
        let Some((sir, origin)) = decide_sir(models, &context, &mut usage, &mut report) else {
            reports.push(report);
            continue;
        };
        report.sir_origin = Some(origin.as_str().into());
        report.sir_hash = Some(sir.semantic_hash());
        report.sir = Some(sir.clone());
        let identity = validate_sir_identity(block, &sir, &expected_uses);
        if !identity.is_empty() {
            report.errors.extend(identity);
            reports.push(report);
            continue;
        }
        let cache_key = cache_key(&sir, block, &accepted);
        report.cache_key = Some(cache_key.clone());
        let declaration_name = declaration_name(id);
        // A confirmed SIR whose cached declaration still matches its cache key skips generation.
        // The cached check does not consume an attempt.
        let cached = match (origin.is_human_confirmed(), review.cached_lean(id)) {
            (true, Some(cached)) if cached.cache_key == cache_key => {
                check_cached_declaration(checker, id, &declaration_name, &sir, cached)
            }
            _ => None,
        };
        if let Some((declaration, checks)) = cached {
            report.lean_origin = Some("cache".into());
            report.elaborated = true;
            report.complete_structure = is_complete(&checks);
            report.checks = checks;
            report.accepted_speculatively = true;
            report.lean_statement = Some(declaration.clone());
            accepted.insert(
                id,
                AcceptedDependency {
                    name: declaration_name,
                    declaration,
                    sir: sir.clone(),
                },
            );
        } else {
            let mut feedback = Vec::new();
            for attempt in 0..config.max_attempts.max(1) {
                report.attempts += 1;
                report.lean_statement = None;
                report.elaborated = false;
                report.checks.clear();
                report.lean_diagnostics.clear();
                match generate_lean(
                    models,
                    LeanGeneration {
                        block,
                        sir: &sir,
                        name: &declaration_name,
                        attempt,
                        feedback: &feedback,
                        dependencies: &dependencies,
                        max_tokens: config.lean_max_tokens,
                    },
                    &mut usage,
                ) {
                    Err(error) => feedback = vec![error.to_string()],
                    Ok(candidate) => {
                        report.lean_statement = Some(candidate.clone());
                        match checker.check(id, &declaration_name, &candidate) {
                            Err(error) => {
                                report.lean_diagnostics = checker
                                    .take_diagnostics()
                                    .into_iter()
                                    .map(|diagnostic| LeanDiagnostic {
                                        block_id: id,
                                        source_start: block.source_start,
                                        source_end: block.source_end,
                                        line: diagnostic.pos.map(|position| position.line),
                                        column: diagnostic.pos.map(|position| position.column),
                                        message: diagnostic.data,
                                    })
                                    .collect();
                                feedback = vec![format!("Lean: {error}")];
                                feedback.extend(admission_hint(&error));
                            }
                            Ok(extracted) => {
                                report.elaborated = true;
                                report.checks =
                                    compare_structure(&sir, &declaration_name, &extracted);
                                let hard_mismatch = report
                                    .checks
                                    .iter()
                                    .any(|c| c.hard && c.state == CheckState::Mismatch);
                                if let Err(error) = checker.finish(!hard_mismatch) {
                                    feedback = vec![error];
                                    continue;
                                }
                                if !hard_mismatch {
                                    report.accepted_speculatively = true;
                                    report.lean_origin = Some("generated".into());
                                    report.complete_structure = is_complete(&report.checks);
                                    accepted.insert(
                                        id,
                                        AcceptedDependency {
                                            name: declaration_name.clone(),
                                            declaration: candidate,
                                            sir: sir.clone(),
                                        },
                                    );
                                    feedback.clear();
                                    break;
                                }
                                feedback = report
                                    .checks
                                    .iter()
                                    .filter(|c| c.state == CheckState::Mismatch)
                                    .map(|c| format!("{}: {}", c.field, c.detail))
                                    .collect();
                            }
                        }
                    }
                }
            }
            report.errors.extend(feedback);
        }
        report.status = match (report.accepted_speculatively, origin.is_human_confirmed()) {
            (true, true) => BlockStatus::StatementConfirmed,
            (true, false) => BlockStatus::SirDrafted,
            (false, _) => BlockStatus::FormalizationError,
        };
        reports.push(report);
    }
    // A confirmed block is statement_confirmed only once its formal dependencies are too. Reports
    // are in dependency order, so each block sees the final status of its dependencies.
    review::settle_dependency_confirmations(&mut reports, &by_id);
    Ok(BatchReport {
        diagnostics: analysis.diagnostics.clone(),
        blocks: reports,
        usage,
    })
}

impl UsageSummary {
    fn add(&mut self, usage: &Usage) {
        self.calls += 1;
        if let Some(tokens) = usage.input_tokens {
            self.input_tokens += tokens
        } else {
            self.unknown_input_token_calls += 1
        }
        if let Some(tokens) = usage.output_tokens {
            self.output_tokens += tokens
        } else {
            self.unknown_output_token_calls += 1
        }
        if let Some(cost) = usage.cost {
            self.known_cost += cost;
        } else {
            self.unknown_cost_calls += 1;
        }
    }
}

fn empty_report(block: &Block) -> BlockReport {
    BlockReport {
        block_id: block.id,
        label: block.label.clone(),
        source_start: block.source_start,
        source_end: block.source_end,
        text_hash: block.text_hash.clone(),
        status: BlockStatus::NotFormalized,
        sir: None,
        sir_hash: None,
        cache_key: None,
        sir_origin: None,
        lean_origin: None,
        cosmetic_reconfirmation: false,
        superseded_confirmation: None,
        previously_rejected: false,
        lean_statement: None,
        elaborated: false,
        complete_structure: false,
        accepted_speculatively: false,
        checks: Vec::new(),
        attempts: 0,
        errors: Vec::new(),
        lean_diagnostics: Vec::new(),
    }
}

#[derive(Clone)]
struct AcceptedDependency {
    name: String,
    declaration: String,
    sir: Sir,
}

fn formal_order(analysis: &Analysis) -> Result<Vec<BlockId>, PipelineError> {
    let formal: BTreeSet<_> = analysis
        .blocks
        .iter()
        .filter(|b| source_kind(b.kind).is_some())
        .map(|b| b.id)
        .collect();
    let mut remaining: BTreeSet<_> = formal.clone();
    let mut done = BTreeSet::new();
    let mut output = Vec::new();
    while let Some(id) = remaining.iter().copied().find(|id| {
        analysis
            .blocks
            .iter()
            .find(|b| b.id == *id)
            .is_some_and(|b| {
                b.deps
                    .iter()
                    .filter(|d| formal.contains(d))
                    .all(|d| done.contains(d))
            })
    }) {
        remaining.remove(&id);
        done.insert(id);
        output.push(id);
    }
    if remaining.is_empty() {
        Ok(output)
    } else {
        Err(PipelineError::DependencyCycle)
    }
}

fn source_kind(kind: BlockKind) -> Option<StatementKind> {
    match kind {
        BlockKind::Definition => Some(StatementKind::Definition),
        BlockKind::Theorem => Some(StatementKind::Theorem),
        BlockKind::Lemma => Some(StatementKind::Lemma),
        BlockKind::Proposition => Some(StatementKind::Proposition),
        BlockKind::Corollary => Some(StatementKind::Corollary),
        _ => None,
    }
}

fn generate_sir(
    models: &mut impl ModelClient,
    block: &Block,
    text: &str,
    dependencies: &[Sir],
    rejected: Option<&Sir>,
    max_tokens: u32,
    usage: &mut UsageSummary,
) -> Result<Sir, ModelError> {
    let mut user_content = format!(
        "SOURCE:\n{text}\n\nDEPENDENCY SIRS:\n{}",
        serde_json::to_string(dependencies).expect("SIR serialization cannot fail")
    );
    if let Some(rejected) = rejected {
        user_content.push_str(&format!(
            "\n\nREVIEW NOTE: a reviewer rejected the previous SIR for this source. Return a corrected SIR; do not repeat the rejected one.\nREJECTED SIR:\n{}",
            serde_json::to_string(rejected).expect("SIR serialization cannot fail")
        ));
    }
    let request = GenerationRequest {
        context: CallContext {
            block_id: block.id.to_string(),
            attempt: 0,
        },
        stage: GenerationStage::Sir,
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: SIR_INSTRUCTIONS.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user_content,
            },
        ],
        schema_version: SIR_SCHEMA_VERSION.into(),
        schema: sir_schema(),
        max_tokens,
    };
    let reply = generate_accounted(models, &request, usage)?;
    let sir = serde_json::from_value(reply.output).map_err(ModelError::Json)?;
    Ok(sir)
}

/// Where the SIR in effect for a block came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SirOrigin {
    Generated,
    Confirmed,
    Edited,
}

impl SirOrigin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Confirmed => "confirmed",
            Self::Edited => "edited",
        }
    }

    const fn is_human_confirmed(self) -> bool {
        !matches!(self, Self::Generated)
    }
}

/// Inputs for choosing the SIR of one block.
struct SirContext<'a> {
    block: &'a Block,
    /// Layout-independent block text sent to the model; see `Analysis::canonical_text`.
    canonical: &'a [u8],
    review: &'a ReviewStore,
    dependencies: &'a [Sir],
    max_tokens: u32,
}

/// Choose the SIR in effect for a block. A confirmed or edited SIR whose block text is unchanged
/// is used without a model call. Otherwise a SIR is generated, and a rejected SIR is given to the
/// model as a note. Returns `None` when the block has no usable SIR; the reason is in the report.
fn decide_sir(
    models: &mut impl ModelClient,
    context: &SirContext<'_>,
    usage: &mut UsageSummary,
    report: &mut BlockReport,
) -> Option<(Sir, SirOrigin)> {
    let block = context.block;
    let entry = context.review.entry(block.id);
    report.previously_rejected = matches!(entry, Some(ReviewEntry::Rejected { .. }));
    if let Some(ReviewEntry::Confirmed {
        sir,
        text_hash,
        edited,
        ..
    }) = entry
        && *text_hash == block.text_hash
    {
        let origin = if *edited {
            SirOrigin::Edited
        } else {
            SirOrigin::Confirmed
        };
        return Some((sir.clone(), origin));
    }
    let Ok(text) = std::str::from_utf8(context.canonical) else {
        report
            .errors
            .push("block source is not valid UTF-8; it was not sent to a model".into());
        return None;
    };
    let rejected = match entry {
        Some(ReviewEntry::Rejected { sir, .. }) => Some(sir),
        _ => None,
    };
    let generated = match generate_sir(
        models,
        block,
        text,
        context.dependencies,
        rejected,
        context.max_tokens,
        usage,
    ) {
        Ok(sir) => sir,
        Err(error) => {
            report.errors.push(error.to_string());
            return None;
        }
    };
    if let Some(previous) = rejected
        && previous.semantic_hash() == generated.semantic_hash()
    {
        report
            .errors
            .push("the regenerated SIR equals the rejected SIR".into());
        return None;
    }
    if let Some(ReviewEntry::Confirmed {
        sir: confirmed,
        sir_hash,
        ..
    }) = entry
    {
        if generated.semantic_hash() == *sir_hash {
            report.cosmetic_reconfirmation = true;
            return Some((confirmed.clone(), SirOrigin::Confirmed));
        }
        let changed: Vec<_> = confirmed
            .diff(&generated)
            .changes
            .iter()
            .map(|change| change.field)
            .collect();
        report.superseded_confirmation = Some(format!("changed fields: {}", changed.join(", ")));
    }
    Some((generated, SirOrigin::Generated))
}

/// Cache key of spec section 4: this block's SIR hash, then the id and SIR hash of each formal
/// dependency in dependency order, joined by newlines and hashed with SHA-256.
fn cache_key(sir: &Sir, block: &Block, accepted: &HashMap<BlockId, AcceptedDependency>) -> String {
    let dependencies: Vec<(BlockId, String)> = block
        .deps
        .iter()
        .filter_map(|dependency| {
            accepted
                .get(dependency)
                .map(|entry| (*dependency, entry.sir.semantic_hash()))
        })
        .collect();
    cache_key_from_hashes(&sir.semantic_hash(), &dependencies)
}

/// The section 4 cache key from a block's SIR hash and the `(id, SIR hash)` pairs of its formal
/// dependencies, in `block.deps` order. The pipeline and the review view both compute keys here.
pub(crate) fn cache_key_from_hashes(sir_hash: &str, dependencies: &[(BlockId, String)]) -> String {
    let mut lines = vec![sir_hash.to_owned()];
    lines.extend(
        dependencies
            .iter()
            .map(|(dependency, hash)| format!("{dependency} {hash}")),
    );
    mls_core::source_hash(lines.join("\n").as_bytes())
}

/// Check a cached declaration against the current checker. It is accepted only when it elaborates
/// with no hard mismatch. A `None` result leaves the checker with no pending candidate.
fn check_cached_declaration(
    checker: &mut impl StatementChecker,
    id: BlockId,
    name: &str,
    sir: &Sir,
    cached: &CachedLean,
) -> Option<(String, Vec<StructureCheck>)> {
    let extracted = checker.check(id, name, &cached.declaration).ok()?;
    let checks = compare_structure(sir, name, &extracted);
    let hard_mismatch = checks
        .iter()
        .any(|check| check.hard && check.state == CheckState::Mismatch);
    checker.finish(!hard_mismatch).ok()?;
    (!hard_mismatch).then(|| (cached.declaration.clone(), checks))
}

/// Whether no structure-scoped check is unchecked.
fn is_complete(checks: &[StructureCheck]) -> bool {
    !checks
        .iter()
        .any(|check| check.scope == CheckScope::Structure && check.state == CheckState::Unchecked)
}

struct LeanGeneration<'a> {
    block: &'a Block,
    sir: &'a Sir,
    name: &'a str,
    attempt: u32,
    feedback: &'a [String],
    dependencies: &'a [&'a AcceptedDependency],
    max_tokens: u32,
}

fn generate_lean(
    models: &mut impl ModelClient,
    generation: LeanGeneration<'_>,
    usage: &mut UsageSummary,
) -> Result<String, ModelError> {
    let LeanGeneration {
        block,
        sir,
        name,
        attempt,
        feedback,
        dependencies,
        max_tokens,
    } = generation;
    let kind = if sir.kind() == StatementKind::Definition {
        "definition"
    } else {
        "theorem"
    };
    let variables = sir
        .variables()
        .iter()
        .map(|v| v.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let hypothesis_names = (0..sir.hypotheses().len())
        .map(|i| format!("h{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let body_rule = if kind == "definition" {
        "A definition must have its actual pure body and must not contain sorry."
    } else {
        "A theorem body must be exactly `:= by sorry`; do not provide a proof."
    };
    let dependency_context = dependencies
        .iter()
        .map(|dependency| {
            json!({
                "name": dependency.name,
                "declaration": dependency.declaration,
                "sir": dependency.sir,
            })
        })
        .collect::<Vec<_>>();
    let request = GenerationRequest {
        context: CallContext {
            block_id: block.id.to_string(),
            attempt,
        },
        stage: if attempt == 0 {
            GenerationStage::LeanStatement
        } else {
            GenerationStage::LeanRepair
        },
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: format!(
                    "Return one Lean {kind} in lean_statement. Its name must be {name}. Use set_option autoImplicit false semantics. Header binders are all explicit, in this order: one binder per SIR variable [{variables}], named as in the SIR and in order, then one hypothesis binder per SIR hypothesis, named [{hypothesis_names}] and in order. Preserve every valid SIR variable name; do not add implicit or extra binders. Never introduce a hypothesis whose type is True or otherwise vacuous. If a SIR item cannot be formalized faithfully, still write the faithful statement and explain the gap in notes; do not pad the binders to match a count. lean_statement must contain exactly one `theorem` or `def` declaration and nothing else: no `set_option`, `open`, `variable`, `namespace`, `import`, attributes, comments or additional commands. Use fully qualified names (for example `Filter.atTop` or `Set.Icc`) instead of `open`. {body_rule} lean_proof must be null."
                ),
            },
            ChatMessage {
                role: "user".into(),
                content: format!(
                    "SIR:\n{}\n\nACCEPTED DEPENDENCIES (generated name, declaration, SIR):\n{}\n\nPREVIOUS ERRORS:\n{}",
                    serde_json::to_string_pretty(sir).expect("SIR serialization cannot fail"),
                    serde_json::to_string(&dependency_context)
                        .expect("dependency serialization cannot fail"),
                    feedback.join("\n")
                ),
            },
        ],
        schema_version: LEAN_SCHEMA_VERSION.into(),
        schema: lean_schema(),
        max_tokens,
    };
    let reply = generate_accounted(models, &request, usage)?;
    let source = reply
        .output
        .get("lean_statement")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| ModelError::InvalidResponse("lean_statement is missing".into()))?;
    Ok(source)
}

/// The admission contract that the trusted Lean checker enforces on every candidate.
const ADMISSION_CONTRACT: &str = "Admission contract: lean_statement must be exactly one declaration, a theorem body must be exactly `by sorry`, and it must contain no other commands, options, attributes, comments, opens or imports, using only supported pure term syntax.";

/// Deterministic repair hint for an admission rejection. Returns `None` for any other checker error.
/// When the error names an unsupported construct, the hint names it as well.
fn admission_hint(error: &str) -> Option<String> {
    if !error.contains("invalid MLS declaration source") {
        return None;
    }
    let construct = error
        .split_once("unsupported syntax '")
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(construct, _)| construct);
    Some(match construct {
        Some(construct) => format!(
            "{ADMISSION_CONTRACT} The construct '{construct}' is not admitted and must be expressed differently."
        ),
        None => ADMISSION_CONTRACT.to_owned(),
    })
}

fn generate_accounted(
    models: &mut impl ModelClient,
    request: &GenerationRequest,
    usage: &mut UsageSummary,
) -> Result<mls_models::GenerationReply, ModelError> {
    match models.generate(request) {
        Ok(reply) => {
            usage.add(&reply.metadata.usage);
            Ok(reply)
        }
        // A replay miss or a stopped recorder never reached a provider, so it is not a model call.
        Err(error @ (ModelError::ReplayMismatch | ModelError::RecordingStopped)) => Err(error),
        Err(error) => {
            if let ModelError::Provider { failure, .. } = &error
                && let Some(recovered) = &failure.usage
            {
                usage.add(recovered);
            } else {
                usage.add(&Usage::default());
            }
            Err(error)
        }
    }
}

pub(crate) fn validate_sir_identity(block: &Block, sir: &Sir, uses: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    if source_kind(block.kind) != Some(sir.kind()) {
        errors.push("SIR kind differs from tagged source kind".into());
    }
    if block.label.as_deref() != sir.label() {
        errors.push("SIR label differs from source label".into());
    }
    if sir.uses() != uses {
        errors.push("SIR uses differ from deterministically linked dependency labels".into());
    }
    for variable in sir.variables() {
        if !valid_lean_binder_name(&variable.name) {
            errors.push(format!(
                "SIR variable name {:?} cannot be preserved as a generated Lean binder",
                variable.name
            ));
        }
    }
    for index in 0..sir.hypotheses().len() {
        if sir
            .variables()
            .iter()
            .any(|variable| variable.name == format!("h{index}"))
        {
            errors.push(format!(
                "SIR variable h{index} conflicts with the canonical hypothesis binder"
            ));
        }
    }
    errors
}

fn valid_lean_binder_name(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "by",
        "def",
        "definition",
        "do",
        "else",
        "end",
        "example",
        "if",
        "in",
        "let",
        "match",
        "namespace",
        "open",
        "section",
        "structure",
        "then",
        "theorem",
        "where",
    ];
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters
            .all(|character| character.is_alphanumeric() || character == '_' || character == '\'')
        && !KEYWORDS.contains(&name)
}

/// Compare trusted Lean structure with the conservative SIR shape.
#[must_use]
pub fn compare_structure(
    sir: &Sir,
    name: &str,
    actual: &ExtractedDeclaration,
) -> Vec<StructureCheck> {
    let mut checks = Vec::new();
    push_match(
        &mut checks,
        "declaration",
        actual.declaration == name,
        format!("expected {name}, got {}", actual.declaration),
        true,
    );
    let expected_kind = if sir.kind() == StatementKind::Definition {
        "definition"
    } else {
        "theorem"
    };
    push_match(
        &mut checks,
        "kind",
        actual.kind == expected_kind,
        format!("expected {expected_kind}, got {}", actual.kind),
        true,
    );
    let expected_count = sir.variables().len() + sir.hypotheses().len();
    push_match(
        &mut checks,
        "binder_count",
        actual.binders.len() == expected_count,
        format!("expected {expected_count}, got {}", actual.binders.len()),
        true,
    );
    for (index, variable) in sir.variables().iter().enumerate() {
        if let Some(binder) = actual.binders.get(index) {
            push_match(
                &mut checks,
                &format!("variables[{index}].name"),
                binder.name == variable.name,
                format!("expected {}, got {}", variable.name, binder.name),
                true,
            );
            push_match(
                &mut checks,
                &format!("variables[{index}].role"),
                !binder.is_proposition,
                "variable binder must not be proposition-valued".into(),
                true,
            );
            push_match(
                &mut checks,
                &format!("variables[{index}].binder_info"),
                binder.binder_info == "explicit",
                format!("explicit binder required, got {}", binder.binder_info),
                true,
            );
        }
        checks.push(semantic_unchecked(
            format!("variables[{index}].type"),
            "semi-formal type text cannot be mechanically equated with elaborated Lean",
        ));
    }
    for (index, _) in sir.hypotheses().iter().enumerate() {
        if let Some(binder) = actual.binders.get(sir.variables().len() + index) {
            push_match(
                &mut checks,
                &format!("hypotheses[{index}].name"),
                binder.name == format!("h{index}"),
                format!("expected h{index}, got {}", binder.name),
                true,
            );
            push_match(
                &mut checks,
                &format!("hypotheses[{index}].role"),
                binder.is_proposition,
                "hypothesis binder must be proposition-valued".into(),
                true,
            );
            push_match(
                &mut checks,
                &format!("hypotheses[{index}].binder_info"),
                binder.binder_info == "explicit",
                format!("explicit binder required, got {}", binder.binder_info),
                true,
            );
            // A faithfully formalized stated hypothesis is never literally `True`; such a binder
            // only pads the binder count.
            push_match(
                &mut checks,
                &format!("hypotheses[{index}].vacuous"),
                binder.type_text.trim() != "True",
                "hypothesis is the vacuous proposition True".into(),
                true,
            );
        }
        checks.push(semantic_unchecked(
            format!("hypotheses[{index}].content"),
            "semi-formal hypothesis content is not mechanically comparable",
        ));
    }
    if sir.kind() != StatementKind::Definition {
        if let Some(expected) = recognized_head(sir.conclusion()) {
            push_match(
                &mut checks,
                "goal.outer_head",
                heads_equal(expected, &actual.goal_outer_head),
                format!("expected {expected}, got {}", actual.goal_outer_head),
                true,
            );
        } else {
            checks.push(unchecked(
                "goal.outer_head".into(),
                "SIR conclusion has no independently recognized outer head",
            ));
        }
    } else if let Some(expected) = recognized_head(sir.conclusion()) {
        if expected == "equality" {
            // `name args = body` defines `name`; the `=` does not reliably mark the body's head.
            checks.push(unchecked(
                "definition.body_outer_head".into(),
                "`=` in a defining expression is ambiguous between the defined name and the body",
            ));
        } else {
            push_match(
                &mut checks,
                "definition.body_outer_head",
                actual
                    .body_outer_head
                    .as_deref()
                    .is_some_and(|head| heads_equal(expected, head)),
                format!(
                    "expected {expected}, got {}",
                    actual.body_outer_head.as_deref().unwrap_or("missing")
                ),
                true,
            );
        }
    } else {
        checks.push(unchecked(
            "definition.body_outer_head".into(),
            "SIR defining expression has no independently recognized outer head",
        ));
    }
    checks.push(semantic_unchecked(
        "goal.content".into(),
        "semi-formal conclusion content is not a proof of Lean equivalence",
    ));
    if sir.kind() == StatementKind::Definition {
        push_match(
            &mut checks,
            "definition.body",
            actual.body.is_some() && !actual.has_sorry,
            "definition requires an extracted body without sorry".into(),
            true,
        );
        checks.push(semantic_unchecked(
            "definition.body_content".into(),
            "definition body semantics are not mechanically comparable to semi-formal text",
        ));
    } else {
        push_match(
            &mut checks,
            "theorem.admission",
            actual.has_sorry,
            "M2 theorem must use the admitted statement body".into(),
            true,
        );
    }
    checks
}

fn push_match(
    checks: &mut Vec<StructureCheck>,
    field: &str,
    matched: bool,
    detail: String,
    hard: bool,
) {
    checks.push(StructureCheck {
        field: field.into(),
        state: if matched {
            CheckState::Matched
        } else {
            CheckState::Mismatch
        },
        detail,
        hard,
        scope: CheckScope::Structure,
    });
}
/// An unchecked structural item; it blocks `complete_structure`.
fn unchecked(field: String, detail: &str) -> StructureCheck {
    StructureCheck {
        field,
        state: CheckState::Unchecked,
        detail: detail.into(),
        hard: false,
        scope: CheckScope::Structure,
    }
}
/// An unchecked semantic item; it does not affect `complete_structure`.
fn semantic_unchecked(field: String, detail: &str) -> StructureCheck {
    StructureCheck {
        scope: CheckScope::Semantics,
        ..unchecked(field, detail)
    }
}
/// ASCII words of two or more letters admitted in a non-quantified conclusion: mathematical
/// function and operator names. Any other purely alphabetic ASCII word of that length is treated
/// as prose (see `is_prose_word`), so the conclusion is never guessed.
const MATHEMATICAL_WORDS: &[&str] = &[
    "sin", "cos", "tan", "exp", "log", "ln", "abs", "max", "min", "det", "tr", "rank", "dim",
    "ker", "im", "id", "deg", "gcd", "lcm", "sqrt", "inf", "sup",
];

/// ASCII punctuation admitted in semi-formal conclusions, only as needed by the operators below.
/// `:` is admitted for typed binders (`∀ x : ℝ, …`); a top-level `:` is still vetoed by
/// `top_level_operator`.
const ALLOWED_ASCII_SYMBOLS: &[char] = &[
    '(', ')', '[', ']', '{', '}', '+', '-', '*', '/', '^', '_', '.', '\'', ',', '=', '<', '>', '!',
    ':',
];

/// Unicode symbols admitted in semi-formal conclusions. Greek letters are admitted as ranges.
const ALLOWED_UNICODE_SYMBOLS: &[char] = &[
    '≤', '≥', '≠', '∧', '∨', '→', '↔', '∀', '∃', '∈', '∉', '⊆', '⊂', '∪', '∩', '∘', '·', '×', '⁻',
    '¹', '²', '³', 'ℝ', 'ℕ', 'ℤ', 'ℚ', 'ℂ', 'ε', 'δ',
];

/// Words that make a leading `∃` ambiguous about multiplicity (`∃!`, exact or bounded counts).
const EXISTENTIAL_COUNT_WORDS: &[&str] = &[
    "unique",
    "uniquely",
    "exactly",
    "precisely",
    "one",
    "only",
    "no",
    "none",
    "most",
    "least",
];

/// Recognize the outer logical head of semi-formal conclusion text, or return `None` when the
/// head is not in the allowlist or is ambiguous. A `None` is reported as Unchecked, never as a
/// guess. Recognition is allowlist-based: every character must be admitted, and prose quantifier
/// phrases such as "for all" or "there exists" are never recognized.
fn recognized_head(text: &str) -> Option<&'static str> {
    let t = text.trim();
    // `|` is admitted only under a leading quantifier, where the head is the quantifier whatever
    // the body is (for example `∃ M, |u n| ≤ M`). Elsewhere it is an unrecognized form.
    let quantified = t.starts_with(['∀', '∃']);
    if t.contains(['"', '`'])
        || !t
            .chars()
            .all(|character| is_allowed_character(character) || (quantified && character == '|'))
        || !balanced_delimiters(t)
        || has_tag_opening(t)
        // An arrow inside brackets belongs to a nested term, such as a limit subscript.
        || has_nested_arrow(t)
    {
        return None;
    }
    // Lean ends a quantifier body before a second relation or an `↔` at the same level, so such
    // text does not have the quantifier as its outer head.
    let (largest_segment, top_level_iff) = depth_zero_relations(t);
    let single_relation_body = !top_level_iff && largest_segment <= 1;
    // A leading quantifier scopes over the whole text, so the connective veto does not apply.
    // Prose words still veto it, so a trailing clause such as ", and y = 0" is never guessed.
    let prose = ascii_letter_runs(t).any(is_prose_word);
    if let Some(rest) = t.strip_prefix('∃') {
        return (!prose && single_relation_body && existential_prefix_is_plain(rest))
            .then_some("exists");
    }
    if t.starts_with('∀') {
        return (!prose && single_relation_body).then_some("forall");
    }
    if largest_segment > 1 || has_unsupported_form(t) {
        return None;
    }
    let head = top_level_operator(t)?;
    // A top-level `→` between bare terms may be a convergence arrow (`a_n → 0`), which Lean does
    // not elaborate as implication. It is implication only when every side is propositional.
    if head == "implication"
        && !split_top_level_arrows(t)
            .into_iter()
            .all(propositional_operand)
    {
        return None;
    }
    Some(head)
}

/// Relation tokens and connectives that make an operand of `→` certainly propositional.
const PROPOSITIONAL_TOKENS: &[char] = &[
    '=', '<', '>', '≤', '≥', '≠', '∈', '∉', '⊆', '⊂', '∧', '∨', '↔',
];

/// Whether an operand of a top-level `→` chain is certainly a proposition. It must contain, at
/// bracket depth 0, a relation token or `∧ ∨ ↔`, or be a fully parenthesized group whose inside is
/// propositional. An operand that is itself an arrow chain must satisfy this on every side.
fn propositional_operand(text: &str) -> bool {
    let operand = text.trim();
    let sides = split_top_level_arrows(operand);
    if sides.len() > 1 {
        return sides.into_iter().all(propositional_operand);
    }
    if depth_zero_chars(operand).any(|character| PROPOSITIONAL_TOKENS.contains(&character)) {
        return true;
    }
    parenthesized_inside(operand).is_some_and(propositional_operand)
}

/// Splits text at its `→` characters that lie at bracket depth 0.
fn split_top_level_arrows(text: &str) -> Vec<&str> {
    let mut sides = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (offset, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '→' if depth == 0 => {
                sides.push(&text[start..offset]);
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    sides.push(&text[start..]);
    sides
}

/// Characters at bracket depth 0.
fn depth_zero_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut depth = 0usize;
    text.chars().filter(move |character| match character {
        '(' | '[' | '{' => {
            depth += 1;
            false
        }
        ')' | ']' | '}' => {
            depth = depth.saturating_sub(1);
            false
        }
        _ => depth == 0,
    })
}

/// The inside of a group that opens at the first character and closes at the last one.
fn parenthesized_inside(text: &str) -> Option<&str> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    for (offset, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return if offset + character.len_utf8() == text.len() {
                        Some(&text[1..offset])
                    } else {
                        None
                    };
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a `→` occurs inside brackets. Such an arrow is part of a nested term, so the text has no
/// outer head that can be recognized from it.
fn has_nested_arrow(text: &str) -> bool {
    let mut depth = 0usize;
    text.chars().any(|character| match character {
        '(' | '[' | '{' => {
            depth += 1;
            false
        }
        ')' | ']' | '}' => {
            depth = depth.saturating_sub(1);
            false
        }
        '→' => depth > 0,
        _ => false,
    })
}

fn is_allowed_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || character.is_whitespace()
        || ALLOWED_ASCII_SYMBOLS.contains(&character)
        || ALLOWED_UNICODE_SYMBOLS.contains(&character)
        || ('\u{03B1}'..='\u{03C9}').contains(&character)
        || ('\u{0391}'..='\u{03A9}').contains(&character)
}

/// A leading `∃` is recognized only for a plain existential: no `!` directly after it and no
/// multiplicity word anywhere in the text.
fn existential_prefix_is_plain(rest: &str) -> bool {
    !rest.trim_start().starts_with('!')
        && !ascii_words(rest).any(|word| {
            EXISTENTIAL_COUNT_WORDS
                .iter()
                .any(|counted| word.eq_ignore_ascii_case(counted))
        })
}

/// TeXmacs tags such as `<leq>`, `<wedge>` or `</label>` start with `<` followed by a letter,
/// backslash or slash. Such text is markup, not a logical head.
fn has_tag_opening(text: &str) -> bool {
    text.chars()
        .zip(text.chars().skip(1))
        .any(|(current, next)| {
            current == '<' && (next.is_ascii_alphabetic() || next == '/' || next == '\\')
        })
}

/// Whether the text contains a form the outer-head scan cannot classify soundly for an unquantified
/// conclusion: multi-character arrows that the single-character allowlist cannot exclude, and
/// prose words (natural-language connectives, quantifiers, verbs and so on).
fn has_unsupported_form(text: &str) -> bool {
    const ARROWS: &[&str] = &["->", "=>", "<->", "<=>"];
    ARROWS.iter().any(|arrow| text.contains(arrow)) || ascii_letter_runs(text).any(is_prose_word)
}

/// A letters-only ASCII run, at least two long, that is not a mathematical name. Such a run is
/// prose ("No", "At", "and", "if", "forall"), so it vetoes an unquantified conclusion. Runs are
/// taken from `ascii_letter_runs`, so prose glued to a digit ("x=0if P") is still a prose run.
fn is_prose_word(run: &str) -> bool {
    run.len() >= 2
        && !MATHEMATICAL_WORDS
            .iter()
            .any(|name| run.eq_ignore_ascii_case(name))
}

/// Maximal runs of ASCII letters. Digits, underscores, primes and every other character split a
/// run, so identifiers such as `x1`, `a_n` and `f'` are made of single-letter runs only.
fn ascii_letter_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_ascii_alphabetic())
        .filter(|run| !run.is_empty())
}

/// Maximal runs of ASCII letters, digits and underscores.
fn ascii_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
}

/// Scans relation tokens at bracket depth 0. Returns the largest number of relation tokens in one
/// segment, where segments are separated by `, ∧ ∨ → ↔`, and whether a `↔` occurs at depth 0.
/// Relation tokens are `= < > ≤ ≥ ≠ ∈ ∉ ⊆ ⊂`, with `<=`, `>=` and `!=` each counted once. A
/// run of `=` counts once. Relations inside brackets belong to a nested term, not to the chain.
fn depth_zero_relations(text: &str) -> (usize, bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut depth = 0usize;
    let mut current = 0usize;
    let mut largest = 0usize;
    let mut top_level_iff = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            ',' | '∧' | '∨' | '→' | '↔' => {
                top_level_iff |= character == '↔';
                largest = largest.max(current);
                current = 0;
            }
            '<' | '>' | '!' if chars.get(index + 1) == Some(&'=') => {
                current += 1;
                index += 2;
                continue;
            }
            '=' => {
                current += 1;
                while chars.get(index + 1) == Some(&'=') {
                    index += 1;
                }
            }
            '<' | '>' | '≤' | '≥' | '≠' | '∈' | '∉' | '⊆' | '⊂' => current += 1,
            _ => {}
        }
        index += 1;
    }
    (largest.max(current), top_level_iff)
}

fn balanced_delimiters(text: &str) -> bool {
    let mut stack = Vec::new();
    for character in text.chars() {
        match character {
            '(' | '[' | '{' => stack.push(character),
            ')' if stack.pop() != Some('(') => return false,
            ']' if stack.pop() != Some('[') => return false,
            '}' if stack.pop() != Some('{') => return false,
            _ => {}
        }
    }
    stack.is_empty()
}

fn top_level_operator(text: &str) -> Option<&'static str> {
    let mut depth = 0usize;
    let mut found: Option<(&'static str, u8)> = None;
    let mut equality_count = 0usize;
    let mut invalid = false;
    for (offset, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => invalid = true,
            ')' | ']' | '}' => depth -= 1,
            _ if depth != 0 => continue,
            // A top-level comma or semicolon separates clauses; its scope cannot be read safely.
            ':' | ',' | ';' => invalid = true,
            '↔' if surrounded_by_whitespace(text, offset, character.len_utf8()) => {
                record_head(&mut found, "iff", 0);
            }
            '→' if surrounded_by_whitespace(text, offset, character.len_utf8()) => {
                record_head(&mut found, "implication", 1);
            }
            '∨' if surrounded_by_whitespace(text, offset, character.len_utf8()) => {
                record_head(&mut found, "or", 2);
            }
            '∧' if surrounded_by_whitespace(text, offset, character.len_utf8()) => {
                record_head(&mut found, "and", 3);
            }
            // Unspaced connectors are ambiguous with respect to precedence, so they are vetoed.
            '↔' | '→' | '∨' | '∧' => invalid = true,
            // `=` need not be spaced (`T(u+v)=0`), but a neighbour that makes it part of another
            // token (`<=`, `>=`, `!=`, `:=`, `==`) is not an equality.
            '=' => {
                let before = text[..offset].chars().next_back();
                let after = text[offset + character.len_utf8()..].chars().next();
                if !matches!(before, Some('<' | '>' | '!' | ':' | '='))
                    && !matches!(after, Some('<' | '>' | '!' | ':' | '='))
                {
                    equality_count += 1;
                    record_head(&mut found, "equality", 4);
                }
            }
            _ => {}
        }
    }
    if invalid || depth != 0 || found == Some(("equality", 4)) && equality_count != 1 {
        None
    } else {
        found.map(|(head, _)| head)
    }
}

fn surrounded_by_whitespace(text: &str, offset: usize, width: usize) -> bool {
    text[..offset]
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace)
        && text[offset + width..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn record_head(slot: &mut Option<(&'static str, u8)>, head: &'static str, precedence: u8) {
    match slot {
        None => *slot = Some((head, precedence)),
        Some((_, current)) if precedence < *current => *slot = Some((head, precedence)),
        _ => {}
    }
}
fn heads_equal(expected: &str, actual: &str) -> bool {
    matches!(
        (expected, actual),
        ("forall", "forall")
            | ("exists", "exists")
            | ("iff", "iff")
            | ("implication", "forall")
            | ("equality", "eq")
            | ("and", "and")
            | ("or", "or")
    )
}

fn declaration_name(id: BlockId) -> String {
    format!("mls_{}", id.to_string().replace('-', "_"))
}

/// Strict JSON schema sent for SIR generation.
#[must_use]
pub fn sir_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["kind","label","variables","hypotheses","conclusion","uses","assumptions_made"],"properties":{"kind":{"enum":["definition","theorem","lemma","proposition","corollary"]},"label":{"type":["string","null"]},"variables":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["name","type"],"properties":{"name":{"type":"string"},"type":{"type":"string"}}}},"hypotheses":{"type":"array","items":{"type":"string"}},"conclusion":{"type":"string"},"uses":{"type":"array","items":{"type":"string"}},"assumptions_made":{"type":"array","items":{"type":"string"}}}})
}
/// Strict JSON schema sent for Lean statement generation.
#[must_use]
pub fn lean_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["lean_statement","lean_proof","mathlib_names_used","notes"],"properties":{"lean_statement":{"type":"string"},"lean_proof":{"type":"null"},"mathlib_names_used":{"type":"array","items":{"type":"string"}},"notes":{"type":"array","items":{"type":"string"}}}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use mls_lean::Binder;
    use mls_models::{
        CallMetadata, DecisionReply, DecisionRequest, GenerationReply, ProviderFailure,
    };

    fn sir(conclusion: &str) -> Sir {
        Sir::try_new(
            StatementKind::Theorem,
            Some("thm:x".into()),
            vec![mls_sir::Variable {
                name: "x".into(),
                type_description: "real".into(),
            }],
            vec!["x > 0".into()],
            conclusion.into(),
            vec![],
            vec![],
        )
        .unwrap()
    }
    fn extracted(binders: Vec<Binder>, head: &str) -> ExtractedDeclaration {
        ExtractedDeclaration {
            declaration: "mls_x".into(),
            kind: "theorem".into(),
            binders,
            goal: "goal".into(),
            goal_outer_head: head.into(),
            axioms: vec!["sorryAx".into()],
            has_sorry: true,
            body: None,
            body_outer_head: None,
        }
    }

    struct OneReply(Option<Result<GenerationReply, ModelError>>);

    impl ModelClient for OneReply {
        fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
            self.0.take().expect("one model call")
        }

        fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
            unreachable!()
        }
    }

    fn test_block() -> Block {
        Block {
            id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
            kind: BlockKind::Theorem,
            text_hash: String::new(),
            label: Some("thm:x".into()),
            deps: vec![],
            source_start: 10,
            source_end: 20,
        }
    }

    #[test]
    fn successful_call_is_accounted_before_local_sir_decoding() {
        let reply = GenerationReply {
            output: json!({"not": "a SIR"}),
            metadata: CallMetadata {
                http_status: 200,
                configured_model: "test".into(),
                resolved_model: "test-v1".into(),
                request_id: None,
                provider: None,
                latency_ms: 1,
                usage: Usage {
                    input_tokens: Some(7),
                    output_tokens: None,
                    total_tokens: None,
                    cost: Some(0.5),
                },
                provider_response: json!({}),
            },
            finish_reason: "stop".into(),
        };
        let mut model = OneReply(Some(Ok(reply)));
        let mut usage = UsageSummary::default();
        assert!(generate_sir(&mut model, &test_block(), "x", &[], None, 10, &mut usage).is_err());
        assert_eq!(usage.calls, 1);
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.unknown_output_token_calls, 1);
        assert_eq!(usage.known_cost, 0.5);
    }

    #[test]
    fn provider_failure_usage_is_accounted_at_call_boundary() {
        let error = ModelError::Provider {
            message: "bad envelope".into(),
            failure: Box::new(ProviderFailure {
                status: 200,
                response: json!({}),
                resolved_model: Some("test-v1".into()),
                usage: Some(Usage {
                    input_tokens: None,
                    output_tokens: Some(9),
                    total_tokens: None,
                    cost: None,
                }),
            }),
        };
        let mut model = OneReply(Some(Err(error)));
        let mut usage = UsageSummary::default();
        assert!(generate_sir(&mut model, &test_block(), "x", &[], None, 10, &mut usage).is_err());
        assert_eq!(usage.calls, 1);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.unknown_input_token_calls, 1);
        assert_eq!(usage.unknown_cost_calls, 1);
    }
    #[test]
    fn dropped_hypothesis_is_hard_mismatch() {
        let checks = compare_structure(
            &sir("∃ y, y = x"),
            "mls_x",
            &extracted(
                vec![Binder {
                    name: "x".into(),
                    type_text: "Real".into(),
                    is_proposition: false,
                    binder_info: "explicit".into(),
                }],
                "exists",
            ),
        );
        assert!(
            checks
                .iter()
                .any(|c| c.field == "binder_count" && c.state == CheckState::Mismatch && c.hard)
        );
    }
    #[test]
    fn vacuous_true_hypothesis_is_hard_mismatch() {
        let binder = |name: &str, type_text: &str| Binder {
            name: name.into(),
            type_text: type_text.into(),
            is_proposition: name != "x",
            binder_info: "explicit".into(),
        };
        let vacuous = |type_text: &str| {
            compare_structure(
                &sir("∃ y, y = x"),
                "mls_x",
                &extracted(vec![binder("x", "Real"), binder("h0", type_text)], "exists"),
            )
        };
        for type_text in ["True", " True "] {
            let checks = vacuous(type_text);
            assert!(checks.iter().any(|check| {
                check.field == "hypotheses[0].vacuous"
                    && check.state == CheckState::Mismatch
                    && check.hard
                    && check.scope == CheckScope::Structure
                    && check.detail == "hypothesis is the vacuous proposition True"
            }));
        }
        let stated = vacuous("x > 0");
        assert!(stated.iter().any(|check| {
            check.field == "hypotheses[0].vacuous" && check.state == CheckState::Matched
        }));
        assert!(
            !stated
                .iter()
                .any(|check| check.state == CheckState::Mismatch)
        );
    }

    #[test]
    fn universal_for_existential_is_hard_mismatch() {
        let checks = compare_structure(
            &sir("∃ y, y = x"),
            "mls_x",
            &extracted(
                vec![
                    Binder {
                        name: "x".into(),
                        type_text: "Real".into(),
                        is_proposition: false,
                        binder_info: "explicit".into(),
                    },
                    Binder {
                        name: "h0".into(),
                        type_text: "x > 0".into(),
                        is_proposition: true,
                        binder_info: "explicit".into(),
                    },
                ],
                "forall",
            ),
        );
        assert!(
            checks
                .iter()
                .any(|c| c.field == "goal.outer_head" && c.state == CheckState::Mismatch)
        );
    }
    #[test]
    fn semantic_content_stays_unchecked() {
        let checks = compare_structure(
            &sir("x = x"),
            "mls_x",
            &extracted(
                vec![
                    Binder {
                        name: "x".into(),
                        type_text: "Real".into(),
                        is_proposition: false,
                        binder_info: "explicit".into(),
                    },
                    Binder {
                        name: "h0".into(),
                        type_text: "x > 0".into(),
                        is_proposition: true,
                        binder_info: "explicit".into(),
                    },
                ],
                "eq",
            ),
        );
        assert!(
            checks
                .iter()
                .any(|c| c.field == "goal.content" && c.state == CheckState::Unchecked)
        );
    }

    #[test]
    fn binder_info_must_be_explicit() {
        let checks = compare_structure(
            &sir("x = x"),
            "mls_x",
            &extracted(
                vec![
                    Binder {
                        name: "x".into(),
                        type_text: "Real".into(),
                        is_proposition: false,
                        binder_info: "implicit".into(),
                    },
                    Binder {
                        name: "h0".into(),
                        type_text: "x > 0".into(),
                        is_proposition: true,
                        binder_info: "explicit".into(),
                    },
                ],
                "eq",
            ),
        );
        assert!(checks.iter().any(|check| {
            check.field == "variables[0].binder_info"
                && check.state == CheckState::Mismatch
                && check.hard
        }));
    }

    #[test]
    fn definition_shape_uses_body_instead_of_result_type() {
        let definition = Sir::try_new(
            StatementKind::Definition,
            Some("def:x".into()),
            vec![],
            vec![],
            "∃ x, x = x".into(),
            vec![],
            vec![],
        )
        .unwrap();
        let actual = ExtractedDeclaration {
            declaration: "mls_x".into(),
            kind: "definition".into(),
            binders: vec![],
            goal: "Prop".into(),
            goal_outer_head: "sort".into(),
            axioms: vec![],
            has_sorry: false,
            body: Some("Exists fun x => Eq x x".into()),
            body_outer_head: Some("exists".into()),
        };
        let checks = compare_structure(&definition, "mls_x", &actual);
        assert!(checks.iter().any(|check| {
            check.field == "definition.body_outer_head" && check.state == CheckState::Matched
        }));
        assert!(!checks.iter().any(|check| check.field == "goal.outer_head"));
    }

    #[test]
    fn outer_head_parser_ignores_nested_operators_and_respects_precedence() {
        assert_eq!(recognized_head("{v ∈ ℝ² | T v = 0}"), None);
        assert_eq!(recognized_head("x = 0 ∧ y = 0"), Some("and"));
        assert_eq!(recognized_head("F (A → B)"), None);
        assert_eq!(recognized_head("f : ℝ → ℝ is continuous"), None);
        // Bare terms around `→` may be a convergence arrow, so they are not implication.
        assert_eq!(recognized_head("P → Q"), None);
        assert_eq!(recognized_head("x = 0 → y = 0"), Some("implication"));
        assert_eq!(recognized_head("a < b → f a < f b"), Some("implication"));
        assert_eq!(recognized_head("P→Q"), None);
        assert_eq!(recognized_head("if P then Q"), None);
        assert_eq!(recognized_head("x = 0 for some x"), None);
        assert_eq!(recognized_head("x = 0 if P"), None);
        assert_eq!(recognized_head("∀ x, (P x"), None);
        assert_eq!(recognized_head("the string `P → Q`"), None);
        assert_eq!(recognized_head("x = y = z"), None);
        assert!(!heads_equal("or", "forall"));
        assert!(heads_equal("or", "or"));
    }

    #[test]
    fn ambiguous_conclusions_are_never_recognized() {
        for text in [
            // Reproduced false Matched/Mismatch cases.
            "x = 0 and P",
            "x = 0 or P",
            "¬ x = 0",
            "For each x, x = 0",
            "There is x with x = 0",
            "there exists exactly one x with x = 0",
            "there exists no x with x = 0",
            "∃ ! x, x = 0",
            "P <rightarrow> x = 0",
            "x = 0 <wedge> y",
            "x <leq> 0 <wedge> y = 0",
            "P \\to x = 0",
            "x \\le 0 \\land y = 0",
            "“x = 0 → y = 0”",
            // Prose quantifier phrases are never recognized, even when unambiguous.
            "for all x, x = x",
            "for every x, x = x",
            "for all x, P x and Q",
            "there exists x such that x = 0",
            "∃ exactly x, x = 0",
            "∃ x, x = 0 only",
            "∃ x, x = 0 none",
            "∃ x, x = 0 unique",
            "∃ x, x = 0 at most",
            "∃ x, x = 0 at least",
            // Characters outside the allowlist.
            "x : ℝ ⊢ x = 0",
            "P | Q ∧ R",
            "x \\in S",
            "“P” ∧ Q",
            "‘P’ ∧ Q",
            "P ~ Q ∧ R",
            "P ⊕ Q ∧ R",
            "P ∧ Q ✓",
            // Natural-language connectives and quantifiers.
            "x = 0 not P",
            "x = 0 implies P",
            "P iff Q",
            "x = 0 unless P",
            "x = 0 whenever P",
            "x = 0 with P",
            "all x satisfy x = 0",
            "x = 0 holds",
            "both x = 0 and y = 0",
            "either x = 0 or y = 0",
            // Unique and exact quantifiers.
            "∃! x, x = 0",
            "there exists a unique x, x = 0",
            "there exists unique x, x = 0",
            // Logical symbols outside the supported grammar.
            "x = 0 ⇒ P",
            "x = 0 ⟹ P",
            "P ⇔ Q",
            "P ⟺ Q",
            "P ∴ Q",
            "P ∵ Q",
            "P -> Q",
            "P => Q",
            "P <-> Q",
            "!P",
            "P & Q",
            "P | Q",
            "~P",
            // Top-level clause separators.
            "P, Q",
            "x = 0; y = 0",
            // A typed binder with a count word or `!` is not a plain existential.
            "∃ ! x : ℝ, x = 0",
            "∃ x : ℝ, exactly one",
            // A top-level `:` is a type ascription, not a logical head.
            "x : ℝ = 0",
            // Unspaced connectors have ambiguous precedence.
            "P→Q ∧ R",
            "P∧Q",
            // Prose-led text in the non-quantified path (real Lean heads: `¬ ∃…`, `∃`, `∃!`).
            "No real x satisfies x^2 = -1",
            "At least one x satisfies x = 0",
            "Exactly one x satisfies x = 0",
            "Infinitely many n satisfy u n = 0",
            "Let x = 0",
            "ab = ba",
            // Relation chains and `↔` inside a quantifier body end the body in Lean.
            "∀ x : ℝ, p x ↔ P ↔ Q",
            "∃ x : ℝ, p x ↔ P ↔ Q",
            "∀ x : ℝ, x < y = P",
            "∃ x ∈ S, x = y = P",
            // Relation chains without a quantifier.
            "a < b = c",
            "0 ≤ x ≤ 1",
            "∀ x, 0 ≤ x ≤ 1",
            // Convergence arrows between bare terms, which Lean elaborates as applications.
            "a_n → 0",
            "x_n → x",
            "u n → 0",
            "f(x) → L",
            "P → Q",
            "(P) → (x = 0)",
            "x = 0 → y",
            "P ∧ Q → R",
            // An arrow inside brackets belongs to a nested term.
            "lim_{x→0} sin x / x = 1",
            "f (x → y) = z",
            // `lim` is not a recognized mathematical word, so it is prose here.
            "lim a = 0",
            "lim f = L",
            // Prose after a quantifier body is never guessed.
            "∀ x, x = 0, and y = 0",
            "∀ x, P x and Q x",
            // Prose glued to a digit or identifier is still a prose run, so it vetoes the head.
            "x=0if P",
            "x=0forall x",
            "x=1ifandonlyifP",
            "u_n=0forlargen",
            "x=0and y=1",
            "f(x)=0for x",
        ] {
            assert_eq!(recognized_head(text), None, "{text}");
        }
    }

    #[test]
    fn supported_conclusions_keep_their_recognized_head() {
        for (text, head) in [
            ("∀ x, f x = 0", "forall"),
            ("∃ x, x = 0", "exists"),
            ("a = b", "equality"),
            ("f(a, b) = 0", "equality"),
            ("P ∧ Q", "and"),
            ("P ∨ Q", "or"),
            ("x = 0 → y = 0", "implication"),
            ("(x = 0) → (y = 0)", "implication"),
            ("x = 0 ∧ P → Q = 1", "implication"),
            ("P ↔ Q", "iff"),
            ("∀ x, P x → Q x", "forall"),
            ("∀ ε > 0, ∃ δ > 0, δ < ε", "forall"),
            ("∃ x ∈ S, x ≤ 0", "exists"),
            ("x ^ 2 = x * x", "equality"),
            ("x ≤ 0 ∧ y ≥ 0", "and"),
            ("x ≠ 0 ∨ y = 0", "or"),
            ("∀ x : ℝ, f x = 0", "forall"),
            ("∀ ε : ℝ, ε > 0 → ∃ δ : ℝ, δ > 0", "forall"),
            ("∃ n : ℕ, n > 0", "exists"),
            // A nested `:` inside set-builder braces is at depth > 0 and does not veto.
            ("{v ∈ ℝ² : T v = 0} = S", "equality"),
            ("∀ x ∈ S, x ≤ 1", "forall"),
            ("sin x = 0", "equality"),
            ("a_n = 0", "equality"),
            ("x1 = x2", "equality"),
            ("ker T = 0", "equality"),
            ("f' x = 0", "equality"),
            ("x <= 0 ∧ y >= 0", "and"),
            ("x != 0 ∧ y = 0", "and"),
        ] {
            assert_eq!(recognized_head(text), Some(head), "{text}");
        }
    }

    #[test]
    fn absolute_value_is_recognized_only_under_a_leading_quantifier() {
        assert_eq!(
            recognized_head("∃ M : ℝ, ∀ n : ℕ, |u n| ≤ M"),
            Some("exists")
        );
        assert_eq!(
            recognized_head("∃ M ∈ ℝ, ∀ x ∈ D, |f(x)| ≤ M"),
            Some("exists")
        );
        assert_eq!(recognized_head("∀ x, |x| ≥ 0"), Some("forall"));
        assert_eq!(recognized_head("|x| = 1"), None);
        assert_eq!(recognized_head("a | b = c"), None);
    }

    #[test]
    fn admission_hint_states_the_contract_and_names_unsupported_constructs() {
        let rejection = "line 1, column 0: invalid MLS declaration source: <mls-declaration>:3:0: expected end of input";
        assert_eq!(
            admission_hint(rejection),
            Some(ADMISSION_CONTRACT.to_owned())
        );
        let unsupported = "line 1, column 0: invalid MLS declaration source: declaration contains unsupported syntax 'native_decide'";
        assert_eq!(
            admission_hint(unsupported),
            Some(format!(
                "{ADMISSION_CONTRACT} The construct 'native_decide' is not admitted and must be expressed differently."
            ))
        );
        assert_eq!(admission_hint("unknown identifier `MissingName`"), None);
    }

    #[test]
    fn unspaced_equality_is_recognized_but_compound_relations_are_not() {
        assert_eq!(recognized_head("T(u+v)=0"), Some("equality"));
        assert_eq!(recognized_head("f(x)=y"), Some("equality"));
        assert_eq!(recognized_head("x=0"), Some("equality"));
        // `<=`, `>=`, `!=`, `:=` and `==` are single tokens, never an equality.
        for text in ["a<=b", "a>=b", "a!=b", "x:=1", "x==y", "a=<b", "a=!b"] {
            assert_ne!(recognized_head(text), Some("equality"), "{text}");
        }
        assert_eq!(recognized_head("a<=b"), None);
        assert_eq!(recognized_head("x:=1"), None);
        // More than one `=` in a chain is ambiguous, spaced or not.
        assert_eq!(recognized_head("a=b=c"), None);
        assert_eq!(recognized_head("a = b=c"), None);
        assert_eq!(recognized_head("T(u+v)=0 ∧ P"), Some("and"));
    }

    #[test]
    fn definition_equality_head_is_unchecked_not_mismatched() {
        let definition = |conclusion: &str| {
            Sir::try_new(
                StatementKind::Definition,
                Some("def:x".into()),
                vec![],
                vec![],
                conclusion.into(),
                vec![],
                vec![],
            )
            .unwrap()
        };
        let actual = |body_outer_head: &str| ExtractedDeclaration {
            declaration: "mls_x".into(),
            kind: "definition".into(),
            binders: vec![],
            goal: "Type".into(),
            goal_outer_head: "sort".into(),
            axioms: vec![],
            has_sorry: false,
            body: Some("body".into()),
            body_outer_head: Some(body_outer_head.into()),
        };
        for (conclusion, body_head) in [
            ("f(x) = x^2", "app"),
            ("ker T = {v ∈ ℝ² : T v = 0}", "setOf"),
        ] {
            let checks = compare_structure(&definition(conclusion), "mls_x", &actual(body_head));
            let check = checks
                .iter()
                .find(|check| check.field == "definition.body_outer_head")
                .expect("body head is always reported");
            assert_eq!(check.state, CheckState::Unchecked, "{conclusion}");
            assert_eq!(check.scope, CheckScope::Structure, "{conclusion}");
            assert!(!check.hard, "{conclusion}");
            assert!(
                checks
                    .iter()
                    .all(|check| !(check.hard && check.state == CheckState::Mismatch)),
                "{conclusion}"
            );
        }
    }

    #[test]
    fn definition_with_non_equality_head_is_still_compared() {
        let definition = Sir::try_new(
            StatementKind::Definition,
            Some("def:x".into()),
            vec![],
            vec![],
            "∀ x, x = x".into(),
            vec![],
            vec![],
        )
        .unwrap();
        let actual = ExtractedDeclaration {
            declaration: "mls_x".into(),
            kind: "definition".into(),
            binders: vec![],
            goal: "Prop".into(),
            goal_outer_head: "sort".into(),
            axioms: vec![],
            has_sorry: false,
            body: Some("app".into()),
            body_outer_head: Some("app".into()),
        };
        let checks = compare_structure(&definition, "mls_x", &actual);
        assert!(checks.iter().any(|check| {
            check.field == "definition.body_outer_head"
                && check.state == CheckState::Mismatch
                && check.hard
        }));
    }

    #[test]
    fn unsupported_conclusion_is_unchecked_not_matched_or_mismatched() {
        let checks = compare_structure(
            &sir("x = 0 and P"),
            "mls_x",
            &extracted(
                vec![
                    Binder {
                        name: "x".into(),
                        type_text: "Real".into(),
                        is_proposition: false,
                        binder_info: "explicit".into(),
                    },
                    Binder {
                        name: "h0".into(),
                        type_text: "x > 0".into(),
                        is_proposition: true,
                        binder_info: "explicit".into(),
                    },
                ],
                "eq",
            ),
        );
        let head = checks
            .iter()
            .find(|check| check.field == "goal.outer_head")
            .expect("outer head is always reported");
        assert_eq!(head.state, CheckState::Unchecked);
        assert!(!head.hard);
    }

    #[test]
    fn check_scopes_separate_semantics_from_structure() {
        let checks = compare_structure(
            &sir("x = x"),
            "mls_x",
            &extracted(
                vec![
                    Binder {
                        name: "x".into(),
                        type_text: "Real".into(),
                        is_proposition: false,
                        binder_info: "explicit".into(),
                    },
                    Binder {
                        name: "h0".into(),
                        type_text: "x > 0".into(),
                        is_proposition: true,
                        binder_info: "explicit".into(),
                    },
                ],
                "eq",
            ),
        );
        let scope_of = |field: &str| {
            checks
                .iter()
                .find(|check| check.field == field)
                .unwrap_or_else(|| panic!("missing {field}"))
                .scope
        };
        assert_eq!(scope_of("variables[0].type"), CheckScope::Semantics);
        assert_eq!(scope_of("hypotheses[0].content"), CheckScope::Semantics);
        assert_eq!(scope_of("goal.content"), CheckScope::Semantics);
        assert_eq!(scope_of("goal.outer_head"), CheckScope::Structure);
        assert_eq!(scope_of("variables[0].name"), CheckScope::Structure);
    }

    #[test]
    fn replay_mismatch_is_not_counted_as_a_provider_call() {
        let mut model = OneReply(Some(Err(ModelError::ReplayMismatch)));
        let mut usage = UsageSummary::default();
        let error = generate_sir(&mut model, &test_block(), "x", &[], None, 10, &mut usage);
        assert!(matches!(error, Err(ModelError::ReplayMismatch)));
        assert_eq!(usage.calls, 0);
    }

    #[test]
    fn recording_stopped_is_not_counted_as_a_provider_call() {
        let mut model = OneReply(Some(Err(ModelError::RecordingStopped)));
        let mut usage = UsageSummary::default();
        let error = generate_sir(&mut model, &test_block(), "x", &[], None, 10, &mut usage);
        assert!(matches!(error, Err(ModelError::RecordingStopped)));
        assert_eq!(usage.calls, 0);
        assert_eq!(usage.unknown_output_token_calls, 0);
        assert_eq!(usage.unknown_cost_calls, 0);
    }

    /// Fails the test if a model is asked anything.
    struct NoModel;

    impl ModelClient for NoModel {
        fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
            panic!("a block without canonical text reached a model");
        }
        fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
            panic!("a block without canonical text reached a model");
        }
    }

    /// Fails the test if the statement checker is asked anything.
    struct NoChecker;

    impl StatementChecker for NoChecker {
        fn check(&mut self, _: BlockId, _: &str, _: &str) -> Result<ExtractedDeclaration, String> {
            panic!("a block without canonical text reached the checker");
        }
        fn finish(&mut self, _: bool) -> Result<(), String> {
            panic!("a block without canonical text reached the checker");
        }
    }

    #[test]
    fn block_without_canonical_text_is_not_formalized_and_not_sent() {
        let source = "<\\body><\\theorem>x<with|mls-id|0f8fad5b-d9cb-469f-a165-70867728950e|></theorem></body>";
        let analysis = analyze(&parse(source.as_bytes()).unwrap());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "canonicalization_failed")
        );
        let report = run_batch_analyzed(
            source.as_bytes(),
            &analysis,
            &mut NoModel,
            &mut NoChecker,
            PipelineConfig::default(),
        )
        .unwrap();
        let block = &report.blocks[0];
        assert_eq!(block.status, BlockStatus::NotFormalized);
        assert!(
            block
                .errors
                .iter()
                .any(|error| error.starts_with("canonicalization_failed")),
            "{:?}",
            block.errors
        );
    }

    #[test]
    fn formal_blocks_are_topologically_ordered() {
        let first: BlockId = "12b8a5c9-8ede-5e8c-9f31-70822cd8fb1d".parse().unwrap();
        let second: BlockId = "b94dd474-4475-5914-bf73-301208211377".parse().unwrap();
        let block = |id, deps| Block {
            id,
            kind: BlockKind::Theorem,
            text_hash: String::new(),
            label: None,
            deps,
            source_start: 0,
            source_end: 0,
        };
        let analysis = Analysis {
            blocks: vec![block(second, vec![first]), block(first, vec![])],
            diagnostics: vec![],
            insertions: vec![],
            canonical: HashMap::new(),
        };
        assert_eq!(formal_order(&analysis).unwrap(), vec![first, second]);
    }
}
