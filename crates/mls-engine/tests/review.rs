//! Acceptance tests for human SIR review: confirm, reject, edit, and the Lean cache they unlock.
//! Every model reply and checker step is scripted, so an unexpected call fails the test.

mod common;

use common::*;
use mls_core::{Block, BlockId};
use mls_engine::{
    BatchReport, BlockReport, BlockStatus, BlockView, CheckState, ReviewEntry, ReviewError,
    ReviewStore, ReviewView, badge, lacks_persisted_ids, review_view, run_batch_analyzed,
    run_batch_reviewed,
};
use mls_models::{GenerationReply, GenerationStage, ModelError};
use mls_sir::{Sir, StatementKind};
use mls_tm::{Analysis, analyze, parse};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// Build a TeXmacs document in the fixture format from (number, environment, text, label).
fn document(blocks: &[(u8, &str, &str, &str)]) -> Vec<u8> {
    let mut source = String::from("<TeXmacs|2.1.4>\n<\\body>\n");
    for (number, environment, text, label) in blocks {
        source.push_str(&format!(
            "  <section|S{number}>\n  <\\with|mls-id|00000000-0000-4000-8000-{number:012}><\\{environment}>{text}<label|{label}></{environment}></with>\n"
        ));
    }
    source.push_str("</body>\n");
    source.into_bytes()
}

fn single(text: &str) -> Vec<u8> {
    document(&[(1, "theorem", text, "thm:first")])
}

fn two_blocks(first_text: &str) -> Vec<u8> {
    document(&[
        (1, "theorem", first_text, "thm:first"),
        (
            2,
            "theorem",
            "Truth still holds by <reference|thm:first>.",
            "thm:second",
        ),
    ])
}

fn analysis_of(source: &[u8]) -> Analysis {
    let document = parse(source).unwrap();
    analyze(&document)
}

fn id(number: u8) -> BlockId {
    format!("00000000-0000-4000-8000-{number:012}")
        .parse()
        .unwrap()
}

fn block(analysis: &Analysis, number: u8) -> &Block {
    analysis
        .blocks
        .iter()
        .find(|block| block.id == id(number))
        .unwrap()
}

fn report(batch: &BatchReport, number: u8) -> &BlockReport {
    batch
        .blocks
        .iter()
        .find(|block| block.block_id == id(number))
        .unwrap()
}

fn ok(number: u8) -> CheckStep {
    CheckStep::Extracted(extracted(number, "True"))
}

fn lean_for(number: u8) -> GenerationReply {
    lean(&format!(
        "theorem mls_00000000_0000_4000_8000_{number:012} : True := by sorry"
    ))
}

fn scripted(replies: Vec<GenerationReply>) -> CapturingClient {
    CapturingClient {
        replies: replies.into_iter().map(Ok).collect(),
        requests: Vec::new(),
    }
}

fn sir_calls(models: &CapturingClient) -> usize {
    models
        .requests
        .iter()
        .filter(|request| request.stage == GenerationStage::Sir)
        .count()
}

fn lean_calls(models: &CapturingClient) -> usize {
    models
        .requests
        .iter()
        .filter(|request| request.stage != GenerationStage::Sir)
        .count()
}

fn prompt(models: &CapturingClient, index: usize) -> &str {
    models.requests[index].messages[1].content.as_str()
}

/// Run the reviewed pipeline over a document and return the report.
fn run(
    source: &[u8],
    models: &mut CapturingClient,
    checker: &mut ScriptedChecker,
    review: &ReviewStore,
) -> BatchReport {
    let analysis = analysis_of(source);
    run_batch_reviewed(source, &analysis, models, checker, config(), review).unwrap()
}

/// Confirm the current report of one block against the analysis it was produced from.
fn confirm(store: &mut ReviewStore, source: &[u8], batch: &BatchReport, number: u8) {
    let analysis = analysis_of(source);
    store
        .confirm(block(&analysis, number), report(batch, number))
        .unwrap();
}

/// Record the accepted Lean declaration of a block after a run.
fn cache_lean(store: &mut ReviewStore, batch: &BatchReport, number: u8) {
    let block_report = report(batch, number);
    store
        .record_accepted_lean(
            id(number),
            block_report.cache_key.clone().unwrap(),
            block_report.lean_statement.clone().unwrap(),
        )
        .unwrap();
}

#[test]
fn empty_store_reports_match_the_plain_batch() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let replies = || {
        vec![
            sir(1, "theorem", "thm:first", vec![], "truth"),
            lean_for(1),
            sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
            lean_for(2),
        ]
    };
    let mut plain_models = scripted(replies());
    let mut plain_checker = ScriptedChecker::new([ok(1), ok(2)]);
    let plain = run_batch_analyzed(
        &source,
        &analysis,
        &mut plain_models,
        &mut plain_checker,
        config(),
    )
    .unwrap();

    let mut reviewed_models = scripted(replies());
    let mut reviewed_checker = ScriptedChecker::new([ok(1), ok(2)]);
    let reviewed = run(
        &source,
        &mut reviewed_models,
        &mut reviewed_checker,
        &ReviewStore::new(),
    );

    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::to_value(&reviewed).unwrap()
    );
    assert!(reviewed.blocks.iter().all(|block| {
        block.status == BlockStatus::SirDrafted && block.sir_origin.as_deref() == Some("generated")
    }));
}

#[test]
fn confirmed_block_skips_sir_and_its_cached_lean_skips_generation() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();

    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    assert_eq!(report(&first, 1).status, BlockStatus::SirDrafted);
    confirm(&mut store, &source, &first, 1);

    // Confirmed, text unchanged: the SIR is not requested again, but Lean is generated once.
    let mut second_models = scripted(vec![lean_for(1)]);
    let mut second_checker = ScriptedChecker::new([ok(1)]);
    let second = run(&source, &mut second_models, &mut second_checker, &store);
    let confirmed = report(&second, 1);
    assert_eq!(confirmed.status, BlockStatus::StatementConfirmed);
    assert_eq!(confirmed.sir_origin.as_deref(), Some("confirmed"));
    assert_eq!(confirmed.lean_origin.as_deref(), Some("generated"));
    assert_eq!(sir_calls(&second_models), 0);
    assert_eq!(lean_calls(&second_models), 1);
    cache_lean(&mut store, &second, 1);

    // With the cached declaration, neither the SIR nor Lean is requested from the model.
    let mut third_models = scripted(vec![]);
    let mut third_checker = ScriptedChecker::new([ok(1)]);
    let third = run(&source, &mut third_models, &mut third_checker, &store);
    let cached = report(&third, 1);
    assert_eq!(cached.status, BlockStatus::StatementConfirmed);
    assert_eq!(cached.lean_origin.as_deref(), Some("cache"));
    assert_eq!(cached.attempts, 0);
    assert!(third_models.requests.is_empty());
    assert_eq!(
        third_checker.checked_sources,
        [report(&second, 1).lean_statement.clone().unwrap()]
    );
    assert_eq!(third_checker.finishes, [true]);
}

