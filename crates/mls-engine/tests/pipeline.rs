//! Recorded end-to-end acceptance tests for the M2 batch pipeline.

mod common;

use common::*;
use mls_core::BlockId;
use mls_engine::{
    BlockStatus, CheckScope, CheckState, LeanStatementChecker, StatementChecker, run_batch,
};
use mls_lean::{Binder, ExtractedDeclaration, LeanWorker, Position, ReplConfig};
use mls_models::{
    DecisionReply, DecisionRequest, GenerationReply, GenerationRequest, ModelClient, ModelError,
    RecordingClient, ReplayClient,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Duration;

fn source(name: &str) -> Vec<u8> {
    std::fs::read(
        fixture_root()
            .join("lean/pipeline")
            .join(format!("{name}.tm")),
    )
    .unwrap()
}

fn replay(name: &str) -> ReplayClient {
    ReplayClient::from_directory(
        fixture_root().join("recordings/pipeline").join(name),
        GENERATION_MODEL,
        DECISION_MODEL,
    )
    .unwrap()
}

#[test]
fn replay_success_has_deterministic_draft_report() {
    let mut models = replay("success");
    let mut checker = ScriptedChecker::new([
        CheckStep::Extracted(extracted(1, "True")),
        CheckStep::Extracted(extracted(2, "True")),
    ]);
    let report = run_batch(&source("success"), &mut models, &mut checker, config()).unwrap();

    assert_eq!(report.blocks.len(), 2);
    assert!(report.blocks.iter().all(|block| {
        block.status == BlockStatus::SirDrafted
            && block.elaborated
            && block.accepted_speculatively
            && !block.complete_structure
            && block.attempts == 1
    }));
    assert_eq!(checker.finishes, [true, true]);
    assert_eq!(report.usage.calls, 4);
    assert_eq!(report.usage.input_tokens, 40);
    assert_eq!(report.usage.output_tokens, 20);
    assert_eq!(report.usage.known_cost, 0.04);
    assert_eq!(report.usage.unknown_cost_calls, 0);
    let actual = serde_json::to_value(&report).unwrap();
    let expected: Value = serde_json::from_slice(
        &std::fs::read(fixture_root().join("lean/pipeline/success-report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn replay_repair_uses_feedback_and_keeps_the_draft_unconfirmed() {
    let mut models = replay("repair");
    let mut checker = ScriptedChecker::new([
        CheckStep::Error("unknown identifier `MissingName`"),
        CheckStep::Extracted(extracted(3, "True")),
    ]);
    let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();
    let block = &report.blocks[0];

    assert_eq!(block.status, BlockStatus::SirDrafted);
    assert!(block.accepted_speculatively);
    assert_eq!(block.attempts, 2);
    assert_eq!(checker.finishes, [true]);
    assert!(checker.checked_sources[0].contains("MissingName"));
    assert!(checker.checked_sources[1].contains(": True"));
    assert!(block.errors.is_empty());
}

#[test]
fn admission_rejection_repair_prompt_carries_the_original_message_and_contract_hint() {
    let original = "Lean: line 1, column 0: invalid MLS declaration source: <mls-declaration>:3:0: expected end of input";
    let mut models = CapturingClient {
        replies: VecDeque::from([
            Ok(sir(3, "theorem", "thm:repair", vec![], "truth")),
            Ok(lean(
                "set_option autoImplicit false\n\ntheorem mls_00000000_0000_4000_8000_000000000003 : True := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : True := by sorry",
            )),
        ]),
        requests: Vec::new(),
    };
    let mut checker = ScriptedChecker::new([
        CheckStep::Error(original),
        CheckStep::Extracted(extracted(3, "True")),
    ]);
    let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();

    assert!(report.blocks[0].accepted_speculatively);
    assert_eq!(models.requests.len(), 3);
    let prompt = models.requests[2].messages[1].content.as_str();
    assert!(prompt.contains(original));
    assert!(prompt.contains("Admission contract: lean_statement must be exactly one declaration"));
    assert!(prompt.contains("theorem body must be exactly `by sorry`"));
    assert!(!prompt.contains("is not admitted and must be expressed differently"));
}

#[test]
fn hard_mismatch_exhausts_attempts_and_blocks_dependent_without_model_calls() {
    let mut models = replay("blocked");
    let mut wrong = extracted(4, "False");
    wrong.goal = "False".into();
    let mut checker = ScriptedChecker::new([
        CheckStep::Extracted(wrong.clone()),
        CheckStep::Extracted(wrong),
    ]);
    let report = run_batch(&source("blocked"), &mut models, &mut checker, config()).unwrap();

    let failed = &report.blocks[0];
    assert_eq!(failed.status, BlockStatus::FormalizationError);
    assert!(!failed.accepted_speculatively);
    assert_eq!(failed.attempts, 2);
    assert_eq!(checker.finishes, [false, false]);
    assert!(
        failed
            .errors
            .iter()
            .any(|error| error.contains("goal.outer_head"))
    );

    let dependent = &report.blocks[1];
    assert_eq!(dependent.status, BlockStatus::NotFormalized);
    assert_eq!(dependent.attempts, 0);
    assert!(dependent.sir.is_none());
    assert_eq!(
        dependent.errors,
        ["a formal statement dependency was not accepted speculatively"]
    );
    assert_eq!(report.usage.calls, 3);
}

#[test]
fn recorded_hard_errors_are_bounded_by_max_attempts() {
    let mut models = replay("exhaustion");
    let mut checker = ScriptedChecker::new([]);
    let report = run_batch(&source("exhaustion"), &mut models, &mut checker, config()).unwrap();
    let block = &report.blocks[0];
    assert_eq!(block.status, BlockStatus::FormalizationError);
    assert_eq!(block.attempts, 2);
    assert!(!block.elaborated);
    assert!(!block.accepted_speculatively);
    assert_eq!(block.errors, ["transport error: synthetic outage 2"]);
    assert_eq!(report.usage.calls, 3);
    assert_eq!(report.usage.unknown_input_token_calls, 2);
    assert_eq!(report.usage.unknown_output_token_calls, 2);
    assert_eq!(report.usage.unknown_cost_calls, 2);
    assert!(checker.checked_sources.is_empty());
}

#[test]
fn replay_rejects_even_a_small_prompt_contract_change() {
    let mut models = replay("success");
    let mut checker = ScriptedChecker::new([]);
    let mut changed = config();
    changed.sir_max_tokens = 4095;
    let report = run_batch(&source("success"), &mut models, &mut checker, changed).unwrap();
    assert_eq!(report.blocks[0].status, BlockStatus::NotFormalized);
    assert_eq!(
        report.blocks[0].errors,
        ["no exact replay match for request"]
    );
    assert_eq!(
        report.blocks[1].errors,
        ["a formal statement dependency was not accepted speculatively"]
    );
    assert!(checker.checked_sources.is_empty());
    // A replay miss never reached a provider, so it is not a counted model call.
    assert_eq!(report.usage.calls, 0);
}

#[test]
fn complete_structure_requires_a_recognized_matched_outer_head() {
    let run = |conclusion: &str| {
        let mut models = CapturingClient {
            replies: VecDeque::from([
                Ok(sir(3, "theorem", "thm:repair", vec![], conclusion)),
                Ok(lean(
                    "theorem mls_00000000_0000_4000_8000_000000000003 : True := by sorry",
                )),
            ]),
            requests: Vec::new(),
        };
        let mut checker = ScriptedChecker::new([CheckStep::Extracted(extracted(3, "eq"))]);
        let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();
        assert_eq!(checker.finishes, [true]);
        report.blocks.into_iter().next().unwrap()
    };

    let recognized = run("0 = 0");
    assert!(recognized.accepted_speculatively);
    assert!(recognized.complete_structure);
    assert!(
        recognized.checks.iter().any(|check| {
            check.field == "goal.outer_head" && check.state == CheckState::Matched
        })
    );

    let unrecognized = run("0 = 0 and P");
    assert!(unrecognized.accepted_speculatively);
    assert!(!unrecognized.complete_structure);
    assert!(unrecognized.checks.iter().any(|check| {
        check.field == "goal.outer_head"
            && check.state == CheckState::Unchecked
            && check.scope == CheckScope::Structure
    }));
}

#[test]
fn replay_dependency_prompt_contains_generated_name_declaration_and_sir() {
    let mut models = replay("dependency");
    let definition = ExtractedDeclaration {
        declaration: "mls_00000000_0000_4000_8000_000000000007".into(),
        kind: "definition".into(),
        binders: vec![Binder {
            name: "x".into(),
            type_text: "Nat".into(),
            is_proposition: false,
            binder_info: "explicit".into(),
        }],
        goal: "Nat".into(),
        goal_outer_head: "Nat".into(),
        axioms: Vec::new(),
        has_sorry: false,
        body: Some("x".into()),
        body_outer_head: Some("free_variable".into()),
    };
    let mut equality = extracted(8, "eq");
    equality.goal = "mls_00000000_0000_4000_8000_000000000007 0 = 0".into();
    let mut checker = ScriptedChecker::new([
        CheckStep::Extracted(definition),
        CheckStep::Extracted(equality),
    ]);
    let report = run_batch(&source("dependency"), &mut models, &mut checker, config()).unwrap();
    assert!(
        report
            .blocks
            .iter()
            .all(|block| block.accepted_speculatively)
    );

    let recorded: Value = serde_json::from_slice(
        &std::fs::read(fixture_root().join("recordings/pipeline/dependency/call-0003.json"))
            .unwrap(),
    )
    .unwrap();
    let prompt = recorded["request"]["messages"][1]["content"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("mls_00000000_0000_4000_8000_000000000007"));
    assert!(prompt.contains("def mls_00000000_0000_4000_8000_000000000007 (x : Nat) : Nat := x"));
    assert!(prompt.contains("\"label\":\"def:identity\""));
}

#[test]
fn elaboration_failure_after_mismatch_leaves_no_stale_evidence() {
    let mut models = CapturingClient {
        replies: VecDeque::from([
            Ok(sir(3, "theorem", "thm:repair", vec![], "0 = 0")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : False := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : MissingName := by sorry",
            )),
        ]),
        requests: Vec::new(),
    };
    let mut wrong = extracted(3, "False");
    wrong.goal = "False".into();
    let mut checker = ScriptedChecker::new([
        CheckStep::Extracted(wrong),
        CheckStep::ErrorWithDiagnostic(
            "unknown identifier 'MissingName'",
            Position {
                line: 1,
                column: 45,
            },
        ),
    ]);
    let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();

    assert_eq!(report.blocks.len(), 1);
    let block = &report.blocks[0];
    assert_eq!(block.status, BlockStatus::FormalizationError);
    assert_eq!(block.attempts, 2);
    assert!(!block.elaborated);
    assert!(!block.accepted_speculatively);
    assert!(!block.complete_structure);
    assert!(block.checks.is_empty());
    assert!(block.sir.is_some());
    assert_eq!(
        block.lean_statement.as_deref(),
        Some("theorem mls_00000000_0000_4000_8000_000000000003 : MissingName := by sorry")
    );
    assert_eq!(block.errors, ["Lean: unknown identifier 'MissingName'"]);
    assert!(
        block
            .errors
            .iter()
            .all(|error| !error.contains("goal.outer_head"))
    );
    // The retained diagnostic is the attempt-2 one, mapped to the source block; nothing from
    // attempt 1 (which had no diagnostic) and no stale check survives.
    assert_eq!(block.lean_diagnostics.len(), 1);
    let diagnostic = &block.lean_diagnostics[0];
    assert_eq!(diagnostic.message, "unknown identifier 'MissingName'");
    assert_eq!(diagnostic.line, Some(1));
    assert_eq!(diagnostic.column, Some(45));
    assert_eq!(diagnostic.block_id, block.block_id);
    assert_eq!(diagnostic.source_start, block.source_start);
    assert_eq!(diagnostic.source_end, block.source_end);
    assert_eq!(checker.finishes, [false]);

    assert_eq!(models.requests.len(), 3);
    let repair_prompt = &models.requests[2].messages[1].content;
    assert!(
        repair_prompt.contains("goal.outer_head: expected equality, got False"),
        "{repair_prompt}"
    );
}

#[test]
fn diagnostics_from_a_failed_attempt_do_not_survive_a_later_hard_mismatch() {
    let mut models = CapturingClient {
        replies: VecDeque::from([
            Ok(sir(3, "theorem", "thm:repair", vec![], "0 = 0")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : MissingName := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : False := by sorry",
            )),
        ]),
        requests: Vec::new(),
    };
    let mut wrong = extracted(3, "False");
    wrong.goal = "False".into();
    let mut checker = ScriptedChecker::new([
        CheckStep::ErrorWithDiagnostic(
            "unknown identifier 'MissingName'",
            Position {
                line: 1,
                column: 45,
            },
        ),
        CheckStep::Extracted(wrong),
    ]);
    let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();

    let block = &report.blocks[0];
    assert_eq!(block.status, BlockStatus::FormalizationError);
    assert_eq!(block.attempts, 2);
    assert!(block.elaborated);
    assert!(!block.accepted_speculatively);
    assert_eq!(
        block.lean_statement.as_deref(),
        Some("theorem mls_00000000_0000_4000_8000_000000000003 : False := by sorry")
    );
    // Attempt 1's diagnostic must not survive: attempt 2 elaborated, so it reports none.
    assert!(block.lean_diagnostics.is_empty());
    // Only attempt 2's comparison is present, and its feedback replaced attempt 1's error.
    let heads: Vec<_> = block
        .checks
        .iter()
        .filter(|check| check.field == "goal.outer_head")
        .collect();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].state, CheckState::Mismatch);
    assert_eq!(heads[0].detail, "expected equality, got False");
    assert!(
        block
            .errors
            .iter()
            .any(|error| error.contains("goal.outer_head"))
    );
    assert!(
        block
            .errors
            .iter()
            .all(|error| !error.contains("MissingName"))
    );
    assert_eq!(checker.finishes, [false]);
}

