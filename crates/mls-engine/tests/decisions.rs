//! Offline acceptance tests for the M4 Jev decision layer.
//!
//! Decisions replay synthetic recordings made through `RecordingClient` around a scripted inner
//! client. Run `cargo test -p mls-engine --test decisions -- --ignored regenerate` to rewrite them.

mod common;

use common::*;
use mls_engine::decisions::{
    Basis, Decider, DecisionConfig, DecisionLayer, DiffersFlag, EditKind, Evidence, EvidenceAnswer,
    FallbackReason, FieldFlag, Finished, FlagReason, ModelDecider, ProseKind, TRUNCATION_MARKER,
};
use mls_models::{
    Answer, CallMetadata, ChoiceAnswer, DecisionReply, DecisionRequest, GenerationReply,
    GenerationRequest, ModelClient, ModelError, NoulAnswer, NoulAssessment, ProviderFailure,
    RecordingClient, ReplayClient, Usage,
};
use mls_sir::{Sir, StatementKind, Variable};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;

const BLOCK_ID: &str = "00000000-0000-4000-8000-000000000001";
const SERVED_MODEL: &str = "typesafe/jev-1.14-20261014";
const CLAIM_TEXT: &str = "Every bounded monotone real sequence converges.";

/// The layer as the server builds it, recording through a scripted inner client.
type Recorded = DecisionLayer<ModelDecider<RecordingClient<Scripted>>>;

fn decision_fixture_dir(name: &str) -> PathBuf {
    fixture_root().join("recordings/decisions").join(name)
}

fn replay_layer(name: &str) -> DecisionLayer<ModelDecider<ReplayClient>> {
    let client =
        ReplayClient::from_directory(decision_fixture_dir(name), GENERATION_MODEL, DECISION_MODEL)
            .unwrap();
    DecisionLayer::new(
        ModelDecider::new(client, DECISION_MODEL),
        DecisionConfig::default(),
    )
    .unwrap()
}

/// Inner client that answers decisions from a fixed script, in order.
struct Scripted {
    replies: VecDeque<Result<DecisionReply, ModelError>>,
}

impl ModelClient for Scripted {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        unreachable!("decisions never request generation")
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        self.replies.pop_front().expect("scripted decision reply")
    }
}

/// Decider that answers from a fixed script and never consults a model.
struct Fixed {
    replies: VecDeque<Result<DecisionReply, ModelError>>,
}

impl Decider for Fixed {
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        self.replies.pop_front().expect("scripted decision reply")
    }
}

/// Decider for inputs that must never reach Jev.
struct NeverCalled;

impl Decider for NeverCalled {
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        panic!("this input must not be sent to Jev")
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: Some(120),
        output_tokens: Some(1),
        total_tokens: Some(121),
        cost: Some(0.25),
    }
}

fn metadata(usage: Usage) -> CallMetadata {
    CallMetadata {
        http_status: 200,
        configured_model: DECISION_MODEL.into(),
        resolved_model: SERVED_MODEL.into(),
        request_id: None,
        provider: None,
        latency_ms: 0,
        usage,
        provider_response: json!({"synthetic": true}),
    }
}

fn answered(answers: Vec<(&str, Answer)>) -> Result<DecisionReply, ModelError> {
    Ok(DecisionReply {
        answers: answers
            .into_iter()
            .map(|(id, answer)| (id.to_owned(), answer))
            .collect(),
        metadata: metadata(usage()),
    })
}

fn choice(label: &str, distribution: &[(&str, f64)], confidence: f64) -> Answer {
    Answer::Choice(ChoiceAnswer {
        choice: label.into(),
        probabilities: Some(
            distribution
                .iter()
                .map(|(option, p)| ((*option).to_owned(), *p))
                .collect(),
        ),
        confidence: Some(confidence),
    })
}

fn choice_without_evidence(label: &str) -> Answer {
    Answer::Choice(ChoiceAnswer {
        choice: label.into(),
        probabilities: None,
        confidence: None,
    })
}

fn yes_no(probability: f64) -> Answer {
    Answer::Noul(NoulAnswer { noul: probability })
}

fn make_sir(hypotheses: &[&str], conclusion: &str) -> Sir {
    Sir::try_new(
        StatementKind::Theorem,
        Some("thm:bounded".into()),
        vec![Variable {
            name: "f".into(),
            type_description: "function ℝ → ℝ".into(),
        }],
        hypotheses.iter().map(|h| (*h).to_owned()).collect(),
        conclusion.into(),
        Vec::new(),
        vec!["interval endpoints follow the text".into()],
    )
    .unwrap()
}

