//! Bounded proof generation and trust-checked proof status.
//!
//! The runner accepts only a [`ConfirmedStatement`], so generated formalizations cannot enter a
//! proof attempt by accident. Provider and Lean integrations implement small traits, keeping this
//! layer usable with online, recording, and offline fixtures.

use mls_core::BlockId;
use mls_models::{CallContext, ChatMessage, GenerationRequest, GenerationStage, ModelClient};
use mls_sir::ProofSir;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

/// Maximum number of proof generation/repair attempts for one confirmed statement.
pub const MAX_PROOF_ATTEMPTS: u32 = 5;
const PROOF_SCHEMA_VERSION: &str = "mls-proof-v1";

/// Human confirmation provenance supplied by the review layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmedStatement {
    /// Stable source block identity.
    block_id: BlockId,
    /// Exact declaration name checked by Lean.
    declaration_name: String,
    /// Accepted Lean statement, without its placeholder proof body.
    lean_statement: String,
    /// Semantic hash of the human-confirmed statement SIR.
    sir_hash: String,
}

impl ConfirmedStatement {
    /// Construct a proof target from the review layer's human-confirmed record.
    pub fn from_human_confirmation(
        block_id: BlockId,
        declaration_name: impl Into<String>,
        lean_statement: impl Into<String>,
        sir_hash: impl Into<String>,
    ) -> Result<Self, String> {
        let value = Self {
            block_id,
            declaration_name: declaration_name.into(),
            lean_statement: lean_statement.into(),
            sir_hash: sir_hash.into(),
        };
        if value.declaration_name.trim().is_empty()
            || value.lean_statement.trim().is_empty()
            || value.sir_hash.trim().is_empty()
        {
            return Err(
                "confirmed statement identity, Lean source, and SIR hash are required".into(),
            );
        }
        Ok(value)
    }

    /// Return the stable source block identity.
    #[must_use]
    pub const fn block_id(&self) -> BlockId {
        self.block_id
    }

    /// Return the exact accepted Lean declaration name.
    #[must_use]
    pub fn declaration_name(&self) -> &str {
        &self.declaration_name
    }

    /// Return the human-confirmed statement source.
    #[must_use]
    pub fn lean_statement(&self) -> &str {
        &self.lean_statement
    }

    /// Return the semantic hash of the confirmed statement SIR.
    #[must_use]
    pub fn sir_hash(&self) -> &str {
        &self.sir_hash
    }
}

/// One proof candidate returned by a generation or repair model call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofCandidate {
    /// Bounded explanatory proof outline.
    pub proof_sir: ProofSir,
    /// Lean tactic proof body, such as `by exact ...`.
    pub lean_proof: String,
}

/// Model boundary for initial proof generation and bounded repairs.
pub trait ProofGenerator {
    /// Generate attempt `attempt`; `previous` and diagnostics are absent on the first attempt.
    fn generate(
        &mut self,
        target: &ConfirmedStatement,
        attempt: u32,
        previous: Option<&ProofCandidate>,
        diagnostics: &[ProofDiagnostic],
    ) -> Result<ProofCandidate, String>;
}

/// Trust facts extracted by Lean from a fully elaborated proof declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanProofCheck {
    /// Whether the complete theorem declaration compiled successfully.
    pub compiled: bool,
    /// Whether the declaration transitively depends on `sorryAx`.
    pub has_sorry: bool,
    /// Transitive axiom names reported by Lean.
    pub axioms: Vec<String>,
}

/// Positive Lean-checked evidence that a statement is false.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanRefutationCheck {
    /// Lean successfully compiled a proof of the statement's negation or a concrete
    /// counterexample satisfying the source assumptions.
    pub positive_evidence: bool,
    /// Whether this evidence transitively depends on `sorryAx`.
    pub has_sorry: bool,
    /// Transitive axiom names used by the evidence.
    pub axioms: Vec<String>,
    /// Human-readable checked Lean evidence, such as the counterexample declaration.
    pub evidence: String,
}