#[test]
fn vacuous_hypothesis_padding_is_repaired_with_structured_feedback() {
    let mut models = CapturingClient {
        replies: VecDeque::from([
            Ok(reply(
                3,
                json!({
                    "kind": "theorem",
                    "label": "thm:repair",
                    "variables": [],
                    "hypotheses": ["0 < 1"],
                    "conclusion": "0 = 0",
                    "uses": [],
                    "assumptions_made": []
                }),
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 (h0 : True) : 0 = 0 := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 (h0 : 0 < 1) : 0 = 0 := by sorry",
            )),
        ]),
        requests: Vec::new(),
    };
    let binder = |type_text: &str| Binder {
        name: "h0".into(),
        type_text: type_text.into(),
        is_proposition: true,
        binder_info: "explicit".into(),
    };
    let padded = ExtractedDeclaration {
        binders: vec![binder("True")],
        ..extracted(3, "eq")
    };
    let stated = ExtractedDeclaration {
        binders: vec![binder("0 < 1")],
        ..extracted(3, "eq")
    };
    let mut checker =
        ScriptedChecker::new([CheckStep::Extracted(padded), CheckStep::Extracted(stated)]);
    let report = run_batch(&source("repair"), &mut models, &mut checker, config()).unwrap();

    let block = &report.blocks[0];
    assert_eq!(block.attempts, 2);
    assert!(block.accepted_speculatively);
    assert!(block.errors.is_empty());
    assert_eq!(checker.finishes, [false, true]);
    let repair_prompt = &models.requests[2].messages[1].content;
    assert!(
        repair_prompt.contains("hypotheses[0].vacuous: hypothesis is the vacuous proposition True"),
        "{repair_prompt}"
    );
}