fn record(
    name: &str,
    replies: Vec<Result<DecisionReply, ModelError>>,
    run: impl FnOnce(&mut Recorded),
) {
    let directory = decision_fixture_dir(name);
    let _ = std::fs::remove_dir_all(&directory);
    let recorder = RecordingClient::new(
        Scripted {
            replies: replies.into(),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    let mut layer = DecisionLayer::new(
        ModelDecider::new(recorder, DECISION_MODEL),
        DecisionConfig::default(),
    )
    .unwrap();
    run(&mut layer);
}

fn assert_usage(usage: &mls_engine::UsageSummary, calls: u64, input_tokens: u64, known_cost: f64) {
    assert_eq!(usage.calls, calls, "calls");
    assert_eq!(usage.input_tokens, input_tokens, "input tokens");
    assert!((usage.known_cost - known_cost).abs() < 1e-12, "known cost");
}

fn flag_summary(flags: &[FieldFlag]) -> Vec<(&str, FlagReason)> {
    flags
        .iter()
        .map(|flag| (flag.field.as_str(), flag.reason))
        .collect()
}

fn run_classify_high<D: Decider>(layer: &mut DecisionLayer<D>) {
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.value, ProseKind::Claim);
    assert_eq!(outcome.basis, Basis::Jev);
    assert!(!outcome.truncated);
    assert_eq!(
        outcome.evidence,
        Some(Evidence {
            model: SERVED_MODEL.into(),
            threshold: 0.9,
            answer: EvidenceAnswer::Choice {
                choice: "claim".into(),
                probabilities: Some(
                    [("claim", 0.96), ("proof_step", 0.02), ("commentary", 0.02)]
                        .map(|(k, v)| (k.to_owned(), v))
                        .into()
                ),
                confidence: Some(0.95),
            },
        })
    );
    assert_usage(layer.usage(), 1, 120, 0.25);
}

#[test]
fn classify_high_confidence_claim_is_used() {
    run_classify_high(&mut replay_layer("classify-high"));
}

fn run_classify_low<D: Decider>(layer: &mut DecisionLayer<D>) {
    let outcome = layer.classify_prose(BLOCK_ID, "Hence the claim follows.");
    assert_eq!(outcome.value, ProseKind::Commentary);
    assert_eq!(
        outcome.basis,
        Basis::Fallback(FallbackReason::LowConfidence)
    );
    assert!(
        outcome.evidence.is_some(),
        "raw evidence is kept on fallback"
    );
    assert_usage(layer.usage(), 1, 120, 0.25);
}

#[test]
fn classify_low_confidence_falls_back_to_commentary() {
    run_classify_low(&mut replay_layer("classify-low"));
}

fn run_classify_malformed<D: Decider>(layer: &mut DecisionLayer<D>) {
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.value, ProseKind::Commentary);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Malformed));
    let evidence = outcome.evidence.expect("the answer arrived, so it is kept");
    assert!(matches!(
        evidence.answer,
        EvidenceAnswer::Choice {
            probabilities: None,
            confidence: None,
            ..
        }
    ));
    assert_usage(layer.usage(), 1, 120, 0.25);
}

#[test]
fn classify_missing_probabilities_falls_back_as_malformed() {
    run_classify_malformed(&mut replay_layer("classify-malformed"));
}

fn run_classify_failure<D: Decider>(layer: &mut DecisionLayer<D>) {
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.value, ProseKind::Commentary);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Failed));
    assert!(outcome.evidence.is_none());
    let usage = layer.usage();
    assert_eq!(usage.calls, 1, "a failed call still reached the provider");
    assert_eq!(usage.unknown_input_token_calls, 1);
    assert_eq!(usage.unknown_cost_calls, 1, "unknown cost is not zero");
    assert_eq!(usage.known_cost, 0.0);
}

#[test]
fn classify_transport_failure_counts_unknown_usage() {
    run_classify_failure(&mut replay_layer("classify-failure"));
}

fn run_finished<D: Decider>(layer: &mut DecisionLayer<D>) {
    assert_eq!(
        layer
            .is_finished(
                BLOCK_ID,
                "Every bounded sequence has a convergent subsequence."
            )
            .value,
        Finished::Yes
    );
    let uncertain = layer.is_finished(BLOCK_ID, "Every bounded sequence has");
    assert_eq!(uncertain.value, Finished::Unknown);
    assert_eq!(
        uncertain.basis,
        Basis::Fallback(FallbackReason::LowConfidence)
    );
    assert_eq!(layer.is_finished(BLOCK_ID, "Let x").value, Finished::No);
    assert_usage(layer.usage(), 3, 360, 0.75);
}

