//! Offline proof generation replay and trust-gate integration.

use mls_core::BlockId;
use mls_engine::proof::{
    ConfirmedStatement, LeanProofCheck, ModelProofGenerator, ProofCandidate, ProofChecker,
    ProofConfig, ProofGenerator, ProofReport, ProofStatus, StatementProofChecker, prove,
};
use mls_engine::{LeanStatementChecker, StatementChecker, UsageSummary};
use mls_lean::{LeanWorker, ReplConfig};
use mls_models::{
    CallMetadata, DecisionReply, DecisionRequest, GenerationReply, GenerationRequest, ModelClient,
    ModelError, ProviderFailure, RecordingClient, ReplayClient, Usage,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

const MODEL: &str = "anthropic/claude-sonnet-5";
const DECISION_MODEL: &str = "typesafe/jev-1.14";

struct FixedModel;

impl ModelClient for FixedModel {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        Ok(GenerationReply {
            output: json!({
                "proof_sir": {"steps":["Use truth introduction"],"uses":[],"assumptions_made":[]},
                "lean_proof": "by trivial"
            }),
            metadata: metadata(),
            finish_reason: "stop".into(),
        })
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        Err(ModelError::InvalidResponse(
            "unexpected decision call".into(),
        ))
    }
}

fn metadata() -> CallMetadata {
    CallMetadata {
        http_status: 200,
        configured_model: MODEL.into(),
        resolved_model: "anthropic/claude-sonnet-5-20261001".into(),
        request_id: Some("proof-fixture".into()),
        provider: Some("offline-test".into()),
        latency_ms: 1,
        usage: Usage {
            input_tokens: Some(12),
            output_tokens: Some(8),
            total_tokens: Some(20),
            cost: Some(0.001),
        },
        provider_response: json!({"fixture": true}),
    }
}

struct LeanFixture(Vec<String>);

impl ProofChecker for LeanFixture {
    fn check_proof(
        &mut self,
        _: &ConfirmedStatement,
        _: &ProofCandidate,
    ) -> Result<LeanProofCheck, String> {
        Ok(LeanProofCheck {
            compiled: true,
            has_sorry: false,
            axioms: self.0.clone(),
        })
    }
}

fn target() -> ConfirmedStatement {
    ConfirmedStatement::from_human_confirmation(
        "00000000-0000-4000-8000-000000000001"
            .parse::<BlockId>()
            .unwrap(),
        "mls_test",
        "theorem mls_test : True := by sorry",
        "confirmed-sir-hash",
    )
    .unwrap()
}

fn run<M: ModelClient>(models: &mut M) -> ProofReport {
    run_accounted(models, vec![]).0
}

fn run_with_axioms<M: ModelClient>(models: &mut M, axioms: Vec<String>) -> ProofReport {
    run_accounted(models, axioms).0
}

fn run_accounted<M: ModelClient>(
    models: &mut M,
    axioms: Vec<String>,
) -> (ProofReport, UsageSummary) {
    let mut usage = UsageSummary::default();
    let mut generator = ModelProofGenerator::new(models, 2048, &mut usage);
    let mut checker = LeanFixture(axioms);
    let report = prove(
        &target(),
        &mut generator,
        &mut checker,
        ProofConfig { max_attempts: 2 },
    );
    (report, usage)
}

#[test]
fn proof_candidate_round_trips_through_recording_and_replay() {
    let directory = std::env::temp_dir().join(format!(
        "mls-proof-replay-{}-{}",
        std::process::id(),
        BlockId::new()
    ));
    let mut recorder = RecordingClient::new(
        FixedModel,
        &directory,
        MODEL,
        DECISION_MODEL,
        "synthetic-proof",
    );
    let recorded = run(&mut recorder);
    assert_eq!(recorded.status, ProofStatus::Verified);
    assert_eq!(recorded.attempts, 1);

    let mut replay = ReplayClient::from_directory(&directory, MODEL, DECISION_MODEL).unwrap();
    let replayed = run(&mut replay);
    assert_eq!(replayed, recorded);
    assert_eq!(
        replayed.candidate.unwrap().proof_sir.steps,
        ["Use truth introduction"]
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn disallowed_axioms_never_verify_a_recorded_proof() {
    let directory = std::env::temp_dir().join(format!(
        "mls-proof-axioms-{}-{}",
        std::process::id(),
        BlockId::new()
    ));
    let mut recorder = RecordingClient::new(
        FixedModel,
        &directory,
        MODEL,
        DECISION_MODEL,
        "synthetic-proof",
    );
    let report = run_with_axioms(&mut recorder, vec!["unsafeCustomAxiom".into()]);
    assert_eq!(report.status, ProofStatus::Unproven);
    assert_eq!(report.diagnostics[0].code, "proof_rejected");
    std::fs::remove_dir_all(directory).unwrap();
}

struct FailingModel(ModelError);

impl ModelClient for FailingModel {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        Err(match &self.0 {
            ModelError::Transport(message) => ModelError::Transport(message.clone()),
            _ => unreachable!(),
        })
    }
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        unreachable!()
    }
}