#[test]
fn a_cached_declaration_that_fails_to_check_falls_back_to_generation() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    confirm(&mut store, &source, &first, 1);
    cache_lean(&mut store, &first, 1);

    let mut models = scripted(vec![lean_for(1)]);
    let mut checker = ScriptedChecker::new([CheckStep::Error("environment changed"), ok(1)]);
    let batch = run(&source, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::StatementConfirmed);
    assert_eq!(block_report.lean_origin.as_deref(), Some("generated"));
    assert_eq!(block_report.attempts, 1);
    assert_eq!(checker.checked_sources.len(), 2);
    assert_eq!(sir_calls(&models), 0);
}

#[test]
fn cosmetic_edit_keeps_the_confirmation_and_reuses_the_cached_lean() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    confirm(&mut store, &source, &first, 1);
    let mut second_models = scripted(vec![lean_for(1)]);
    let mut second_checker = ScriptedChecker::new([ok(1)]);
    let second = run(&source, &mut second_models, &mut second_checker, &store);
    cache_lean(&mut store, &second, 1);

    // The wording changes, the model returns the same SIR, and the cache key does not move.
    let reworded = single("Truth holds, indeed.");
    let mut models = scripted(vec![sir(1, "theorem", "thm:first", vec![], "truth")]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(&reworded, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::StatementConfirmed);
    assert!(block_report.cosmetic_reconfirmation);
    assert_eq!(block_report.sir_origin.as_deref(), Some("confirmed"));
    assert!(block_report.superseded_confirmation.is_none());
    assert_eq!(sir_calls(&models), 1);
    assert_eq!(lean_calls(&models), 0);
    assert_eq!(block_report.lean_origin.as_deref(), Some("cache"));
}

#[test]
fn layout_only_resave_keeps_the_confirmation_and_the_cache_without_a_model_call() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    confirm(&mut store, &source, &first, 1);
    let mut second_models = scripted(vec![lean_for(1)]);
    let mut second_checker = ScriptedChecker::new([ok(1)]);
    let second = run(&source, &mut second_models, &mut second_checker, &store);
    cache_lean(&mut store, &second, 1);

    // TeXmacs saves the same tree in a long form with indentation and a doubled space.
    let resaved = b"<TeXmacs|2.1.5>\n\n<\\body>\n  <section|S1>\n  <with|mls-id|00000000-0000-4000-8000-000000000001|<\\theorem>\n    Truth  holds.<label|thm:first>\n  </theorem>>\n</body>\n";
    let mut models = scripted(vec![]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(resaved, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::StatementConfirmed);
    assert!(!block_report.cosmetic_reconfirmation);
    assert_eq!(block_report.sir_origin.as_deref(), Some("confirmed"));
    assert_eq!(sir_calls(&models), 0);
    assert_eq!(lean_calls(&models), 0);
    assert_eq!(block_report.lean_origin.as_deref(), Some("cache"));
}

#[test]
fn semantic_change_supersedes_the_confirmation_and_needs_review_again() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    confirm(&mut store, &source, &first, 1);

    let changed = single("Truth holds, and more.");
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth and more"),
        lean_for(1),
    ]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(&changed, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::SirDrafted);
    assert_eq!(block_report.sir_origin.as_deref(), Some("generated"));
    assert!(!block_report.cosmetic_reconfirmation);
    let superseded = block_report.superseded_confirmation.as_deref().unwrap();
    assert!(superseded.contains("conclusion"), "{superseded}");
    assert_eq!(
        block_report.sir.as_ref().unwrap().conclusion(),
        "truth and more"
    );
    assert_eq!(lean_calls(&models), 1);
}

#[test]
fn rejected_sir_is_shown_to_the_next_generation() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    let analysis = analysis_of(&source);
    store
        .reject(block(&analysis, 1), report(&first, 1))
        .unwrap();

    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "another truth"),
        lean_for(1),
    ]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(&source, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::SirDrafted);
    assert!(block_report.previously_rejected);
    let rejected_json = serde_json::to_string(first.blocks[0].sir.as_ref().unwrap()).unwrap();
    assert!(
        prompt(&models, 0).contains(&rejected_json),
        "{}",
        prompt(&models, 0)
    );
    assert!(prompt(&models, 0).contains("REVIEW NOTE"));
}

#[test]
fn regenerating_an_identical_rejected_sir_is_not_formalized_and_skips_lean() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1)]);
    let first = run(&source, &mut first_models, &mut first_checker, &store);
    let analysis = analysis_of(&source);
    store
        .reject(block(&analysis, 1), report(&first, 1))
        .unwrap();

    let mut models = scripted(vec![sir(1, "theorem", "thm:first", vec![], "truth")]);
    let mut checker = ScriptedChecker::new([]);
    let batch = run(&source, &mut models, &mut checker, &store);
    let block_report = report(&batch, 1);
    assert_eq!(block_report.status, BlockStatus::NotFormalized);
    assert_eq!(
        block_report.errors,
        ["the regenerated SIR equals the rejected SIR"]
    );
    assert!(block_report.sir.is_none());
    assert!(block_report.previously_rejected);
    assert_eq!(models.requests.len(), 1);
    assert!(checker.checked_sources.is_empty());
}

#[test]
fn invalid_edits_are_refused_and_leave_the_store_unchanged() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let second = block(&analysis, 2);
    let expected_uses = ["thm:first".to_owned()];
    let mut store = ReviewStore::new();
    let before = store.clone();
    let valid = json!({
        "kind": "theorem", "label": "thm:second", "variables": [], "hypotheses": [],
        "conclusion": "edited truth", "uses": ["thm:first"], "assumptions_made": []
    });
    let edit = |store: &mut ReviewStore, value: &Value| {
        store.edit(second, &expected_uses, &value.to_string())
    };

    assert!(matches!(
        store.edit(second, &expected_uses, "{not json"),
        Err(ReviewError::InvalidSirJson(_))
    ));
    assert_eq!(store, before);

    let mut unknown = valid.clone();
    unknown["extra"] = json!(1);
    assert!(matches!(
        edit(&mut store, &unknown),
        Err(ReviewError::InvalidSirJson(_))
    ));
    assert_eq!(store, before);

    let mut wrong_label = valid.clone();
    wrong_label["label"] = json!("thm:other");
    match edit(&mut store, &wrong_label) {
        Err(ReviewError::IdentityMismatch(problems)) => {
            assert!(problems.iter().any(|problem| problem.contains("label")));
        }
        other => panic!("expected an identity mismatch, got {other:?}"),
    }
    assert_eq!(store, before);

    let mut wrong_kind = valid.clone();
    wrong_kind["kind"] = json!("definition");
    match edit(&mut store, &wrong_kind) {
        Err(ReviewError::IdentityMismatch(problems)) => {
            assert!(problems.iter().any(|problem| problem.contains("kind")));
        }
        other => panic!("expected an identity mismatch, got {other:?}"),
    }
    assert_eq!(store, before);

    let mut wrong_uses = valid.clone();
    wrong_uses["uses"] = json!(["thm:other"]);
    match edit(&mut store, &wrong_uses) {
        Err(ReviewError::IdentityMismatch(problems)) => {
            assert!(problems.iter().any(|problem| problem.contains("uses")));
        }
        other => panic!("expected an identity mismatch, got {other:?}"),
    }
    assert_eq!(store, before);

    edit(&mut store, &valid).unwrap();
    assert!(matches!(
        store.entry(id(2)),
        Some(ReviewEntry::Confirmed { edited: true, .. })
    ));
}