#[test]
fn finished_gate_answers_yes_no_and_waits_when_unsure() {
    run_finished(&mut replay_layer("finished"));
}

#[test]
fn finished_oracle_adapter_uses_answer_confidence_and_conservative_fallbacks() {
    use mls_engine::live::{Finished as LiveFinished, FinishedOracle, FinishedQuery, OracleAnswer};
    let mut layer = DecisionLayer::new(
        Fixed {
            replies: VecDeque::from([
                answered(vec![("answer", yes_no(0.97))]),
                answered(vec![("answer", yes_no(0.02))]),
                answered(vec![("answer", yes_no(0.5))]),
                Err(ModelError::Transport("offline".into())),
                answered(vec![("answer", choice_without_evidence("yes"))]),
            ]),
        },
        DecisionConfig::default(),
    )
    .unwrap();
    let query = FinishedQuery {
        block: BLOCK_ID.parse().unwrap(),
        text: "A complete claim.",
    };
    let yes = FinishedOracle::is_finished(&mut layer, &query);
    assert_eq!(yes.finished, LiveFinished::Yes);
    assert!((yes.confidence - 0.97).abs() < 1e-12);
    let no = FinishedOracle::is_finished(&mut layer, &query);
    assert_eq!(no.finished, LiveFinished::No);
    assert!(
        (no.confidence - 0.98).abs() < 1e-12,
        "confidence of No is 1-p"
    );
    for _ in 0..3 {
        assert_eq!(
            FinishedOracle::is_finished(&mut layer, &query),
            OracleAnswer::UNKNOWN
        );
    }
    let long = "x".repeat(4_001);
    assert_eq!(
        FinishedOracle::is_finished(
            &mut layer,
            &FinishedQuery {
                text: &long,
                ..query
            }
        ),
        OracleAnswer::UNKNOWN
    );
    assert_eq!(layer.usage().calls, 5);
    assert_eq!(layer.usage().input_tokens, 480);
    assert_eq!(layer.usage().unknown_cost_calls, 1);
}

fn run_semantic_edit<D: Decider>(layer: &mut DecisionLayer<D>) {
    let cosmetic = layer.is_semantic_edit(BLOCK_ID, "a ≤ b", "a ≤ b ");
    assert_eq!(cosmetic.value, EditKind::Cosmetic);
    assert_eq!(cosmetic.basis, Basis::Jev);
    assert_eq!(
        layer.is_semantic_edit(BLOCK_ID, "a ≤ b", "a < b").value,
        EditKind::Semantic
    );
    let unsure = layer.is_semantic_edit(BLOCK_ID, "a ≤ b", "a ≤ c");
    assert_eq!(unsure.value, EditKind::Semantic);
    assert_eq!(unsure.basis, Basis::Fallback(FallbackReason::LowConfidence));
    // Cut text can hide the very change that matters, so it is never called cosmetic.
    let long = "x ".repeat(3_000);
    let cut = layer.is_semantic_edit(BLOCK_ID, &long, &long);
    assert_eq!(cut.value, EditKind::Semantic);
    assert_eq!(cut.basis, Basis::Fallback(FallbackReason::Oversized));
    assert!(cut.truncated);
    assert_usage(layer.usage(), 3, 360, 0.75);
}

#[test]
fn semantic_edit_is_cosmetic_only_when_confident_and_never_for_cut_text() {
    run_semantic_edit(&mut replay_layer("semantic-edit"));
}

fn run_link<D: Decider>(layer: &mut DecisionLayer<D>) {
    let labels = ["thm:first".to_owned(), "lem:bound".to_owned()];
    let linked = layer.link_implicit(BLOCK_ID, "By the previous lemma, the bound holds.", &labels);
    assert_eq!(linked.value.as_deref(), Some("lem:bound"));
    assert_eq!(linked.basis, Basis::Jev);

    let none = layer.link_implicit(BLOCK_ID, "Let g be bounded.", &labels);
    assert_eq!(none.value, None);
    assert_eq!(none.basis, Basis::Jev);

    let unsure = layer.link_implicit(BLOCK_ID, "Then the bound holds.", &labels);
    assert_eq!(unsure.value, None);
    assert_eq!(unsure.basis, Basis::Fallback(FallbackReason::LowConfidence));

    assert_usage(layer.usage(), 3, 360, 0.75);
}