#[test]
fn failed_model_attempts_are_accounted_but_replay_mismatches_are_not_calls() {
    let mut transport = FailingModel(ModelError::Transport("offline".into()));
    let (report, usage) = run_accounted(&mut transport, vec![]);
    assert_eq!(report.status, ProofStatus::Unproven);
    assert_eq!(usage.calls, 2);
    assert_eq!(usage.unknown_input_token_calls, 2);
    assert_eq!(usage.unknown_output_token_calls, 2);
    assert_eq!(usage.unknown_cost_calls, 2);

    let mut replay = ReplayClient::new(vec![], MODEL, DECISION_MODEL).unwrap();
    let (_, usage) = run_accounted(&mut replay, vec![]);
    assert_eq!(usage.calls, 0);
}

#[test]
fn provider_error_usage_is_retained_for_failed_proof_calls() {
    let mut client = FailingProvider;
    let (_, usage) = run_accounted(&mut client, vec![]);
    assert_eq!(usage.calls, 2);
    assert_eq!(usage.input_tokens, 6);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.unknown_cost_calls, 2);
}

struct FailingProvider;

impl ModelClient for FailingProvider {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        Err(ModelError::Provider {
            message: "provider failed".into(),
            failure: Box::new(ProviderFailure {
                status: 500,
                response: json!({"error":"synthetic"}),
                resolved_model: None,
                usage: Some(Usage {
                    input_tokens: Some(3),
                    output_tokens: Some(0),
                    total_tokens: Some(3),
                    cost: None,
                }),
            }),
        })
    }
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        unreachable!()
    }
}

struct FixedProof;

impl ProofGenerator for FixedProof {
    fn generate(
        &mut self,
        _: &ConfirmedStatement,
        _: u32,
        _: Option<&ProofCandidate>,
        _: &[mls_engine::proof::ProofDiagnostic],
    ) -> Result<ProofCandidate, String> {
        Ok(ProofCandidate {
            proof_sir: mls_sir::ProofSir::try_new(
                vec!["Truth introduction".into()],
                vec![],
                vec![],
            )
            .unwrap(),
            lean_proof: "by exact?".into(),
        })
    }
}

#[test]
fn confirmed_statement_reaches_verified_status_with_real_lean_when_enabled() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local Lean runtime");
        return;
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .to_path_buf();
    let mut worker = LeanWorker::spawn(ReplConfig {
        program: repository.join("scripts/run-lean-repl.sh"),
        args: vec![],
        cwd: repository.join(".mls"),
        env: vec![],
        timeout: Duration::from_secs(180),
        max_stdout_bytes: 1024 * 1024,
        max_stderr_bytes: 64 * 1024,
    })
    .expect("pinned Lean runtime should start");
    let mut statement_checker = LeanStatementChecker::new(&mut worker);
    let confirmed = target();
    statement_checker
        .check(
            confirmed.block_id(),
            confirmed.declaration_name(),
            confirmed.lean_statement(),
        )
        .expect("confirmed placeholder should already be in the accepted environment");
    statement_checker.finish(true).unwrap();
    let mut checker = StatementProofChecker::new(&mut statement_checker);
    let mut generator = FixedProof;
    let report = prove(
        &confirmed,
        &mut generator,
        &mut checker,
        ProofConfig { max_attempts: 1 },
    );
    assert_eq!(report.status, ProofStatus::Verified, "{report:?}");
}