#[test]
fn valid_edit_is_pinned_and_used_without_a_sir_call() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let mut store = ReviewStore::new();
    let edited = json!({
        "kind": "theorem", "label": "thm:second", "variables": [], "hypotheses": [],
        "conclusion": "edited truth", "uses": ["thm:first"], "assumptions_made": []
    });
    store
        .edit(
            block(&analysis, 2),
            &["thm:first".to_owned()],
            &edited.to_string(),
        )
        .unwrap();

    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
        lean_for(2),
    ]);
    let mut checker = ScriptedChecker::new([ok(1), ok(2)]);
    let batch = run(&source, &mut models, &mut checker, &store);
    // The edited block waits for its dependency, which has no confirmation yet.
    let edited_report = report(&batch, 2);
    assert_eq!(edited_report.status, BlockStatus::SirDrafted);
    assert_eq!(edited_report.sir_origin.as_deref(), Some("edited"));
    assert_eq!(sir_calls(&models), 1);
    assert!(prompt(&models, 2).contains("edited truth"));

    // Confirming the dependency settles both blocks in the view, without another run.
    confirm(&mut store, &source, &batch, 1);
    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    assert_eq!(view_block(&view, 1).status, BlockStatus::StatementConfirmed);
    let settled = view_block(&view, 2);
    assert_eq!(settled.status, BlockStatus::StatementConfirmed);
    assert_eq!(settled.review, "edited");
    assert!(!view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn changed_dependency_sir_changes_the_cache_key_and_regenerates_lean() {
    let v1 = two_blocks("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
        sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
        lean_for(2),
    ]);
    let mut first_checker = ScriptedChecker::new([ok(1), ok(2)]);
    let first = run(&v1, &mut first_models, &mut first_checker, &store);
    confirm(&mut store, &v1, &first, 1);
    confirm(&mut store, &v1, &first, 2);
    cache_lean(&mut store, &first, 1);
    cache_lean(&mut store, &first, 2);

    let mut cached_models = scripted(vec![]);
    let mut cached_checker = ScriptedChecker::new([ok(1), ok(2)]);
    let cached = run(&v1, &mut cached_models, &mut cached_checker, &store);
    assert!(cached.blocks.iter().all(|block| {
        block.status == BlockStatus::StatementConfirmed
            && block.lean_origin.as_deref() == Some("cache")
    }));
    let original_key = report(&cached, 2).cache_key.clone();
    assert_eq!(original_key, report(&first, 2).cache_key);

    // Block 1 changes meaning. Block 2's text is unchanged, but its key must now differ.
    let v2 = two_blocks("Truth fails.");
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "falsity"),
        lean_for(1),
        lean_for(2),
    ]);
    let mut checker = ScriptedChecker::new([ok(1), ok(2)]);
    let batch = run(&v2, &mut models, &mut checker, &store);
    assert_eq!(report(&batch, 1).status, BlockStatus::SirDrafted);
    // The dependent keeps its confirmation, but it waits until its dependency is confirmed again.
    let dependent = report(&batch, 2);
    assert_eq!(dependent.status, BlockStatus::SirDrafted);
    assert_eq!(dependent.sir_origin.as_deref(), Some("confirmed"));
    assert_eq!(dependent.lean_origin.as_deref(), Some("generated"));
    assert_ne!(dependent.cache_key, original_key);
    assert_eq!(sir_calls(&models), 1);
    assert_eq!(lean_calls(&models), 2);
    let view = review_view(DOCUMENT, &analysis_of(&v2), &batch, &store);
    let waiting = view_block(&view, 2);
    assert_eq!(waiting.status, BlockStatus::SirDrafted);
    assert!(!waiting.needs_rerun);
    assert!(
        waiting
            .message
            .starts_with("sir_drafted: confirmed; waiting for dependency confirmation: thm:first"),
        "{}",
        waiting.message
    );
}

#[test]
fn confirm_refuses_stale_foreign_and_unusable_reports() {
    let v1 = single("Truth holds.");
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let first = run(&v1, &mut models, &mut checker, &ReviewStore::new());

    let mut store = ReviewStore::new();
    let changed = analysis_of(&single("Truth changed."));
    assert_eq!(
        store.confirm(block(&changed, 1), report(&first, 1)),
        Err(ReviewError::StaleReport { block: id(1) })
    );
    assert_eq!(store, ReviewStore::new());

    let two = analysis_of(&two_blocks("Truth holds."));
    assert_eq!(
        store.confirm(block(&two, 2), report(&first, 1)),
        Err(ReviewError::ReportForOtherBlock {
            block: id(2),
            report: id(1),
        })
    );
    assert_eq!(store, ReviewStore::new());

    // An identity failure keeps the SIR for review but is not usable, so it cannot be confirmed.
    let mut wrong_label = scripted(vec![sir(1, "theorem", "thm:wrong", vec![], "truth")]);
    let mut no_checks = ScriptedChecker::new([]);
    let unusable = run(&v1, &mut wrong_label, &mut no_checks, &ReviewStore::new());
    assert_eq!(report(&unusable, 1).status, BlockStatus::NotFormalized);
    let original = analysis_of(&v1);
    match store.confirm(block(&original, 1), report(&unusable, 1)) {
        Err(ReviewError::NoUsableSir { errors, .. }) => {
            assert!(errors.iter().any(|error| error.contains("label")));
        }
        other => panic!("expected no usable SIR, got {other:?}"),
    }
    assert_eq!(store, ReviewStore::new());
    // Rejecting it is still allowed, so the next generation is told why.
    store
        .reject(block(&original, 1), report(&unusable, 1))
        .unwrap();
    assert!(matches!(
        store.entry(id(1)),
        Some(ReviewEntry::Rejected { .. })
    ));
}