#[test]
fn implicit_link_selects_a_label_or_none_and_falls_back_when_unsure() {
    run_link(&mut replay_layer("link"));
}

#[test]
fn implicit_link_falls_back_without_judging_cut_text() {
    let mut layer = DecisionLayer::new(NeverCalled, DecisionConfig::default()).unwrap();
    let text = format!(
        "{} By the previous lemma, the bound holds.",
        "x".repeat(4_000)
    );
    let outcome = layer.link_implicit(BLOCK_ID, &text, &["lem:bound".into()]);
    assert_eq!(outcome.value, None);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Oversized));
    assert!(outcome.truncated);
    assert!(outcome.evidence.is_none());
    assert_eq!(layer.usage().calls, 0);
}

#[test]
fn interval_checks_accept_brackets_immediately_after_interval() {
    for (conclusion, block) in [
        ("∀ x ∈ [0,1], f x ≤ 1", "On interval(0,1), f x ≤ 1."),
        ("∀ x ∈ (0,1), f x ≤ 1", "On interval[0,1], f x ≤ 1."),
        ("∀ x ∈ (0,1), f x ≤ 1", "On Interval[0,1], f x ≤ 1."),
        (
            "∀ x ∈ interval[0,1], f x ≤ 1",
            "On the interval (0,1), f x ≤ 1.",
        ),
        ("∀ x ∈ interval[0,1], f x ≤ 1", "On interval(0,1), f x ≤ 1."),
    ] {
        let mut layer = DecisionLayer::new(
            Fixed {
                replies: VecDeque::from([answered(vec![(
                    "conclusion.quantifier",
                    choice(
                        "universal",
                        &[("universal", 1.0), ("existential", 0.0), ("neither", 0.0)],
                        1.0,
                    ),
                )])]),
            },
            DecisionConfig::default(),
        )
        .unwrap();
        let flags = layer.check_sir_fields(BLOCK_ID, block, &make_sir(&[], conclusion));
        assert_eq!(
            flag_summary(&flags),
            [("interval[0].endpoints", FlagReason::Disagrees)],
            "{conclusion}; {block}"
        );
        assert!(flags[0].evidence.is_none());
    }
}

#[test]
fn field_checks_recognize_textual_for_all_conclusions() {
    for conclusion in [
        "For all x, f x ≤ 1",
        "  for all x, f x ≤ 1",
        "FOR ALL x, f x ≤ 1",
    ] {
        let mut layer = DecisionLayer::new(
            Fixed {
                replies: VecDeque::from([answered(vec![(
                    "conclusion.quantifier",
                    choice(
                        "universal",
                        &[("universal", 1.0), ("existential", 0.0), ("neither", 0.0)],
                        1.0,
                    ),
                )])]),
            },
            DecisionConfig::default(),
        )
        .unwrap();
        let flags = layer.check_sir_fields(
            BLOCK_ID,
            "Every x satisfies f x ≤ 1.",
            &make_sir(&[], conclusion),
        );
        assert!(flags.is_empty(), "{conclusion}: {flags:?}");
    }
}

fn run_field_checks<D: Decider>(layer: &mut DecisionLayer<D>) {
    let sir = make_sir(
        &["f is continuous on [a, b]", "a ≤ b", "f is bounded"],
        "∃ M, ∀ x ∈ [a, b], |f x| ≤ M",
    );
    let flags = layer.check_sir_fields(
        BLOCK_ID,
        "Let f be continuous on the interval (a, b). Then the interval is bounded.",
        &sir,
    );
    assert_eq!(
        flag_summary(&flags),
        [
            // Explicit brackets are read deterministically: open does not match the SIR's closed.
            ("interval[0].endpoints", FlagReason::Disagrees),
            ("conclusion.quantifier", FlagReason::Disagrees),
            ("hypotheses[1].stated", FlagReason::Disagrees),
            ("hypotheses[2].stated", FlagReason::Uncertain),
            ("interval[1].endpoints", FlagReason::Disagrees),
        ]
    );
    assert!(
        flags[0].evidence.is_none(),
        "deterministic flags carry no Jev answer"
    );
    assert!(
        flags[1].evidence.is_some(),
        "a Jev disagreement keeps its evidence"
    );
    assert_usage(layer.usage(), 1, 120, 0.25);
}

#[test]
fn field_checks_flag_disagreement_and_uncertainty_by_field() {
    run_field_checks(&mut replay_layer("field-checks"));
}