#[test]
fn non_utf8_block_is_its_own_error_and_does_not_abort_the_batch() {
    // The first theorem is valid. The second contains a raw 0xE9 byte: valid Latin-1 but not UTF-8.
    // The parser preserves it, and it must not be sent to a model as text.
    let mut document = b"<TeXmacs|2.1.4>\n<\\body>\n  <section|First>\n  <\\with|mls-id|00000000-0000-4000-8000-000000000001><\\theorem>Truth holds.<label|thm:first></theorem></with>\n  <section|Second>\n  <\\with|mls-id|00000000-0000-4000-8000-000000000002><\\theorem>Caf".to_vec();
    document.push(0xE9);
    document.extend_from_slice(b"<label|thm:second></theorem></with>\n</body>\n");
    let mut models = CapturingClient {
        replies: VecDeque::from([
            Ok(sir(1, "theorem", "thm:first", vec![], "truth")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000001 : True := by sorry",
            )),
        ]),
        requests: Vec::new(),
    };
    let mut checker = ScriptedChecker::new([CheckStep::Extracted(extracted(1, "True"))]);
    let report = run_batch(&document, &mut models, &mut checker, config()).unwrap();

    assert_eq!(report.blocks.len(), 2);
    let first = &report.blocks[0];
    assert_eq!(first.status, BlockStatus::SirDrafted);
    assert!(first.accepted_speculatively);
    let second = &report.blocks[1];
    assert_eq!(second.status, BlockStatus::NotFormalized);
    assert_eq!(second.attempts, 0);
    assert!(second.sir.is_none());
    assert!(second.lean_statement.is_none());
    assert_eq!(
        second.errors,
        ["block source is not valid UTF-8; it was not sent to a model"]
    );
    // Only the first block reached a model, and its accounting survives the second block.
    assert_eq!(models.requests.len(), 2);
    assert_eq!(checker.finishes, [true]);
    assert_eq!(report.usage.calls, 2);
    assert_eq!(report.usage.input_tokens, 20);
    assert_eq!(report.usage.output_tokens, 10);
}