#[test]
fn record_accepted_lean_requires_a_confirmed_sir() {
    let mut store = ReviewStore::new();
    assert_eq!(
        store.record_accepted_lean(id(1), "key".into(), "declaration".into()),
        Err(ReviewError::NotConfirmed { block: id(1) })
    );
    assert!(store.cached_lean(id(1)).is_none());
}

#[test]
fn store_round_trips_and_refuses_unknown_or_tampered_fields() {
    let source = two_blocks("Truth holds.");
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
        sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
        lean_for(2),
    ]);
    let mut checker = ScriptedChecker::new([ok(1), ok(2)]);
    let first = run(&source, &mut models, &mut checker, &ReviewStore::new());
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &first, 1);
    cache_lean(&mut store, &first, 1);
    confirm(&mut store, &source, &first, 2);

    let json = store.to_json();
    assert_eq!(ReviewStore::from_json(&json).unwrap(), store);

    let value: Value = serde_json::from_str(&json).unwrap();
    let refused = |mutated: &Value| ReviewStore::from_json(&mutated.to_string());

    let mut top = value.clone();
    top["bogus"] = json!(1);
    assert!(matches!(refused(&top), Err(ReviewError::InvalidStore(_))));

    let mut entry = value.clone();
    entry["entries"][id(1).to_string()]["bogus"] = json!(1);
    assert!(matches!(refused(&entry), Err(ReviewError::InvalidStore(_))));

    let mut cached = value.clone();
    cached["lean_cache"][id(1).to_string()]["bogus"] = json!(1);
    assert!(matches!(
        refused(&cached),
        Err(ReviewError::InvalidStore(_))
    ));

    let mut tampered = value.clone();
    tampered["entries"][id(2).to_string()]["sir_hash"] = json!("00");
    assert!(matches!(
        refused(&tampered),
        Err(ReviewError::InvalidStore(_))
    ));

    let mut version = value.clone();
    version["schema_version"] = json!(2);
    assert!(matches!(
        refused(&version),
        Err(ReviewError::InvalidStore(_))
    ));
}

#[test]
fn without_a_store_entry_no_block_is_ever_statement_confirmed() {
    type Scenario = (
        Vec<u8>,
        Vec<Result<GenerationReply, ModelError>>,
        Vec<CheckStep>,
    );
    let wrong_label = sir(1, "theorem", "thm:wrong", vec![], "truth");
    let scenarios: Vec<(&str, Scenario, Vec<BlockStatus>)> = vec![
        (
            "two formalized blocks",
            (
                two_blocks("Truth holds."),
                vec![
                    Ok(sir(1, "theorem", "thm:first", vec![], "truth")),
                    Ok(lean_for(1)),
                    Ok(sir(2, "theorem", "thm:second", vec!["thm:first"], "truth")),
                    Ok(lean_for(2)),
                ],
                vec![ok(1), ok(2)],
            ),
            vec![BlockStatus::SirDrafted, BlockStatus::SirDrafted],
        ),
        (
            "lean keeps failing",
            (
                single("Truth holds."),
                vec![
                    Ok(sir(1, "theorem", "thm:first", vec![], "truth")),
                    Ok(lean_for(1)),
                    Ok(lean_for(1)),
                ],
                vec![
                    CheckStep::Error("unknown identifier"),
                    CheckStep::Error("unknown identifier"),
                ],
            ),
            vec![BlockStatus::FormalizationError],
        ),
        (
            "sir call fails",
            (
                two_blocks("Truth holds."),
                vec![Err(ModelError::Transport("outage".into()))],
                vec![],
            ),
            vec![BlockStatus::NotFormalized, BlockStatus::NotFormalized],
        ),
        (
            "identity mismatch",
            (single("Truth holds."), vec![Ok(wrong_label)], vec![]),
            vec![BlockStatus::NotFormalized],
        ),
    ];

    for (name, (source, replies, steps), expected) in scenarios {
        let mut models = CapturingClient {
            replies: VecDeque::from(replies),
            requests: Vec::new(),
        };
        let mut checker = ScriptedChecker::new(steps);
        let batch = run(&source, &mut models, &mut checker, &ReviewStore::new());
        let statuses: Vec<_> = batch.blocks.iter().map(|block| block.status).collect();
        assert_eq!(statuses, expected, "{name}");
        for block in &batch.blocks {
            assert!(!block.cosmetic_reconfirmation, "{name}");
            assert!(block.superseded_confirmation.is_none(), "{name}");
            assert!(
                !matches!(block.sir_origin.as_deref(), Some("confirmed" | "edited")),
                "{name}"
            );
        }
        let counts = badge(&batch);
        assert_eq!(counts.statement_confirmed, 0, "{name}");
        assert_eq!(
            counts.not_formalized
                + counts.sir_drafted
                + counts.formalization_error
                + counts.statement_confirmed,
            batch.blocks.len(),
            "{name}"
        );
    }
}

#[test]
fn an_unconfirmed_draft_is_never_cached() {
    let source = single("Truth holds.");
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(&source, &mut models, &mut checker, &ReviewStore::new());
    let mut store = ReviewStore::new();
    let result = store.record_accepted_lean(
        id(1),
        report(&batch, 1).cache_key.clone().unwrap(),
        report(&batch, 1).lean_statement.clone().unwrap(),
    );
    assert_eq!(result, Err(ReviewError::NotConfirmed { block: id(1) }));
    assert!(store.cached_lean(id(1)).is_none());
    assert!(
        report(&batch, 1)
            .checks
            .iter()
            .all(|check| check.state != CheckState::Mismatch)
    );
}

const DOCUMENT: &str = "/example/notes.tm";

fn view_block(view: &ReviewView, number: u8) -> &BlockView {
    view.blocks
        .iter()
        .find(|block| block.block_id == id(number))
        .unwrap()
}

fn edited_sir_json(conclusion: &str) -> String {
    json!({
        "kind": "theorem",
        "label": "thm:first",
        "variables": [],
        "hypotheses": [],
        "conclusion": conclusion,
        "uses": [],
        "assumptions_made": []
    })
    .to_string()
}

/// A statement_confirmed block must show a SIR whose confirmation is in the store.
fn assert_confirmation_invariant(view: &ReviewView, store: &ReviewStore) {
    for block in &view.blocks {
        if block.status != BlockStatus::StatementConfirmed {
            continue;
        }
        let sir = block.sir.as_ref().expect("a confirmed block shows its SIR");
        match store.entry(block.block_id) {
            Some(ReviewEntry::Confirmed { sir_hash, .. }) => {
                assert_eq!(*sir_hash, sir.semantic_hash());
            }
            other => panic!("statement_confirmed without a confirmation: {other:?}"),
        }
    }
}