fn run_field_check_failure<D: Decider>(layer: &mut DecisionLayer<D>) {
    let sir = make_sir(
        &["f is continuous on [a, b]"],
        "∃ M, ∀ x ∈ [a, b], |f x| ≤ M",
    );
    let flags = layer.check_sir_fields(
        BLOCK_ID,
        "Let f be continuous on the interval (a, b).",
        &sir,
    );
    assert_eq!(
        flag_summary(&flags),
        [
            ("interval[0].endpoints", FlagReason::Disagrees),
            ("conclusion.quantifier", FlagReason::Uncertain),
            ("hypotheses[0].stated", FlagReason::Uncertain),
        ]
    );
    assert!(flags[1..].iter().all(|flag| flag.evidence.is_none()));
    let usage = layer.usage();
    assert_eq!(usage.calls, 1);
    assert_eq!(
        usage.input_tokens, 40,
        "recovered provider usage is counted"
    );
    assert_eq!(usage.unknown_cost_calls, 1);
}

#[test]
fn field_check_provider_failure_marks_every_asked_field_uncertain() {
    run_field_check_failure(&mut replay_layer("field-check-failure"));
}

fn run_differs<D: Decider>(layer: &mut DecisionLayer<D>) {
    let sir = make_sir(&[], "∀ x, f x ≤ 1");
    let block = "Every x satisfies f x ≤ 1.";

    let yes = layer.sir_obviously_differs(BLOCK_ID, block, &sir);
    assert_eq!(
        yes.value,
        Some(DiffersFlag {
            probability: Some(0.96)
        })
    );
    assert_eq!(yes.basis, Basis::Jev);

    let unsure = layer.sir_obviously_differs(BLOCK_ID, block, &sir);
    assert_eq!(
        unsure.value,
        Some(DiffersFlag {
            probability: Some(0.5)
        })
    );
    assert_eq!(unsure.basis, Basis::Fallback(FallbackReason::LowConfidence));

    let no = layer.sir_obviously_differs(BLOCK_ID, block, &sir);
    assert_eq!(no.value, None);
    assert_eq!(no.basis, Basis::Jev);
    assert_usage(layer.usage(), 3, 360, 0.75);
}

#[test]
fn obvious_difference_highlights_yes_and_uncertainty_but_not_confident_no() {
    run_differs(&mut replay_layer("differs"));
}

fn run_size_cap<D: Decider>(layer: &mut DecisionLayer<D>) {
    let long = CLAIM_TEXT.repeat(100);
    assert!(long.chars().count() > DecisionConfig::default().max_text_chars);
    let outcome = layer.classify_prose(BLOCK_ID, &long);
    assert!(outcome.truncated);
    assert_eq!(outcome.value, ProseKind::Claim);
    assert_usage(layer.usage(), 1, 120, 0.25);
}

#[test]
fn oversized_text_is_cut_with_a_marker_before_it_is_sent() {
    run_size_cap(&mut replay_layer("size-cap"));
    let path = decision_fixture_dir("size-cap").join("call-0000.json");
    let call: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let sent = call["request"]["state"]["block"].as_str().unwrap();
    assert!(sent.ends_with(TRUNCATION_MARKER));
    assert_eq!(
        sent.chars().count(),
        DecisionConfig::default().max_text_chars
    );
}

#[test]
fn recorded_decision_keeps_probabilities_version_and_request() {
    let path = decision_fixture_dir("classify-high").join("call-0000.json");
    let call: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(call["endpoint_kind"], "decision");
    assert_eq!(call["configured_model"], DECISION_MODEL);
    assert_eq!(call["response"]["metadata"]["resolved_model"], SERVED_MODEL);
    assert_eq!(
        call["response"]["answers"]["answer"]["probabilities"]["claim"],
        0.96
    );
    assert_eq!(call["response"]["answers"]["answer"]["confidence"], 0.95);
    assert_eq!(call["request"]["model"], DECISION_MODEL);
    assert_eq!(call["request"]["questions"]["answer"]["type"], "choice");
    assert_eq!(call["provenance"], "synthetic");
}

/// Scripted deciders that return results the layer must not count as provider calls.
#[test]
fn replay_miss_and_stopped_recorder_are_fallbacks_and_not_counted() {
    let mut layer = DecisionLayer::new(
        Fixed {
            replies: VecDeque::from([
                Err(ModelError::ReplayMismatch),
                Err(ModelError::RecordingStopped),
            ]),
        },
        DecisionConfig::default(),
    )
    .unwrap();
    for _ in 0..2 {
        let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
        assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Failed));
    }
    assert_usage(layer.usage(), 0, 0, 0.0);
}