fn spawn_real_worker() -> LeanWorker {
    let repository = fixture_root()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    LeanWorker::spawn(ReplConfig {
        program: repository.join("scripts/run-lean-repl.sh"),
        args: vec![repository.join(".mls").to_string_lossy().into_owned()],
        cwd: repository,
        env: Vec::new(),
        timeout: Duration::from_secs(180),
        max_stdout_bytes: 1024 * 1024,
        max_stderr_bytes: 64 * 1024,
    })
    .unwrap()
}

#[test]
fn real_worker_repair_feedback_contains_lean_diagnostic_and_source_mapping() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let mut worker = spawn_real_worker();
    let replies = VecDeque::from([
        Ok(sir(3, "theorem", "thm:repair", vec![], "truth")),
        Ok(lean(
            "theorem mls_00000000_0000_4000_8000_000000000003 : MissingName := by sorry",
        )),
        Ok(lean(
            "theorem mls_00000000_0000_4000_8000_000000000003 : True := by sorry",
        )),
    ]);
    let mut models = CapturingClient {
        replies,
        requests: Vec::new(),
    };
    let mut checker = LeanStatementChecker::new(&mut worker);
    let document = source("repair");
    let report = run_batch(&document, &mut models, &mut checker, config()).unwrap();
    let block = &report.blocks[0];
    assert!(block.accepted_speculatively);
    assert_eq!(block.attempts, 2);
    assert_eq!(
        &document[block.source_start..block.source_end],
        b"<\\theorem>Truth holds.<label|thm:repair></theorem>"
    );
    let repair_prompt = &models.requests[2].messages[1].content;
    assert!(repair_prompt.contains("MissingName"), "{repair_prompt}");
    assert!(
        repair_prompt.contains("unknown identifier"),
        "{repair_prompt}"
    );
    assert_eq!(models.requests[2].context.attempt, 1);
}