/// Lean checker boundary for proofs and optional explicit refutation evidence.
pub trait ProofChecker {
    /// Elaborate the complete named theorem and inspect its transitive trust dependencies.
    fn check_proof(
        &mut self,
        target: &ConfirmedStatement,
        candidate: &ProofCandidate,
    ) -> Result<LeanProofCheck, String>;

    /// Optionally check a separately constructed negation or counterexample.
    ///
    /// An ordinary failed proof is never refutation evidence. Return `None` unless the checker
    /// positively elaborated a witness against the exact confirmed statement.
    fn check_refutation(
        &mut self,
        _target: &ConfirmedStatement,
    ) -> Result<Option<LeanRefutationCheck>, String> {
        Ok(None)
    }
}

/// Sonnet adapter using the existing model client, including deterministic recording and replay.
pub struct ModelProofGenerator<'a, M> {
    models: &'a mut M,
    max_tokens: u32,
    usage: &'a mut crate::UsageSummary,
}

impl<'a, M> ModelProofGenerator<'a, M> {
    /// Build an adapter over the existing model client and account for each attempted call.
    pub fn new(models: &'a mut M, max_tokens: u32, usage: &'a mut crate::UsageSummary) -> Self {
        Self {
            models,
            max_tokens,
            usage,
        }
    }
}

impl<M: ModelClient> ProofGenerator for ModelProofGenerator<'_, M> {
    fn generate(
        &mut self,
        target: &ConfirmedStatement,
        attempt: u32,
        previous: Option<&ProofCandidate>,
        diagnostics: &[ProofDiagnostic],
    ) -> Result<ProofCandidate, String> {
        let repair = previous.is_some();
        let mut content = format!(
            "Write a Lean proof for this exact human-confirmed declaration. Preserve its statement exactly. Return an object with proof_sir (steps, uses, assumptions_made) and lean_proof. lean_proof must be a complete tactic term beginning with `by`. Use only the imported environment; add no commands, axioms, sorry, or admissions. You may use local `exact?` and `apply?` tactics. Do not invoke network-backed search tactics or tools such as `#loogle` or `#leansearch`.\n\nConfirmed Lean statement:\n{}",
            target.lean_statement
        );
        if let Some(previous) = previous {
            content.push_str("\n\nPrevious proof candidate:\n");
            content.push_str(&serde_json::to_string(previous).unwrap_or_default());
        }
        if !diagnostics.is_empty() {
            content.push_str("\n\nLean diagnostics to repair:\n");
            for diagnostic in diagnostics.iter().rev().take(4).rev() {
                content.push_str("- ");
                content.push_str(&diagnostic.message);
                content.push('\n');
            }
        }
        let request = GenerationRequest {
            context: CallContext {
                block_id: target.block_id.to_string(),
                attempt: attempt.saturating_sub(1),
            },
            stage: if repair {
                GenerationStage::ProofRepair
            } else {
                GenerationStage::Proof
            },
            messages: vec![ChatMessage {
                role: "user".into(),
                content,
            }],
            schema_version: PROOF_SCHEMA_VERSION.into(),
            schema: proof_schema(),
            max_tokens: self.max_tokens,
        };
        let reply = crate::generate_accounted(self.models, &request, self.usage)
            .map_err(|error| error.to_string())?;
        serde_json::from_value(reply.output)
            .map_err(|error| format!("invalid proof output: {error}"))
    }
}

/// Adapt the transactional statement checker to proof checking. The candidate environment is
/// always discarded, so each attempt is checked against the same accepted environment.
pub struct StatementProofChecker<'a, C> {
    checker: &'a mut C,
}

impl<'a, C> StatementProofChecker<'a, C> {
    /// Wrap an existing checker that validates a complete named declaration.
    pub fn new(checker: &'a mut C) -> Self {
        Self { checker }
    }
}