fn fresh_two_block_run(source: &[u8]) -> BatchReport {
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
        sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
        lean_for(2),
    ]);
    let mut checker = ScriptedChecker::new([ok(1), ok(2)]);
    run(source, &mut models, &mut checker, &ReviewStore::new())
}

#[test]
fn a_fresh_run_is_drafted_for_every_block_and_counts_all_keys() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&source);
    let view = review_view(DOCUMENT, &analysis, &batch, &ReviewStore::new());

    let order: Vec<_> = view.blocks.iter().map(|block| block.block_id).collect();
    assert_eq!(order, vec![id(1), id(2)]);
    assert!(
        view.blocks
            .iter()
            .all(|block| block.status == BlockStatus::SirDrafted
                && block.review == "none"
                && !block.needs_rerun
                && block.lean_statement.is_some()
                && block.message.starts_with("sir_drafted"))
    );
    assert!(!view.needs_rerun);
    assert_eq!(view.protocol, 1);
    let counts = serde_json::to_value(view.counts).unwrap();
    assert_eq!(
        counts,
        json!({"not_formalized": 0, "sir_drafted": 2, "statement_confirmed": 0, "formalization_error": 0})
    );
}

#[test]
fn a_confirmed_sir_is_statement_confirmed_only_for_its_block() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &batch, 1);

    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    let first = view_block(&view, 1);
    assert_eq!(first.status, BlockStatus::StatementConfirmed);
    assert_eq!(first.review, "confirmed");
    assert!(!first.needs_rerun);
    assert!(first.message.starts_with("statement_confirmed"));
    assert_eq!(view_block(&view, 2).status, BlockStatus::SirDrafted);
    assert!(!view.needs_rerun);
    assert_eq!(view.counts.statement_confirmed, 1);
    assert_eq!(view.counts.sir_drafted, 1);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn an_edited_sir_is_drafted_until_a_run_gives_it_a_lean_statement() {
    let source = single("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = run(
        &source,
        &mut scripted(vec![
            sir(1, "theorem", "thm:first", vec![], "truth"),
            lean_for(1),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &ReviewStore::new(),
    );
    let mut store = ReviewStore::new();
    store
        .edit(block(&analysis, 1), &[], &edited_sir_json("edited truth"))
        .unwrap();

    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    let edited = view_block(&view, 1);
    assert_eq!(edited.status, BlockStatus::SirDrafted);
    assert_eq!(edited.review, "edited");
    assert!(edited.needs_rerun);
    assert!(edited.lean_statement.is_none());
    assert_eq!(edited.sir.as_ref().unwrap().conclusion(), "edited truth");
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn a_rejected_sir_is_not_formalized_and_needs_a_rerun() {
    let source = single("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&single("Truth holds."));
    let mut store = ReviewStore::new();
    store
        .reject(block(&analysis, 1), report(&batch, 1))
        .unwrap();

    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    let rejected = view_block(&view, 1);
    assert_eq!(rejected.status, BlockStatus::NotFormalized);
    assert_eq!(rejected.review, "rejected");
    assert!(rejected.needs_rerun);
    assert!(rejected.previously_rejected);
    assert!(rejected.sir.is_none());
    assert!(rejected.lean_statement.is_none());
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn a_changed_block_is_downgraded_and_needs_a_rerun() {
    let source = single("Truth holds.");
    let first = run(
        &source,
        &mut scripted(vec![
            sir(1, "theorem", "thm:first", vec![], "truth"),
            lean_for(1),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &ReviewStore::new(),
    );
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &first, 1);
    let batch = run(
        &source,
        &mut scripted(vec![lean_for(1)]),
        &mut ScriptedChecker::new([ok(1)]),
        &store,
    );
    assert_eq!(report(&batch, 1).status, BlockStatus::StatementConfirmed);

    // The text changed after the last run, so the report no longer describes the block.
    let changed = analysis_of(&single("Truth holds, and more."));
    let view = review_view(DOCUMENT, &changed, &batch, &store);
    let block_view = view_block(&view, 1);
    assert_eq!(block_view.status, BlockStatus::SirDrafted);
    assert_eq!(block_view.review, "confirmed");
    assert!(block_view.needs_rerun);
    assert!(
        block_view.message.starts_with("sir_drafted: block changed"),
        "{}",
        block_view.message
    );
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn blocks_missing_from_the_report_are_not_formalized_in_formal_order() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let empty = BatchReport {
        diagnostics: Vec::new(),
        blocks: Vec::new(),
        usage: Default::default(),
    };
    let mut store = ReviewStore::new();
    store.forget(id(2));
    let view = review_view(DOCUMENT, &analysis, &empty, &store);
    let order: Vec<_> = view.blocks.iter().map(|block| block.block_id).collect();
    assert_eq!(order, vec![id(1), id(2)]);
    assert!(view.blocks.iter().all(|block| {
        block.status == BlockStatus::NotFormalized && block.needs_rerun && block.sir.is_none()
    }));
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn a_report_block_the_document_no_longer_has_needs_a_rerun() {
    let source = two_blocks("Truth holds.");
    let batch = fresh_two_block_run(&source);
    let without_second = analysis_of(&single("Truth holds."));
    let view = review_view(DOCUMENT, &without_second, &batch, &ReviewStore::new());
    assert_eq!(view.blocks.len(), 1);
    assert_eq!(view.blocks[0].block_id, id(1));
    assert!(view.needs_rerun);
}

#[test]
fn a_cosmetic_reconfirmation_is_anchored_to_the_reworded_block() {
    let source = single("Truth holds.");
    let mut store = ReviewStore::new();
    let mut first_models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
    ]);
    let first = run(
        &source,
        &mut first_models,
        &mut ScriptedChecker::new([ok(1)]),
        &store,
    );
    confirm(&mut store, &source, &first, 1);
    let mut second_models = scripted(vec![lean_for(1)]);
    let second = run(
        &source,
        &mut second_models,
        &mut ScriptedChecker::new([ok(1)]),
        &store,
    );
    cache_lean(&mut store, &second, 1);

    let reworded = single("Truth holds, indeed.");
    let reworded_analysis = analysis_of(&reworded);
    let mut models = scripted(vec![sir(1, "theorem", "thm:first", vec![], "truth")]);
    let batch = run(
        &reworded,
        &mut models,
        &mut ScriptedChecker::new([ok(1)]),
        &store,
    );
    assert!(report(&batch, 1).cosmetic_reconfirmation);

    let before = review_view(DOCUMENT, &reworded_analysis, &batch, &store);
    assert!(view_block(&before, 1).needs_rerun);
    assert!(store.anchor_text(block(&reworded_analysis, 1), report(&batch, 1)));
    assert!(!store.anchor_text(block(&reworded_analysis, 1), report(&batch, 1)));

    let after = review_view(DOCUMENT, &reworded_analysis, &batch, &store);
    let anchored = view_block(&after, 1);
    assert_eq!(anchored.status, BlockStatus::StatementConfirmed);
    assert_eq!(anchored.review, "confirmed");
    assert!(!anchored.needs_rerun);
    assert!(!after.needs_rerun);
    assert_confirmation_invariant(&after, &store);
}

#[test]
fn the_view_is_a_pure_function_of_its_inputs() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &batch, 2);
    let first = serde_json::to_string(&review_view(DOCUMENT, &analysis, &batch, &store)).unwrap();
    let restored = ReviewStore::from_json(&store.to_json()).unwrap();
    let again =
        serde_json::to_string(&review_view(DOCUMENT, &analysis, &batch, &restored)).unwrap();
    assert_eq!(first, again);
}

#[test]
fn formal_blocks_without_persisted_ids_are_detected_and_other_blocks_are_not() {
    let unidentified =
        b"<TeXmacs|2.1.4>\n<\\body>\n  <\\theorem>Truth holds.<label|thm:first></theorem>\n</body>\n";
    assert!(lacks_persisted_ids(&analysis_of(unidentified)));
    assert!(!lacks_persisted_ids(&analysis_of(&single("Truth holds."))));
    let proof_only = b"<TeXmacs|2.1.4>\n<\\body>\n  <\\proof>Trivial.</proof>\n</body>\n";
    assert!(!lacks_persisted_ids(&analysis_of(proof_only)));
}

#[test]
fn a_confirmed_block_waits_for_an_unconfirmed_dependency_until_it_is_confirmed() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let fresh = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &fresh, 2);

    // Only the second block is confirmed. Its SIR is reused, its Lean is accepted speculatively,
    // and it waits for the first block, which still has no confirmation.
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:first", vec![], "truth"),
        lean_for(1),
        lean_for(2),
    ]);
    let mut checker = ScriptedChecker::new([ok(1), ok(2)]);
    let batch = run(&source, &mut models, &mut checker, &store);
    assert_eq!(report(&batch, 1).status, BlockStatus::SirDrafted);
    let waiting = report(&batch, 2);
    assert_eq!(waiting.status, BlockStatus::SirDrafted);
    assert_eq!(waiting.sir_origin.as_deref(), Some("confirmed"));
    assert!(waiting.accepted_speculatively);
    assert_eq!(sir_calls(&models), 1);
    assert_eq!(lean_calls(&models), 2);

    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    let second = view_block(&view, 2);
    assert_eq!(second.status, BlockStatus::SirDrafted);
    assert_eq!(second.review, "confirmed");
    assert!(!second.needs_rerun);
    assert!(
        second
            .message
            .starts_with("sir_drafted: confirmed; waiting for dependency confirmation: thm:first"),
        "{}",
        second.message
    );
    assert!(!view.needs_rerun);

    // Confirming the first block settles both, with no rerun: no SIR or cache key changed.
    confirm(&mut store, &source, &batch, 1);
    let settled = review_view(DOCUMENT, &analysis, &batch, &store);
    assert_eq!(
        view_block(&settled, 1).status,
        BlockStatus::StatementConfirmed
    );
    assert_eq!(
        view_block(&settled, 2).status,
        BlockStatus::StatementConfirmed
    );
    assert!(!settled.needs_rerun);
    assert_confirmation_invariant(&settled, &store);
}