#[test]
fn answers_with_wrong_type_or_extra_ids_are_malformed() {
    let mut layer = DecisionLayer::new(
        Fixed {
            replies: VecDeque::from([
                answered(vec![("answer", yes_no(0.99))]),
                answered(vec![
                    (
                        "answer",
                        choice(
                            "claim",
                            &[("claim", 1.0), ("proof_step", 0.0), ("commentary", 0.0)],
                            1.0,
                        ),
                    ),
                    ("extra", yes_no(0.99)),
                ]),
            ]),
        },
        DecisionConfig::default(),
    )
    .unwrap();
    let wrong_type = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(wrong_type.basis, Basis::Fallback(FallbackReason::Malformed));
    let extra = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(extra.basis, Basis::Fallback(FallbackReason::Malformed));
    assert_usage(layer.usage(), 2, 240, 0.5);
}

#[test]
fn oversized_or_invalid_inputs_never_reach_jev() {
    let mut layer = DecisionLayer::new(NeverCalled, DecisionConfig::default()).unwrap();
    let long = "x".repeat(5_000);
    let sir = make_sir(&["a ≤ b"], "∀ x, f x ≤ 1");

    assert_eq!(layer.is_finished(BLOCK_ID, &long).value, Finished::Unknown);
    assert_eq!(
        layer.is_finished(BLOCK_ID, &long).basis,
        Basis::Fallback(FallbackReason::Oversized)
    );
    assert_eq!(
        layer.sir_obviously_differs(BLOCK_ID, &long, &sir).value,
        Some(DiffersFlag { probability: None }),
        "an unjudged difference is highlighted"
    );
    assert_eq!(
        flag_summary(&layer.check_sir_fields(BLOCK_ID, &long, &sir)),
        [("block", FlagReason::Unchecked)]
    );

    let many: Vec<String> = (0..65).map(|i| format!("lem:{i}")).collect();
    assert_eq!(
        layer.link_implicit(BLOCK_ID, "Then.", &many).basis,
        Basis::Fallback(FallbackReason::Oversized)
    );
    assert_eq!(
        layer.link_implicit(BLOCK_ID, "Then.", &[]).basis,
        Basis::Fallback(FallbackReason::NoCandidates)
    );
    let duplicated = ["lem:a".to_owned(), "lem:a".to_owned()];
    assert_eq!(
        layer.link_implicit(BLOCK_ID, "Then.", &duplicated).basis,
        Basis::Fallback(FallbackReason::InvalidInput)
    );
    let reserved = ["none".to_owned()];
    assert_eq!(
        layer.link_implicit(BLOCK_ID, "Then.", &reserved).basis,
        Basis::Fallback(FallbackReason::InvalidInput)
    );
    assert_eq!(layer.usage().calls, 0);
}

/// Inner client that answers with a model other than the pinned decision model.
struct OtherModel(Usage);

impl ModelClient for OtherModel {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        unreachable!("decisions never request generation")
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        Ok(DecisionReply {
            answers: [(
                "answer".to_owned(),
                choice(
                    "claim",
                    &[("claim", 1.0), ("proof_step", 0.0), ("commentary", 0.0)],
                    1.0,
                ),
            )]
            .into(),
            metadata: CallMetadata {
                configured_model: "typesafe/jev-latest".into(),
                resolved_model: "typesafe/jev-latest".into(),
                ..metadata(self.0.clone())
            },
        })
    }
}

#[test]
fn reply_from_an_unpinned_model_is_rejected() {
    let mut layer = DecisionLayer::new(
        ModelDecider::new(OtherModel(usage()), DECISION_MODEL),
        DecisionConfig::default(),
    )
    .unwrap();
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.value, ProseKind::Commentary);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Failed));
    assert!(outcome.evidence.is_none());
    assert_usage(layer.usage(), 1, 120, 0.25);
    assert_eq!(layer.usage().output_tokens, 1);
    assert_eq!(layer.usage().unknown_input_token_calls, 0);
    assert_eq!(layer.usage().unknown_output_token_calls, 0);
    assert_eq!(layer.usage().unknown_cost_calls, 0);
}