impl<C: crate::StatementChecker> ProofChecker for StatementProofChecker<'_, C> {
    fn check_proof(
        &mut self,
        target: &ConfirmedStatement,
        candidate: &ProofCandidate,
    ) -> Result<LeanProofCheck, String> {
        let (head, _) = target
            .lean_statement
            .rsplit_once(":= by sorry")
            .ok_or_else(|| {
                "accepted theorem statement has no canonical `:= by sorry` placeholder".to_owned()
            })?;
        let accepted_name = format!("theorem {}", target.declaration_name);
        if !head.trim_start().starts_with(&accepted_name) {
            return Err("accepted statement does not begin with its confirmed theorem name".into());
        }
        // The confirmed placeholder is already present in the accepted environment. Check the
        // exact same theorem signature under a private sibling name to avoid redeclaration.
        let proof_name = format!("{}_proof", target.declaration_name);
        let proof_head = head.replacen(&accepted_name, &format!("theorem {proof_name}"), 1);
        let declaration = format!(
            "{}:= {}",
            proof_head.trim_end(),
            candidate.lean_proof.trim()
        );
        let check_result = self
            .checker
            .check_proof(target.block_id, &proof_name, &declaration);
        let finish_result = self.checker.finish(false);
        let extracted = check_result?;
        finish_result?;
        Ok(LeanProofCheck {
            compiled: true,
            has_sorry: extracted.has_sorry,
            axioms: extracted.axioms,
        })
    }
}

fn proof_schema() -> serde_json::Value {
    json!({"type":"object","additionalProperties":false,"required":["proof_sir","lean_proof"],"properties":{"proof_sir":ProofSir::json_schema(),"lean_proof":{"type":"string","minLength":1}}})
}

/// Block-local proof diagnostic suitable for mapping into a document marker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofDiagnostic {
    /// Stable source block identity.
    pub block_id: BlockId,
    /// Stable machine-readable category.
    pub code: String,
    /// Bounded human-readable generation or Lean failure detail.
    pub message: String,
    /// One-based proof generation attempt, if applicable.
    pub attempt: Option<u32>,
}

/// Final proof state for a confirmed statement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProofStatus {
    /// Lean checked the proof with no `sorryAx` or disallowed new axioms.
    Verified,
    /// Bounded proof attempts were exhausted without a verified proof or checked refutation.
    Unproven,
    /// Lean positively checked a negation or counterexample against the confirmed statement.
    Refuted {
        /// Checked Lean evidence for the negation or counterexample.
        evidence: String,
    },
}

/// Compact proof state exposed directly on a reviewed block for status badges.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofState {
    /// A clean full theorem proof compiled in Lean.
    Verified,
    /// Bounded proof search ended without a checked proof or refutation.
    Unproven,
    /// Lean checked positive evidence that the confirmed statement is false.
    Refuted,
}

impl From<&ProofStatus> for ProofState {
    fn from(status: &ProofStatus) -> Self {
        match status {
            ProofStatus::Verified => Self::Verified,
            ProofStatus::Unproven => Self::Unproven,
            ProofStatus::Refuted { .. } => Self::Refuted,
        }
    }
}

/// Results and diagnostics for bounded proof attempts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofReport {
    /// Final status.
    pub status: ProofStatus,
    /// Most recent generated candidate, if any.
    pub candidate: Option<ProofCandidate>,
    /// Number of generation/repair calls consumed.
    pub attempts: u32,
    /// Ordered diagnostics that explain failed attempts.
    pub diagnostics: Vec<ProofDiagnostic>,
}

/// Runtime attempt limit, capped by the hard safety bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProofConfig {
    /// Requested maximum attempts, including initial generation.
    pub max_attempts: u32,
}