#[test]
fn editing_the_text_of_a_confirmed_block_downgrades_it_and_needs_a_rerun() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &batch, 1);
    confirm(&mut store, &source, &batch, 2);
    let before = review_view(DOCUMENT, &analysis, &batch, &store);
    assert!(
        before
            .blocks
            .iter()
            .all(|block| { block.status == BlockStatus::StatementConfirmed && !block.needs_rerun })
    );
    assert!(!before.needs_rerun);

    let edited = document(&[
        (1, "theorem", "Truth holds.", "thm:first"),
        (
            2,
            "theorem",
            "Truth still holds, now by <reference|thm:first>.",
            "thm:second",
        ),
    ]);
    let view = review_view(DOCUMENT, &analysis_of(&edited), &batch, &store);
    let second = view_block(&view, 2);
    assert_eq!(second.status, BlockStatus::SirDrafted);
    assert_eq!(second.review, "confirmed");
    assert!(second.needs_rerun);
    assert!(
        second.message.starts_with("sir_drafted: block changed"),
        "{}",
        second.message
    );
    assert_eq!(view_block(&view, 1).status, BlockStatus::StatementConfirmed);
    assert!(!view_block(&view, 1).needs_rerun);
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn renaming_a_dependency_label_makes_its_dependent_not_formalized() {
    let source = two_blocks("Truth holds.");
    let batch = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &batch, 1);
    confirm(&mut store, &source, &batch, 2);

    let renamed = document(&[
        (1, "theorem", "Truth holds.", "thm:alpha"),
        (
            2,
            "theorem",
            "Truth still holds by <reference|thm:first>.",
            "thm:second",
        ),
    ]);
    let view = review_view(DOCUMENT, &analysis_of(&renamed), &batch, &store);
    let first = view_block(&view, 1);
    assert_eq!(first.status, BlockStatus::NotFormalized);
    assert!(first.needs_rerun);
    assert!(first.message.contains("SIR label"), "{}", first.message);
    let second = view_block(&view, 2);
    assert_eq!(second.status, BlockStatus::NotFormalized);
    assert!(second.needs_rerun);
    assert!(second.message.contains("uses differ"), "{}", second.message);
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn an_edited_dependency_drifts_the_key_of_its_confirmed_dependent() {
    let source = two_blocks("Truth holds.");
    let analysis = analysis_of(&source);
    let batch = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &batch, 1);
    confirm(&mut store, &source, &batch, 2);
    let before = review_view(DOCUMENT, &analysis, &batch, &store);
    assert_eq!(
        view_block(&before, 2).status,
        BlockStatus::StatementConfirmed
    );

    store
        .edit(block(&analysis, 1), &[], &edited_sir_json("edited truth"))
        .unwrap();
    let view = review_view(DOCUMENT, &analysis, &batch, &store);
    let first = view_block(&view, 1);
    assert_eq!(first.status, BlockStatus::SirDrafted);
    assert_eq!(first.review, "edited");
    assert!(first.needs_rerun);
    let second = view_block(&view, 2);
    assert_eq!(second.status, BlockStatus::SirDrafted);
    assert!(second.needs_rerun);
    assert!(
        second
            .message
            .contains("the cache key no longer matches the SIR and its formal dependencies"),
        "{}",
        second.message
    );
    assert!(view.needs_rerun);
    assert_confirmation_invariant(&view, &store);
}