fn real_declaration(index: u8) -> (BlockId, String) {
    let id: BlockId = format!("00000000-0000-4000-8000-{index:012}")
        .parse()
        .unwrap();
    (id, format!("mls_00000000_0000_4000_8000_{index:012}"))
}

#[test]
fn interleaved_checkers_keep_pending_candidates_isolated() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let (id_a, name_a) = real_declaration(21);
    let (id_b, name_b) = real_declaration(22);
    let (id_c, name_c) = real_declaration(23);
    let (id_d, name_d) = real_declaration(24);
    let (id_e, name_e) = real_declaration(25);
    let (id_f, name_f) = real_declaration(26);
    let mut worker_a = spawn_real_worker();
    let mut worker_b = spawn_real_worker();
    let mut checker_a = LeanStatementChecker::new(&mut worker_a);
    let mut checker_b = LeanStatementChecker::new(&mut worker_b);

    // Both candidates are pending at the same time, one per checker.
    checker_a
        .check(
            id_a,
            &name_a,
            &format!("theorem {name_a} : True := by sorry"),
        )
        .unwrap();
    checker_b
        .check(
            id_b,
            &name_b,
            &format!("theorem {name_b} : True := by sorry"),
        )
        .unwrap();
    checker_a.finish(true).unwrap();
    checker_b.finish(false).unwrap();

    // A committed its own candidate, so a declaration referencing it elaborates.
    checker_a
        .check(id_c, &name_c, &format!("def {name_c} : True := {name_a}"))
        .unwrap();
    checker_a.finish(false).unwrap();

    // A never received B's candidate.
    let error = checker_a
        .check(id_d, &name_d, &format!("def {name_d} : True := {name_b}"))
        .unwrap_err();
    assert!(error.contains(&name_b), "{error}");

    // B discarded its candidate and never received A's accepted declaration.
    let error = checker_b
        .check(id_e, &name_e, &format!("def {name_e} : True := {name_a}"))
        .unwrap_err();
    assert!(error.contains(&name_a), "{error}");
    let error = checker_b
        .check(id_f, &name_f, &format!("def {name_f} : True := {name_b}"))
        .unwrap_err();
    assert!(error.contains(&name_b), "{error}");
}