impl Default for ProofConfig {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

/// Generate and check a proof for a human-confirmed statement.
pub fn prove<G: ProofGenerator, C: ProofChecker>(
    target: &ConfirmedStatement,
    generator: &mut G,
    checker: &mut C,
    config: ProofConfig,
) -> ProofReport {
    let max_attempts = config.max_attempts.clamp(1, MAX_PROOF_ATTEMPTS);
    let mut diagnostics = Vec::new();
    let mut previous: Option<ProofCandidate> = None;
    let mut attempts = 0;

    while attempts < max_attempts {
        attempts += 1;
        let candidate = match generator.generate(target, attempts, previous.as_ref(), &diagnostics)
        {
            Ok(candidate) => candidate,
            Err(error) => {
                diagnostics.push(diagnostic(
                    target,
                    "proof_generation_failed",
                    error,
                    attempts,
                ));
                continue;
            }
        };
        if candidate.lean_proof.trim().is_empty() {
            diagnostics.push(diagnostic(
                target,
                "proof_candidate_empty",
                "generated Lean proof is empty".into(),
                attempts,
            ));
            previous = Some(candidate);
            continue;
        }
        match checker.check_proof(target, &candidate) {
            Ok(check) if trusted(check.compiled, check.has_sorry, &check.axioms) => {
                return ProofReport {
                    status: ProofStatus::Verified,
                    candidate: Some(candidate),
                    attempts,
                    diagnostics,
                };
            }
            Ok(check) => {
                let message = if !check.compiled {
                    "Lean did not compile the complete theorem declaration".to_owned()
                } else if check.has_sorry {
                    "proof transitively depends on sorryAx".to_owned()
                } else {
                    format!(
                        "proof uses disallowed axioms: {}",
                        disallowed_axioms(&check.axioms).join(", ")
                    )
                };
                diagnostics.push(diagnostic(target, "proof_rejected", message, attempts));
            }
            Err(error) => diagnostics.push(diagnostic(target, "lean_proof_error", error, attempts)),
        }
        previous = Some(candidate);
    }

    let checked_refutation = match checker.check_refutation(target) {
        Ok(refutation) => refutation,
        Err(error) => {
            diagnostics.push(diagnostic(target, "lean_refutation_error", error, attempts));
            None
        }
    };
    let (status, candidate) = match checked_refutation.filter(is_trusted_refutation) {
        Some(refutation) => (
            ProofStatus::Refuted {
                evidence: refutation.evidence,
            },
            previous,
        ),
        None => (ProofStatus::Unproven, previous),
    };
    ProofReport {
        status,
        candidate,
        attempts,
        diagnostics,
    }
}

fn trusted(compiled: bool, has_sorry: bool, axioms: &[String]) -> bool {
    compiled && !has_sorry && disallowed_axioms(axioms).is_empty()
}

fn disallowed_axioms(axioms: &[String]) -> Vec<&str> {
    const ALLOWED_BASELINE: &[&str] = &["propext", "Classical.choice", "Quot.sound"];
    axioms
        .iter()
        .map(String::as_str)
        .filter(|axiom| !ALLOWED_BASELINE.contains(axiom))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_trusted_refutation(check: &LeanRefutationCheck) -> bool {
    check.positive_evidence
        && !check.has_sorry
        && !check.evidence.trim().is_empty()
        && disallowed_axioms(&check.axioms).is_empty()
}

fn diagnostic(
    target: &ConfirmedStatement,
    code: &str,
    message: String,
    attempt: u32,
) -> ProofDiagnostic {
    ProofDiagnostic {
        block_id: target.block_id,
        code: code.into(),
        message: message.chars().take(2_000).collect(),
        attempt: Some(attempt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Generator(VecDeque<Result<ProofCandidate, String>>);

    impl ProofGenerator for Generator {
        fn generate(
            &mut self,
            _target: &ConfirmedStatement,
            _attempt: u32,
            _previous: Option<&ProofCandidate>,
            _diagnostics: &[ProofDiagnostic],
        ) -> Result<ProofCandidate, String> {
            self.0
                .pop_front()
                .unwrap_or_else(|| Err("no fixture candidate".into()))
        }
    }

    #[derive(Default)]
    struct Checker {
        proofs: VecDeque<Result<LeanProofCheck, String>>,
        refutation: Option<LeanRefutationCheck>,
    }

    impl ProofChecker for Checker {
        fn check_proof(
            &mut self,
            _target: &ConfirmedStatement,
            _candidate: &ProofCandidate,
        ) -> Result<LeanProofCheck, String> {
            self.proofs
                .pop_front()
                .unwrap_or_else(|| Err("no fixture check".into()))
        }

        fn check_refutation(
            &mut self,
            _target: &ConfirmedStatement,
        ) -> Result<Option<LeanRefutationCheck>, String> {
            Ok(self.refutation.take())
        }
    }

    fn target() -> ConfirmedStatement {
        let target = ConfirmedStatement::from_human_confirmation(
            BlockId::new(),
            "mls_example",
            "theorem mls_example : True",
            "sir-hash",
        )
        .unwrap();
        assert_eq!(target.declaration_name(), "mls_example");
        assert_eq!(target.lean_statement(), "theorem mls_example : True");
        assert_eq!(target.sir_hash(), "sir-hash");
        let _ = target.block_id();
        target
    }

    fn candidate() -> ProofCandidate {
        ProofCandidate {
            proof_sir: ProofSir::try_new(vec!["apply the definition".into()], vec![], vec![])
                .unwrap(),
            lean_proof: "by trivial".into(),
        }
    }

    #[test]
    fn trusted_lean_proof_is_verified() {
        let mut generator = Generator(VecDeque::from([Ok(candidate())]));
        let mut checker = Checker {
            proofs: VecDeque::from([Ok(LeanProofCheck {
                compiled: true,
                has_sorry: false,
                axioms: vec!["propext".into()],
            })]),
            ..Checker::default()
        };
        let report = prove(
            &target(),
            &mut generator,
            &mut checker,
            ProofConfig { max_attempts: 3 },
        );
        assert_eq!(report.status, ProofStatus::Verified);
        assert_eq!(report.attempts, 1);
    }

    #[test]
    fn exhausted_attempts_are_unproven_without_positive_refutation() {
        let mut generator = Generator(VecDeque::from([Ok(candidate()), Ok(candidate())]));
        let mut checker = Checker {
            proofs: VecDeque::from([
                Ok(LeanProofCheck {
                    compiled: true,
                    has_sorry: true,
                    axioms: vec!["sorryAx".into()],
                }),
                Err("unsolved goals".into()),
            ]),
            ..Checker::default()
        };
        let report = prove(
            &target(),
            &mut generator,
            &mut checker,
            ProofConfig { max_attempts: 2 },
        );
        assert_eq!(report.status, ProofStatus::Unproven);
        assert_eq!(report.attempts, 2);
        assert_eq!(report.diagnostics.len(), 2);
    }

    #[test]
    fn refuted_requires_positive_clean_lean_evidence() {
        let mut generator = Generator(VecDeque::from([Ok(candidate())]));
        let mut checker = Checker {
            proofs: VecDeque::from([Err("unsolved goals".into())]),
            refutation: Some(LeanRefutationCheck {
                positive_evidence: false,
                has_sorry: false,
                axioms: vec![],
                evidence: "ordinary failed proof".into(),
            }),
        };
        let report = prove(
            &target(),
            &mut generator,
            &mut checker,
            ProofConfig { max_attempts: 1 },
        );
        assert_eq!(report.status, ProofStatus::Unproven);

        let mut generator = Generator(VecDeque::from([Ok(candidate())]));
        let mut checker = Checker {
            proofs: VecDeque::from([Err("unsolved goals".into())]),
            refutation: Some(LeanRefutationCheck {
                positive_evidence: true,
                has_sorry: false,
                axioms: vec![],
                evidence: "theorem mls_counterexample : ¬ mls_example := by ...".into(),
            }),
        };
        let report = prove(
            &target(),
            &mut generator,
            &mut checker,
            ProofConfig { max_attempts: 1 },
        );
        assert!(matches!(report.status, ProofStatus::Refuted { .. }));
    }
}