#[test]
fn rejected_unpinned_reply_keeps_unknown_usage_unknown() {
    let mut layer = DecisionLayer::new(
        ModelDecider::new(OtherModel(Usage::default()), DECISION_MODEL),
        DecisionConfig::default(),
    )
    .unwrap();
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Failed));
    assert_usage(layer.usage(), 1, 0, 0.0);
    assert_eq!(layer.usage().unknown_input_token_calls, 1);
    assert_eq!(layer.usage().unknown_output_token_calls, 1);
    assert_eq!(layer.usage().unknown_cost_calls, 1);
}

#[test]
fn rejected_unpinned_reply_keeps_partially_known_usage() {
    let mut layer = DecisionLayer::new(
        ModelDecider::new(
            OtherModel(Usage {
                input_tokens: Some(120),
                ..Usage::default()
            }),
            DECISION_MODEL,
        ),
        DecisionConfig::default(),
    )
    .unwrap();
    let outcome = layer.classify_prose(BLOCK_ID, CLAIM_TEXT);
    assert_eq!(outcome.basis, Basis::Fallback(FallbackReason::Failed));
    assert_usage(layer.usage(), 1, 120, 0.0);
    assert_eq!(layer.usage().unknown_input_token_calls, 0);
    assert_eq!(layer.usage().unknown_output_token_calls, 1);
    assert_eq!(layer.usage().unknown_cost_calls, 1);
}

#[test]
fn configuration_rejects_unusable_bounds_and_thresholds() {
    let defaults = DecisionConfig::default();
    assert!(defaults.validate().is_ok());
    let tiny = DecisionConfig {
        max_text_chars: 64,
        ..defaults
    };
    assert!(DecisionLayer::new(NeverCalled, tiny).is_err());
    let weak = DecisionConfig {
        prose_threshold: 0.4,
        ..defaults
    };
    assert!(weak.validate().is_err());
}