// The checked-in recordings are generated through RecordingClient. This is deliberately opt-in so
// normal tests can only consume immutable fixtures through ReplayClient.
#[test]
fn update_synthetic_recordings() {
    if std::env::var("MLS_UPDATE_PIPELINE_FIXTURES").as_deref() != Ok("1") {
        return;
    }
    write_recording(
        "success",
        vec![
            Ok(sir(1, "theorem", "thm:first", vec![], "truth")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000001 : True := by sorry",
            )),
            Ok(sir(2, "theorem", "thm:second", vec!["thm:first"], "truth")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000002 : True := by sorry",
            )),
        ],
        [
            CheckStep::Extracted(extracted(1, "True")),
            CheckStep::Extracted(extracted(2, "True")),
        ],
    );
    write_recording(
        "repair",
        vec![
            Ok(sir(3, "theorem", "thm:repair", vec![], "truth")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : MissingName := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000003 : True := by sorry",
            )),
        ],
        [
            CheckStep::Error("unknown identifier `MissingName`"),
            CheckStep::Extracted(extracted(3, "True")),
        ],
    );
    let mut wrong = extracted(4, "False");
    wrong.goal = "False".into();
    write_recording(
        "blocked",
        vec![
            Ok(sir(4, "theorem", "thm:failed", vec![], "0 = 0")),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000004 : False := by sorry",
            )),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000004 : False := by sorry",
            )),
        ],
        [
            CheckStep::Extracted(wrong.clone()),
            CheckStep::Extracted(wrong),
        ],
    );
    write_recording(
        "exhaustion",
        vec![
            Ok(sir(6, "theorem", "thm:exhaustion", vec![], "truth")),
            Err(ModelError::Transport("synthetic outage 1".into())),
            Err(ModelError::Transport("synthetic outage 2".into())),
        ],
        [],
    );
    let definition_sir = reply(
        7,
        json!({
            "kind": "definition",
            "label": "def:identity",
            "variables": [{"name": "x", "type": "natural number"}],
            "hypotheses": [],
            "conclusion": "x",
            "uses": [],
            "assumptions_made": []
        }),
    );
    let theorem_sir = sir(
        8,
        "theorem",
        "thm:identity-zero",
        vec!["def:identity"],
        "identity of zero = zero",
    );
    let definition = ExtractedDeclaration {
        declaration: "mls_00000000_0000_4000_8000_000000000007".into(),
        kind: "definition".into(),
        binders: vec![Binder {
            name: "x".into(),
            type_text: "Nat".into(),
            is_proposition: false,
            binder_info: "explicit".into(),
        }],
        goal: "Nat".into(),
        goal_outer_head: "Nat".into(),
        axioms: Vec::new(),
        has_sorry: false,
        body: Some("x".into()),
        body_outer_head: Some("free_variable".into()),
    };
    let mut theorem = extracted(8, "eq");
    theorem.goal = "mls_00000000_0000_4000_8000_000000000007 0 = 0".into();
    write_recording(
        "dependency",
        vec![
            Ok(definition_sir),
            Ok(lean(
                "def mls_00000000_0000_4000_8000_000000000007 (x : Nat) : Nat := x",
            )),
            Ok(theorem_sir),
            Ok(lean(
                "theorem mls_00000000_0000_4000_8000_000000000008 : mls_00000000_0000_4000_8000_000000000007 0 = 0 := by sorry",
            )),
        ],
        [
            CheckStep::Extracted(definition),
            CheckStep::Extracted(theorem),
        ],
    );
}

fn write_recording<const N: usize>(
    name: &str,
    replies: Vec<Result<GenerationReply, ModelError>>,
    checks: [CheckStep; N],
) {
    let directory = fixture_root().join("recordings/pipeline").join(name);
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let client = QueueClient(replies.into());
    let mut recorder = RecordingClient::new(
        client,
        directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic: mls-engine pipeline acceptance fixture",
    );
    let mut checker = ScriptedChecker::new(checks);
    let report = run_batch(&source(name), &mut recorder, &mut checker, config()).unwrap();
    if name == "success" {
        std::fs::write(
            fixture_root().join("lean/pipeline/success-report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}

struct QueueClient(VecDeque<Result<GenerationReply, ModelError>>);

impl ModelClient for QueueClient {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        self.0.pop_front().expect("missing synthetic response")
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        unreachable!("M2 does not make decision calls")
    }
}