#[test]
fn a_changed_block_without_a_decision_needs_a_rerun() {
    let source = single("Truth holds.");
    let batch = run(
        &source,
        &mut scripted(vec![
            sir(1, "theorem", "thm:first", vec![], "truth"),
            lean_for(1),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &ReviewStore::new(),
    );
    let view = review_view(
        DOCUMENT,
        &analysis_of(&single("Truth fails.")),
        &batch,
        &ReviewStore::new(),
    );
    let block_view = view_block(&view, 1);
    assert_eq!(block_view.status, BlockStatus::SirDrafted);
    assert_eq!(block_view.review, "none");
    assert!(block_view.needs_rerun);
    assert!(view.needs_rerun);
}

#[test]
fn a_changed_sir_never_re_anchors_a_confirmation() {
    let source = single("Truth holds.");
    let first = run(
        &source,
        &mut scripted(vec![
            sir(1, "theorem", "thm:first", vec![], "truth"),
            lean_for(1),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &ReviewStore::new(),
    );
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &first, 1);
    let before = store.clone();

    let changed = single("Truth holds, and more.");
    let changed_analysis = analysis_of(&changed);
    let batch = run(
        &changed,
        &mut scripted(vec![
            sir(1, "theorem", "thm:first", vec![], "truth and more"),
            lean_for(1),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &store,
    );
    assert!(!report(&batch, 1).cosmetic_reconfirmation);
    assert!(!store.anchor_text(block(&changed_analysis, 1), report(&batch, 1)));
    assert_eq!(store, before);
    let view = review_view(DOCUMENT, &changed_analysis, &batch, &store);
    let block_view = view_block(&view, 1);
    assert_eq!(block_view.status, BlockStatus::SirDrafted);
    assert!(block_view.needs_rerun);
}

#[test]
fn a_cosmetic_reconfirmation_that_fails_identity_is_not_re_anchored() {
    let source = two_blocks("Truth holds.");
    let first = fresh_two_block_run(&source);
    let mut store = ReviewStore::new();
    confirm(&mut store, &source, &first, 2);
    let before = store.clone();

    // Renaming the first block removes the link that the confirmed SIR of the second block uses.
    // The second block's text only changes cosmetically, so its SIR keeps the confirmed hash.
    let renamed = document(&[
        (1, "theorem", "Truth holds.", "thm:alpha"),
        (
            2,
            "theorem",
            "Truth still holds, indeed, by <reference|thm:first>.",
            "thm:second",
        ),
    ]);
    let renamed_analysis = analysis_of(&renamed);
    let mut models = scripted(vec![
        sir(1, "theorem", "thm:alpha", vec![], "truth"),
        lean_for(1),
        sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
    ]);
    let mut checker = ScriptedChecker::new([ok(1)]);
    let batch = run(&renamed, &mut models, &mut checker, &store);
    let second = report(&batch, 2);
    assert!(second.cosmetic_reconfirmation);
    assert!(second.cache_key.is_none());
    assert_eq!(second.status, BlockStatus::NotFormalized);
    assert!(!store.anchor_text(block(&renamed_analysis, 2), second));
    assert_eq!(store, before);
}

/// Deterministic xorshift generator, so every scenario replays exactly.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next_u64() % u64::try_from(bound).unwrap()).unwrap()
    }
}

const FIRST_LABELS: [&str; 2] = ["thm:first", "thm:alpha"];
/// Text variants of the two blocks, each with a semantic id. Equal ids give cosmetic edits only.
const FIRST_TEXTS: [(&str, u8); 4] = [
    ("Truth holds.", 1),
    ("Truth  holds.", 1),
    ("Truth fails.", 2),
    ("Truth holds, and more.", 3),
];
const SECOND_TEXTS: [(&str, u8); 3] = [
    ("Truth still holds by <reference|thm:first>.", 1),
    ("Truth  still holds by <reference|thm:first>.", 1),
    ("Truth no longer holds by <reference|thm:first>.", 2),
];

#[derive(Clone, Copy)]
struct Doc {
    first_label: usize,
    first_text: usize,
    second_text: usize,
}

impl Doc {
    fn source(self) -> Vec<u8> {
        document(&[
            (
                1,
                "theorem",
                FIRST_TEXTS[self.first_text].0,
                FIRST_LABELS[self.first_label],
            ),
            (2, "theorem", SECOND_TEXTS[self.second_text].0, "thm:second"),
        ])
    }

    fn oracle(self) -> OracleClient {
        OracleClient {
            labels: [FIRST_LABELS[self.first_label].into(), "thm:second".into()],
            semantics: [
                FIRST_TEXTS[self.first_text].1,
                SECOND_TEXTS[self.second_text].1,
            ],
        }
    }

    /// The `uses` of a block 2 SIR in this document.
    fn second_uses(self) -> Vec<String> {
        if self.first_label == 0 {
            vec!["thm:first".into()]
        } else {
            Vec::new()
        }
    }

    /// A valid hand-written SIR for block `number` (1 or 2).
    fn human_edit(self, number: u8, conclusion: &str) -> (Vec<String>, String) {
        let (label, uses) = if number == 1 {
            (FIRST_LABELS[self.first_label], Vec::new())
        } else {
            ("thm:second", self.second_uses())
        };
        let json = json!({
            "kind": "theorem", "label": label, "variables": [], "hypotheses": [],
            "conclusion": conclusion, "uses": uses, "assumptions_made": []
        })
        .to_string();
        (uses, json)
    }
}

/// What the CLI does after a run: re-anchor cosmetic confirmations, and cache the Lean declaration
/// of each statement_confirmed block.
fn reconcile_like_the_cli(analysis: &Analysis, batch: &BatchReport, store: &mut ReviewStore) {
    for block_report in &batch.blocks {
        if !matches!(
            block_report.sir_origin.as_deref(),
            Some("confirmed" | "edited")
        ) {
            continue;
        }
        let block = analysis
            .blocks
            .iter()
            .find(|block| block.id == block_report.block_id)
            .unwrap();
        store.anchor_text(block, block_report);
        if block_report.status == BlockStatus::StatementConfirmed {
            store
                .record_accepted_lean(
                    block.id,
                    block_report.cache_key.clone().unwrap(),
                    block_report.lean_statement.clone().unwrap(),
                )
                .unwrap();
        }
    }
}

/// The confirmation contract. Every statement_confirmed row has a human decision in effect for its
/// SIR and its current text, a valid identity, and only statement_confirmed formal dependencies.
fn assert_contract(
    analysis: &Analysis,
    store: &ReviewStore,
    rows: &[(BlockId, BlockStatus, Option<Sir>)],
) {
    for (id, status, sir) in rows {
        if *status != BlockStatus::StatementConfirmed {
            continue;
        }
        let block = analysis
            .blocks
            .iter()
            .find(|block| block.id == *id)
            .unwrap();
        let sir = sir.as_ref().expect("a confirmed block shows its SIR");
        match store.entry(*id) {
            Some(ReviewEntry::Confirmed {
                sir_hash,
                text_hash,
                ..
            }) => {
                assert_eq!(*sir_hash, sir.semantic_hash(), "SIR hash of {id}");
                assert_eq!(*text_hash, block.text_hash, "text hash of {id}");
            }
            other => panic!("statement_confirmed without a confirmation for {id}: {other:?}"),
        }
        assert_eq!(sir.kind(), StatementKind::Theorem, "{id}");
        assert_eq!(sir.label(), block.label.as_deref(), "{id}");
        let uses: Vec<String> = block
            .deps
            .iter()
            .filter_map(|dependency| {
                analysis
                    .blocks
                    .iter()
                    .find(|candidate| candidate.id == *dependency)
                    .and_then(|candidate| candidate.label.clone())
            })
            .collect();
        assert_eq!(sir.uses(), uses.as_slice(), "uses of {id}");
        for dependency in &block.deps {
            let status = rows
                .iter()
                .find(|row| row.0 == *dependency)
                .map(|row| row.1);
            assert_eq!(
                status,
                Some(BlockStatus::StatementConfirmed),
                "{id} depends on {dependency}"
            );
        }
    }
}

#[test]
fn random_review_sequences_keep_the_confirmation_contract() {
    let mut confirmed_rows = 0;
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut doc = Doc {
            first_label: 0,
            first_text: 0,
            second_text: 0,
        };
        let mut store = ReviewStore::new();
        let mut last: Option<BatchReport> = None;
        for step in 0..60 {
            match rng.below(8) {
                0 | 1 => {
                    let source = doc.source();
                    let analysis = analysis_of(&source);
                    let batch = run_batch_reviewed(
                        &source,
                        &analysis,
                        &mut doc.oracle(),
                        &mut EchoChecker,
                        config(),
                        &store,
                    )
                    .unwrap();
                    reconcile_like_the_cli(&analysis, &batch, &mut store);
                    let rows: Vec<_> = batch
                        .blocks
                        .iter()
                        .map(|block| (block.block_id, block.status, block.sir.clone()))
                        .collect();
                    assert_contract(&analysis, &store, &rows);
                    last = Some(batch);
                }
                2 | 3 => {
                    if let Some(batch) = &last {
                        let analysis = analysis_of(&doc.source());
                        let number = u8::try_from(1 + rng.below(2)).unwrap();
                        let target = block(&analysis, number);
                        let block_report = report(batch, number);
                        if rng.below(2) == 0 {
                            let _ = store.confirm(target, block_report);
                        } else {
                            let _ = store.reject(target, block_report);
                        }
                    }
                }
                4 => {
                    let analysis = analysis_of(&doc.source());
                    let number = u8::try_from(1 + rng.below(2)).unwrap();
                    let (uses, json) = doc.human_edit(number, &format!("human {step}"));
                    let _ = store.edit(block(&analysis, number), &uses, &json);
                }
                5 => {
                    if rng.below(2) == 0 {
                        doc.first_text = rng.below(FIRST_TEXTS.len());
                    } else {
                        doc.second_text = rng.below(SECOND_TEXTS.len());
                    }
                }
                6 => doc.first_label = 1 - doc.first_label,
                _ => {}
            }

            let analysis = analysis_of(&doc.source());
            let empty = BatchReport {
                diagnostics: Vec::new(),
                blocks: Vec::new(),
                usage: Default::default(),
            };
            let batch = last.as_ref().unwrap_or(&empty);
            let view = review_view(DOCUMENT, &analysis, batch, &store);
            let rows: Vec<_> = view
                .blocks
                .iter()
                .map(|block| (block.block_id, block.status, block.sir.clone()))
                .collect();
            assert_contract(&analysis, &store, &rows);
            for block in &view.blocks {
                if block.status == BlockStatus::StatementConfirmed {
                    assert!(
                        !block.needs_rerun,
                        "seed {seed}, step {step}: {}",
                        block.message
                    );
                }
            }
            confirmed_rows += rows
                .iter()
                .filter(|row| row.1 == BlockStatus::StatementConfirmed)
                .count();
        }
    }
    // The scenarios must actually reach statement_confirmed, or the contract checks nothing.
    assert!(confirmed_rows > 0);
}

#[test]
fn identity_results_are_compared_with_the_last_run() {
    // The run links block 2 to nothing, yet its SIR says it uses thm:first. Identity fails.
    let unlinked = document(&[
        (1, "theorem", "Truth holds.", "thm:alpha"),
        (
            2,
            "theorem",
            "Truth still holds by <reference|thm:first>.",
            "thm:second",
        ),
    ]);
    let batch = run(
        &unlinked,
        &mut scripted(vec![
            sir(1, "theorem", "thm:alpha", vec![], "truth"),
            lean_for(1),
            sir(2, "theorem", "thm:second", vec!["thm:first"], "truth"),
        ]),
        &mut ScriptedChecker::new([ok(1)]),
        &ReviewStore::new(),
    );
    assert_eq!(report(&batch, 2).status, BlockStatus::NotFormalized);
    assert!(report(&batch, 2).cache_key.is_none());

    // The same document again: the failure is what the run reported, so nothing has drifted.
    let same = review_view(
        DOCUMENT,
        &analysis_of(&unlinked),
        &batch,
        &ReviewStore::new(),
    );
    assert_eq!(view_block(&same, 2).status, BlockStatus::NotFormalized);
    assert!(!view_block(&same, 2).needs_rerun);

    // Restoring the label links block 2, so the failure no longer holds and a rerun is needed.
    let restored = document(&[
        (1, "theorem", "Truth holds.", "thm:first"),
        (
            2,
            "theorem",
            "Truth still holds by <reference|thm:first>.",
            "thm:second",
        ),
    ]);
    let view = review_view(
        DOCUMENT,
        &analysis_of(&restored),
        &batch,
        &ReviewStore::new(),
    );
    let second = view_block(&view, 2);
    assert_eq!(second.status, BlockStatus::NotFormalized);
    assert!(second.needs_rerun);
    assert!(
        second.message.contains("the SIR identity changed"),
        "{}",
        second.message
    );
    assert!(view.needs_rerun);
}