#[test]
fn every_threshold_requires_an_uncertainty_band() {
    let defaults = DecisionConfig::default();
    for config in [
        DecisionConfig {
            prose_threshold: 0.5,
            ..defaults
        },
        DecisionConfig {
            finished_threshold: 0.5,
            ..defaults
        },
        DecisionConfig {
            edit_threshold: 0.5,
            ..defaults
        },
        DecisionConfig {
            link_threshold: 0.5,
            ..defaults
        },
        DecisionConfig {
            field_threshold: 0.5,
            ..defaults
        },
        DecisionConfig {
            differs_threshold: 0.5,
            ..defaults
        },
    ] {
        assert!(config.validate().is_err(), "{config:?}");
    }
    assert!(
        DecisionConfig {
            prose_threshold: 1.0,
            finished_threshold: 0.5 + f64::EPSILON,
            ..defaults
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn threshold_requires_a_representable_uncertainty_probability() {
    let too_narrow = 0.5_f64.next_up();
    let first_usable = too_narrow.next_up();
    assert_eq!(first_usable, 0.5 + f64::EPSILON);
    // The adjacent probabilities at the narrow cutoff are both decisive: there is no float
    // between them that could be uncertain.
    assert_eq!(
        NoulAnswer { noul: 0.5 }.assess(too_narrow),
        NoulAssessment::No
    );
    assert_eq!(
        NoulAnswer { noul: too_narrow }.assess(too_narrow),
        NoulAssessment::Yes
    );
    assert_eq!(
        NoulAnswer { noul: too_narrow }.assess(first_usable),
        NoulAssessment::Uncertain
    );
    let defaults = DecisionConfig::default();
    for config in [
        DecisionConfig {
            prose_threshold: too_narrow,
            ..defaults
        },
        DecisionConfig {
            finished_threshold: too_narrow,
            ..defaults
        },
        DecisionConfig {
            edit_threshold: too_narrow,
            ..defaults
        },
        DecisionConfig {
            link_threshold: too_narrow,
            ..defaults
        },
        DecisionConfig {
            field_threshold: too_narrow,
            ..defaults
        },
        DecisionConfig {
            differs_threshold: too_narrow,
            ..defaults
        },
    ] {
        assert!(config.validate().is_err(), "{config:?}");
    }
    assert!(
        DecisionConfig {
            finished_threshold: first_usable,
            ..defaults
        }
        .validate()
        .is_ok()
    );
}

/// Decisions must not reach review state. `decisions.rs` has no access to `ReviewStore`, and this
/// guard fails if it gains a path to the review module or a confirmed status.
#[test]
fn decisions_cannot_reach_review_state() {
    let source = include_str!("../src/decisions.rs");
    for forbidden in [
        "crate::review",
        "review::",
        "ReviewStore::",
        "statement_confirmed",
        "BlockStatus::",
        "ReviewEntry",
    ] {
        assert!(
            !source.contains(forbidden),
            "decisions.rs must not reference {forbidden}"
        );
    }
}

fn record_all() {
    record(
        "classify-high",
        vec![answered(vec![(
            "answer",
            choice(
                "claim",
                &[("claim", 0.96), ("proof_step", 0.02), ("commentary", 0.02)],
                0.95,
            ),
        )])],
        run_classify_high,
    );
    record(
        "classify-low",
        vec![answered(vec![(
            "answer",
            choice(
                "proof_step",
                &[("claim", 0.4), ("proof_step", 0.35), ("commentary", 0.25)],
                0.45,
            ),
        )])],
        run_classify_low,
    );
    record(
        "classify-malformed",
        vec![answered(vec![("answer", choice_without_evidence("claim"))])],
        run_classify_malformed,
    );
    record(
        "classify-failure",
        vec![Err(ModelError::Transport("connection reset".into()))],
        run_classify_failure,
    );
    record(
        "finished",
        vec![
            answered(vec![("answer", yes_no(0.97))]),
            answered(vec![("answer", yes_no(0.6))]),
            answered(vec![("answer", yes_no(0.02))]),
        ],
        run_finished,
    );
    record(
        "semantic-edit",
        vec![
            answered(vec![("answer", yes_no(0.04))]),
            answered(vec![("answer", yes_no(0.97))]),
            answered(vec![("answer", yes_no(0.5))]),
        ],
        run_semantic_edit,
    );
    record(
        "link",
        vec![
            answered(vec![(
                "answer",
                choice(
                    "lem:bound",
                    &[("lem:bound", 0.96), ("none", 0.02), ("thm:first", 0.02)],
                    0.95,
                ),
            )]),
            answered(vec![(
                "answer",
                choice(
                    "none",
                    &[("lem:bound", 0.02), ("none", 0.95), ("thm:first", 0.03)],
                    0.93,
                ),
            )]),
            answered(vec![(
                "answer",
                choice(
                    "thm:first",
                    &[("lem:bound", 0.3), ("none", 0.2), ("thm:first", 0.5)],
                    0.6,
                ),
            )]),
        ],
        run_link,
    );
    record(
        "field-checks",
        vec![answered(vec![
            (
                "conclusion.quantifier",
                choice(
                    "universal",
                    &[
                        ("universal", 0.95),
                        ("existential", 0.03),
                        ("neither", 0.02),
                    ],
                    0.93,
                ),
            ),
            (
                "hypotheses[0].stated",
                choice("stated", &[("stated", 0.96), ("implied", 0.04)], 0.95),
            ),
            (
                "hypotheses[1].stated",
                choice("implied", &[("stated", 0.06), ("implied", 0.94)], 0.9),
            ),
            (
                "hypotheses[2].stated",
                choice("stated", &[("stated", 0.6), ("implied", 0.4)], 0.5),
            ),
            (
                "interval[1].endpoints",
                choice(
                    "open",
                    &[
                        ("closed", 0.03),
                        ("open", 0.93),
                        ("half_open", 0.02),
                        ("unspecified", 0.02),
                    ],
                    0.92,
                ),
            ),
        ])],
        run_field_checks,
    );
    record(
        "field-check-failure",
        vec![Err(ModelError::Provider {
            message: "provider HTTP status 503".into(),
            failure: Box::new(ProviderFailure {
                status: 503,
                response: json!({"error": "unavailable"}),
                resolved_model: None,
                usage: Some(Usage {
                    input_tokens: Some(40),
                    output_tokens: Some(0),
                    total_tokens: Some(40),
                    cost: None,
                }),
            }),
        })],
        run_field_check_failure,
    );
    record(
        "differs",
        vec![
            answered(vec![("answer", yes_no(0.96))]),
            answered(vec![("answer", yes_no(0.5))]),
            answered(vec![("answer", yes_no(0.03))]),
        ],
        run_differs,
    );
    record(
        "size-cap",
        vec![answered(vec![(
            "answer",
            choice(
                "claim",
                &[("claim", 0.96), ("proof_step", 0.02), ("commentary", 0.02)],
                0.95,
            ),
        )])],
        run_size_cap,
    );
}

/// Rewrites `tests/fixtures/recordings/decisions` from the scripts above. Run deliberately:
/// `cargo test -p mls-engine --test decisions -- --ignored regenerate`.
#[test]
#[ignore = "regenerates the synthetic decision recordings; run deliberately"]
fn regenerate_synthetic_decision_recordings() {
    record_all();
}
