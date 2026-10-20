//! Acceptance tests for live mode (M4): incremental invalidation, gating, scheduling, stale-result
//! discard, caches, review actions and the pipeline runner. Model replies and the Lean checker are
//! scripted, so every model call is counted and unexpected calls fail the test.

mod common;

use common::*;
use mls_core::BlockId;
use mls_engine::live::{
    ActionError, ChangeSet, Clock, Completion, Document, Engine, EngineConfig, Finished,
    FinishedOracle, FinishedQuery, GateConfig, JobOutcome, JobRunner, LeanCache, ManualClock,
    OracleAnswer, PipelineRunner, ReviewAction, SirCache, SirProvenance, Stale, sir_cache_key,
};
use mls_engine::{BlockStatus, ReviewStore, ReviewView, UsageSummary};
use mls_models::{GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelError};
use mls_sir::Sir;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The block identity of the document's `n`th block.
fn id(number: u8) -> BlockId {
    format!("00000000-0000-4000-8000-{number:012}")
        .parse()
        .unwrap()
}

/// One block of a generated test document.
#[derive(Clone, Debug)]
struct Spec {
    number: u8,
    text: String,
    /// Numbers of the blocks it references, in reference order.
    deps: Vec<u8>,
}

fn label(number: u8) -> String {
    format!("thm:b{number}")
}

/// Render blocks in the given order. `indent` and `gap` change layout only.
fn render(blocks: &[Spec], indent: &str, gap: &str) -> Vec<u8> {
    let mut source = format!("<TeXmacs|2.1.4>\n<\\body>\n{gap}");
    for block in blocks {
        let refs: String = block
            .deps
            .iter()
            .map(|dep| format!(" <reference|{}>", label(*dep)))
            .collect();
        source.push_str(&format!(
            "{indent}<\\with|mls-id|{}><\\theorem>{}{refs}<label|{}></theorem></with>\n{gap}",
            id(block.number),
            block.text,
            label(block.number)
        ));
    }
    source.push_str("</body>\n");
    source.into_bytes()
}

/// A plain chain of three blocks: 1 stands alone, 2 uses 1, 3 stands alone.
fn three(first_text: &str) -> Vec<Spec> {
    vec![
        Spec {
            number: 1,
            text: first_text.into(),
            deps: vec![],
        },
        Spec {
            number: 2,
            text: "Second claim.".into(),
            deps: vec![1],
        },
        Spec {
            number: 3,
            text: "Independent claim.".into(),
            deps: vec![],
        },
    ]
}

/// Scripted generation: each block's SIR comes from its spec, and every SIR call is counted.
struct Model {
    specs: BTreeMap<BlockId, (String, Vec<String>, String)>,
    sir_calls: Vec<BlockId>,
    lean_calls: usize,
}

impl Model {
    /// Specs for numbered blocks: label, uses, and a conclusion that names the block.
    fn for_blocks(blocks: &[Spec]) -> Self {
        let mut model = Self {
            specs: BTreeMap::new(),
            sir_calls: Vec::new(),
            lean_calls: 0,
        };
        for block in blocks {
            model.set(
                block.number,
                &block.deps,
                &format!("claim {}", block.number),
            );
        }
        model
    }

    fn set(&mut self, number: u8, deps: &[u8], conclusion: &str) {
        self.specs.insert(
            id(number),
            (
                label(number),
                deps.iter().map(|dep| label(*dep)).collect(),
                conclusion.to_owned(),
            ),
        );
    }

    fn sir_count(&self) -> usize {
        self.sir_calls.len()
    }
}

impl ModelClient for Model {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        if request.stage != GenerationStage::Sir {
            self.lean_calls += 1;
            return Ok(lean("theorem placeholder : True := by sorry"));
        }
        let block: BlockId = request.context.block_id.parse().unwrap();
        self.sir_calls.push(block);
        let (label, uses, conclusion) = self
            .specs
            .get(&block)
            .unwrap_or_else(|| panic!("no SIR spec for {block}"))
            .clone();
        Ok(reply(
            1,
            json!({
                "kind": "theorem",
                "label": label,
                "variables": [],
                "hypotheses": [],
                "conclusion": conclusion,
                "uses": uses,
                "assumptions_made": []
            }),
        ))
    }

    fn decide(
        &mut self,
        _: &mls_models::DecisionRequest,
    ) -> Result<mls_models::DecisionReply, ModelError> {
        unreachable!("live mode makes no decision calls in M4")
    }
}

fn engine_with(debounce_ms: u64) -> Engine {
    Engine::new(
        EngineConfig {
            document: "doc.tm".into(),
            model: GENERATION_MODEL.into(),
            gate: GateConfig {
                debounce_ms,
                ..GateConfig::default()
            },
            sir_cache_capacity: 64,
            lean_cache_capacity: 64,
        },
        ReviewStore::new(),
    )
}

/// Run every eligible job, with the time far past any debounce. Returns the number of jobs run.
fn settle(engine: &mut Engine, models: &mut Model, checker: &mut EchoChecker) -> usize {
    let mut runs = 0;
    while engine
        .run_next(
            &mut PipelineRunner::new(&mut *models, &mut *checker, config()),
            1_000_000,
        )
        .is_some()
    {
        runs += 1;
        assert!(runs < 1_000, "scheduler does not settle");
    }
    runs
}

fn status(view: &ReviewView, number: u8) -> BlockStatus {
    view.blocks
        .iter()
        .find(|block| block.block_id == id(number))
        .unwrap_or_else(|| panic!("block {number} is missing from the view"))
        .status
}

/// Confirm every block of the view that has a SIR and an accepted statement.
fn confirm_all(engine: &mut Engine, numbers: &[u8]) {
    for number in numbers {
        engine
            .on_review(ReviewAction::Confirm(id(*number)), 1_000_000)
            .unwrap();
    }
}

#[test]
fn typing_in_the_active_block_never_schedules_it() {
    let mut engine = engine_with(0);
    let blocks = three("Truth holds.");
    engine
        .on_change(&render(&blocks, "  ", "\n"), Some(id(1)), 0)
        .unwrap();

    // The block under the cursor is never eligible, however long ago it changed. Its dependent
    // waits behind it, and the unrelated block is scheduled instead.
    let job = engine
        .next_job(1_000_000)
        .expect("the unrelated block is eligible");
    assert_eq!(job.block, id(3));
    assert!(engine.next_job(1_000_000).is_none());

    let typed = three("Truth holds again.");
    engine
        .on_change(&render(&typed, "  ", "\n"), Some(id(1)), 2_000_000)
        .unwrap();
    assert!(engine.next_job(3_000_000).is_none());
}

#[test]
fn leaving_a_block_schedules_it_first_then_its_dependents() {
    let mut models = Model::for_blocks(&three("Truth holds."));
    let mut checker = EchoChecker;
    let mut engine = engine_with(800);
    engine
        .on_change(&render(&three("Truth holds."), "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    assert!(engine.dirty().is_empty());

    let edited = three("Truth holds, edited.");
    engine
        .on_change(&render(&edited, "  ", "\n"), Some(id(1)), 2_000_000)
        .unwrap();
    assert!(
        engine.next_job(2_000_000).is_none(),
        "the active block and its waiting dependent are not eligible"
    );

    engine.on_cursor(Some(id(3)));
    let first = engine.next_job(2_000_000).expect("the block just left");
    assert_eq!(first.block, id(1));
    assert_eq!(first.priority, mls_engine::live::Priority::JustLeft);
    let outcome = {
        let context = engine.context(&first).unwrap();
        PipelineRunner::new(&mut models, &mut checker, config()).run(&first, &context)
    };
    assert_eq!(engine.complete(first, outcome), Completion::Applied);

    let second = engine.next_job(2_900_000).expect("the dependent");
    assert_eq!(second.block, id(2));
    assert_eq!(second.priority, mls_engine::live::Priority::Dependent);
}

#[test]
fn debounce_is_measured_with_the_injected_clock() {
    let clock = ManualClock::new(0);
    let mut models = Model::for_blocks(&three("Truth holds."));
    let mut checker = EchoChecker;
    let mut engine = engine_with(800);
    engine
        .on_change(
            &render(&three("Truth holds."), "  ", "\n"),
            None,
            clock.now_ms(),
        )
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);

    clock.advance(5_000);
    let mut changed = three("Truth holds.");
    changed[2].text = "Independent claim, reworded.".into();
    engine
        .on_change(&render(&changed, "  ", "\n"), None, clock.now_ms())
        .unwrap();
    clock.advance(799);
    assert!(engine.next_job(clock.now_ms()).is_none());
    clock.advance(1);
    let job = engine.next_job(clock.now_ms()).expect("debounce elapsed");
    assert_eq!(job.block, id(3));
}

#[test]
fn stale_result_is_discarded_when_the_block_changes_mid_job() {
    let mut models = Model::for_blocks(&three("Truth holds."));
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&three("Truth holds."), "  ", "\n"), None, 0)
        .unwrap();
    let job = engine.next_job(10).expect("a job");
    assert_eq!(job.block, id(1));
    let outcome = {
        let context = engine.context(&job).unwrap();
        PipelineRunner::new(&mut models, &mut checker, config()).run(&job, &context)
    };
    engine
        .on_change(
            &render(&three("Truth holds, typed meanwhile."), "  ", "\n"),
            None,
            20,
        )
        .unwrap();

    assert_eq!(
        engine.complete(job, outcome),
        Completion::Stale(Stale::TextChanged)
    );
    assert!(engine.dirty().contains(&id(1)), "the block stays dirty");
    assert!(
        engine.usage().calls > 0,
        "discarded work still incurs model usage"
    );
    assert!(engine.sir_cache().is_empty(), "a stale SIR is not cached");
    assert!(
        engine.lean_cache().is_empty(),
        "a stale declaration is not cached"
    );
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.block_id != id(1) || block.status == BlockStatus::NotFormalized)
    );
}

#[test]
fn transitive_upstream_invalidation_discards_an_in_flight_dependent() {
    // A -> B -> C. Changing A leaves B's old SIR hash intact while B is dirty.
    let mut blocks = three("Truth holds.");
    blocks[2].deps = vec![2];
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    assert!(engine.retry(id(3), 10));
    let job = engine.next_job(10).unwrap();
    let outcome = PipelineRunner::new(&mut models, &mut checker, config())
        .run(&job, &engine.context(&job).unwrap());
    blocks[0].text.push_str(" Reworded.");
    engine
        .on_change(&render(&blocks, "", "\n"), None, 20)
        .unwrap();
    let upstream = engine.next_job(20).unwrap();
    assert_eq!(upstream.block, id(1));
    assert_eq!(
        engine.complete(job, outcome),
        Completion::Stale(Stale::DependencyChanged)
    );
    assert!(engine.dirty().contains(&id(3)));
    assert!(engine.abandon(&upstream));
    settle(&mut engine, &mut models, &mut checker);
    assert!(engine.dirty().is_empty());
}

#[test]
fn invalidation_is_remembered_after_upstream_jobs_settle_with_the_same_sir() {
    let mut blocks = three("Truth holds.");
    blocks[2].deps = vec![2];
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    engine.retry(id(3), 10);
    let job = engine.next_job(10).unwrap();
    let outcome = PipelineRunner::new(&mut models, &mut checker, config())
        .run(&job, &engine.context(&job).unwrap());
    blocks[0].text.push_str(" Reworded.");
    engine
        .on_change(&render(&blocks, "", "\n"), None, 20)
        .unwrap();
    assert_eq!(settle(&mut engine, &mut models, &mut checker), 2);
    assert_eq!(
        engine.complete(job, outcome),
        Completion::Stale(Stale::InputsChanged)
    );
    assert!(engine.dirty().contains(&id(3)));
}

#[test]
fn abandoned_jobs_can_be_reissued_and_late_outcomes_cannot_release_replacements() {
    let blocks = three("Truth holds.");
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    let abandoned = engine.next_job(0).unwrap();
    assert!(engine.abandon(&abandoned));
    let replacement = engine.next_job(0).unwrap();
    assert_eq!(replacement.block, abandoned.block);
    assert!(!engine.abandon(&abandoned));
    assert_eq!(
        engine.complete(
            abandoned,
            JobOutcome::Failed {
                message: "late cancelled worker".into(),
                usage: UsageSummary::default(),
            }
        ),
        Completion::Stale(Stale::NotInFlight)
    );
    assert_ne!(engine.next_job(0).unwrap().block, replacement.block);
    assert!(
        engine.abandon(&replacement),
        "replacement is still in flight"
    );
    assert_eq!(engine.next_job(0).unwrap().block, replacement.block);
}

#[test]
fn nonformal_label_changes_invalidate_formal_users_of_an_unchanged_alias() {
    let blocks = three("Truth holds.");
    let source = String::from_utf8(render(&blocks, "", "\n"))
        .unwrap()
        .replacen("<\\theorem>", "<\\section>", 1)
        .replacen("</theorem>", "</section>", 1);
    let mut engine = engine_with(0);
    engine.on_change(source.as_bytes(), None, 0).unwrap();
    assert_eq!(engine.document().block(id(2)).unwrap().deps, [id(1)]);
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    settle(&mut engine, &mut models, &mut checker);
    let changed = source.replacen("<label|thm:b1>", "<label|sec:new-primary><label|thm:b1>", 1);
    let change = engine.on_change(changed.as_bytes(), None, 10).unwrap();
    assert!(change.changed.contains(&id(1)));
    assert!(
        !change.changed.contains(&id(2)),
        "reference alias and text are unchanged"
    );
    assert_eq!(engine.dirty(), &BTreeSet::from([id(2)]));
    let job = engine.next_job(10).unwrap();
    let context = engine.context(&job).unwrap();
    assert_eq!(context.expected_uses, ["sec:new-primary"]);
    assert!(
        context.sir_candidate.is_none(),
        "cached SIR has outdated uses"
    );
    models.specs.get_mut(&id(2)).unwrap().1 = vec!["sec:new-primary".into()];
    let outcome = PipelineRunner::new(&mut models, &mut checker, config()).run(&job, &context);
    assert_eq!(engine.complete(job, outcome), Completion::Applied);
    assert_eq!(models.sir_calls, [id(2), id(3), id(2)]);
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.errors.is_empty())
    );
}

#[test]
fn a_formal_block_changed_to_a_section_leaves_no_ghost_work() {
    let blocks = three("Truth holds.");
    let source = String::from_utf8(render(&blocks, "", "\n")).unwrap();
    let mut engine = engine_with(0);
    engine.on_change(source.as_bytes(), None, 0).unwrap();
    let old = engine.next_job(0).unwrap();
    let demoted =
        source
            .replacen("<\\theorem>", "<\\section>", 1)
            .replacen("</theorem>", "</section>", 1);
    engine.on_change(demoted.as_bytes(), None, 10).unwrap();
    assert!(!engine.dirty().contains(&id(1)));
    assert!(!engine.abandon(&old), "demotion releases its old issuance");
    settle_none(&mut engine);
    assert!(engine.dirty().is_empty());
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.block_id != id(1))
    );
}

#[test]
fn a_previous_cursor_exit_does_not_bypass_debounce_on_later_dependency_edits() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(800);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    engine.on_cursor(Some(id(2)));
    engine.on_cursor(None);
    let mut edited = blocks.clone();
    edited[0].text.push_str(" Revised.");
    engine
        .on_change(&render(&edited, "", "\n"), Some(id(1)), 2_000_000)
        .unwrap();
    engine.on_cursor(None);
    assert_eq!(
        engine.run_next(
            &mut PipelineRunner::new(&mut models, &mut checker, config()),
            2_000_000
        ),
        Some(Completion::Applied)
    );
    assert!(
        engine.next_job(2_000_799).is_none(),
        "dependent must debounce its new invalidation"
    );
    assert_eq!(engine.next_job(2_000_800).unwrap().block, id(2));
}

#[test]
fn failed_and_stale_failed_outcomes_count_usage_and_preserve_unknown_values() {
    let blocks = three("Truth holds.");
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    let job = engine.next_job(0).unwrap();
    let unknown_usage = || UsageSummary {
        calls: 1,
        unknown_input_token_calls: 1,
        unknown_output_token_calls: 1,
        unknown_cost_calls: 1,
        ..UsageSummary::default()
    };
    assert_eq!(
        engine.complete(
            job,
            JobOutcome::Failed {
                message: "model call failed".into(),
                usage: unknown_usage(),
            }
        ),
        Completion::Applied
    );
    assert_eq!(engine.usage().calls, 1);
    assert!(
        !engine.dirty().contains(&id(1)),
        "failure is terminal until explicitly retried"
    );
    engine.retry(id(1), 10);
    let job = engine.next_job(10).unwrap();
    let edited = three("Truth holds, edited.");
    engine
        .on_change(&render(&edited, "", "\n"), None, 20)
        .unwrap();
    assert_eq!(
        engine.complete(
            job,
            JobOutcome::Failed {
                message: "model call failed on old source".into(),
                usage: unknown_usage(),
            }
        ),
        Completion::Stale(Stale::TextChanged)
    );
    assert_eq!(engine.usage().calls, 2);
    assert_eq!(engine.usage().unknown_input_token_calls, 2);
    assert_eq!(engine.usage().unknown_output_token_calls, 2);
    assert_eq!(engine.usage().unknown_cost_calls, 2);
    assert!(engine.dirty().contains(&id(1)));
}

#[test]
fn layout_only_resave_dirties_nothing_and_calls_no_model() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    let calls = (models.sir_count(), models.lean_calls);

    let change = engine
        .on_change(&render(&blocks, "        ", "\n\n  \n"), None, 10)
        .unwrap();
    assert!(change.changed.is_empty(), "{change:?}");
    assert!(engine.dirty().is_empty());
    assert!(engine.next_job(1_000_000).is_none());
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!((models.sir_count(), models.lean_calls), calls);
}

#[test]
fn dependency_edit_invalidates_downstream_statuses_and_lean_keys() {
    let blocks = [
        Spec {
            number: 1,
            text: "Truth holds.".into(),
            deps: vec![],
        },
        Spec {
            number: 2,
            text: "Second claim.".into(),
            deps: vec![1],
        },
    ];
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    confirm_all(&mut engine, &[1, 2]);
    let view = engine.view();
    assert_eq!(status(&view, 1), BlockStatus::StatementConfirmed);
    assert_eq!(status(&view, 2), BlockStatus::StatementConfirmed);
    let lean_before = models.lean_calls;

    // The dependency's SIR changes. Its text changed too, so it must be formalized again.
    models.set(1, &[], "a different claim");
    let edited = [
        Spec {
            number: 1,
            text: "Truth holds, restated.".into(),
            deps: vec![],
        },
        blocks[1].clone(),
    ];
    engine
        .on_change(&render(&edited, "  ", "\n"), None, 10)
        .unwrap();
    let view = engine.view();
    assert_eq!(status(&view, 2), BlockStatus::SirDrafted);

    settle(&mut engine, &mut models, &mut checker);
    let view = engine.view();
    assert_ne!(status(&view, 2), BlockStatus::StatementConfirmed);
    assert!(
        models.lean_calls > lean_before,
        "the dependent's Lean statement is regenerated under its new key"
    );
}

#[test]
fn sir_cache_hit_makes_no_model_call_after_an_unrelated_edit() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!(models.sir_calls, [id(1), id(2), id(3)]);
    let lean_calls = models.lean_calls;

    // Block 1 changes its wording but not its meaning. Its dependent is dirty, yet its inputs are
    // unchanged, so its SIR comes from the cache.
    let reworded = three("Truth holds, in other words.");
    engine
        .on_change(&render(&reworded, "  ", "\n"), None, 10)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!(models.sir_calls[3..], [id(1)]);
    assert_eq!(
        models.lean_calls, lean_calls,
        "Lean declarations are re-checked, not regenerated"
    );

    // Block 3 is unrelated to the rest: only its own SIR is generated again.
    let mut unrelated = reworded.clone();
    unrelated[2].text = "Independent claim, reworded.".into();
    engine
        .on_change(&render(&unrelated, "  ", "\n"), None, 20)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!(models.sir_calls[4..], [id(3)]);
}

#[test]
fn review_actions_follow_the_cli_preconditions() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();

    assert!(matches!(
        engine.on_review(ReviewAction::Confirm(id(1)), 0),
        Err(ActionError::NotInRun(_))
    ));
    assert!(matches!(
        engine.on_review(ReviewAction::Confirm(BlockId::new()), 0),
        Err(ActionError::UnknownBlock(_))
    ));

    settle(&mut engine, &mut models, &mut checker);
    let edited = three("Truth holds, typed since the run.");
    engine
        .on_change(&render(&edited, "  ", "\n"), None, 10)
        .unwrap();
    assert!(matches!(
        engine.on_review(ReviewAction::Confirm(id(1)), 20),
        Err(ActionError::Review(_))
    ));
    assert!(
        engine.review().entry(id(1)).is_none(),
        "a refused action changes nothing"
    );
}

#[test]
fn rejecting_a_sir_marks_its_dependents_and_regenerates_the_block() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);

    engine.on_review(ReviewAction::Reject(id(1)), 10).unwrap();
    assert!(engine.dirty().contains(&id(1)));
    assert!(
        engine.dirty().contains(&id(2)),
        "the dependent waits for the new SIR"
    );
    let calls = models.sir_count();
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!(
        models.sir_count(),
        calls + 1,
        "only the rejected block asks the model again"
    );
    assert_eq!(status(&engine.view(), 1), BlockStatus::NotFormalized);
}

#[test]
fn a_failed_job_settles_the_block_and_its_dependents_see_no_statement() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    let job = engine.next_job(10).unwrap();
    assert_eq!(job.block, id(1));
    assert_eq!(
        engine.complete(
            job,
            JobOutcome::Failed {
                message: "worker crashed".into(),
                usage: UsageSummary::default()
            }
        ),
        Completion::Applied
    );
    assert!(!engine.dirty().contains(&id(1)));
    settle(&mut engine, &mut models, &mut checker);
    let view = engine.view();
    let first = view
        .blocks
        .iter()
        .find(|block| block.block_id == id(1))
        .unwrap();
    assert_eq!(first.status, BlockStatus::NotFormalized);
    assert_eq!(first.errors, ["worker crashed"]);
    let second = view
        .blocks
        .iter()
        .find(|block| block.block_id == id(2))
        .unwrap();
    assert_eq!(second.status, BlockStatus::NotFormalized);
    assert!(
        models.sir_calls.iter().all(|block| *block != id(2)),
        "a blocked block never reaches the model"
    );
    assert!(engine.dirty().is_empty());
    assert!(
        engine.next_job(2_000_000).is_none(),
        "a failure is not automatically retried"
    );

    assert!(engine.retry(id(1), 2_000_000));
    assert_eq!(engine.dirty(), &BTreeSet::from([id(1), id(2)]));
    assert_eq!(settle(&mut engine, &mut models, &mut checker), 2);
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.errors.is_empty())
    );
    assert!(!engine.retry(BlockId::new(), 2_000_000));
}

#[test]
fn refused_documents_leave_the_engine_unchanged() {
    let blocks = three("Truth holds.");
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    let revision = engine.document().revision();
    let duplicate = format!(
        "<TeXmacs|2.1.4>\n<\\body>\n<\\with|mls-id|{0}><\\theorem>A.<label|thm:x></theorem></with>\n<\\with|mls-id|{0}><\\theorem>B.<label|thm:y></theorem></with>\n</body>\n",
        id(9)
    );
    assert!(engine.on_change(duplicate.as_bytes(), None, 5).is_err());
    assert_eq!(engine.document().revision(), revision);
    assert!(engine.document().contains(id(1)));
}

#[test]
fn removed_blocks_drop_their_state_and_report_their_decisions_for_forgetting() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    confirm_all(&mut engine, &[3]);

    let shorter = vec![blocks[0].clone(), blocks[1].clone()];
    let change = engine
        .on_change(&render(&shorter, "  ", "\n"), None, 10)
        .unwrap();
    assert_eq!(change.removed, [id(3)]);
    assert!(
        engine.review().entry(id(3)).is_some(),
        "the engine leaves the store alone"
    );
    engine.review_mut().forget(id(3));
    assert!(engine.review().entry(id(3)).is_none());
    assert!(!engine.dirty().contains(&id(3)));
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.block_id != id(3))
    );
}

/// Pure document-level checks against an independent reference graph.
struct Graph {
    /// For each block number, the numbers of the blocks it references.
    deps: BTreeMap<u8, Vec<u8>>,
}

impl Graph {
    fn downstream(&self, seed: u8) -> BTreeSet<u8> {
        let mut reached = BTreeSet::new();
        let mut changed = true;
        while changed {
            changed = false;
            for (number, deps) in &self.deps {
                let touches = deps.iter().any(|dep| *dep == seed || reached.contains(dep));
                if touches && reached.insert(*number) {
                    changed = true;
                }
            }
        }
        reached
    }
}

/// Deterministic pseudo-random numbers (splitmix64), so every run covers the same cases.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A random acyclic document of `count` blocks, where each block references earlier ones.
fn random_document(rng: &mut Rng, count: u8) -> (Vec<Spec>, Graph) {
    let mut specs = Vec::new();
    let mut deps = BTreeMap::new();
    for number in 1..=count {
        let picked: Vec<u8> = (1..number).filter(|_| rng.below(3) == 0).collect();
        deps.insert(number, picked.clone());
        specs.push(Spec {
            number,
            text: format!("Claim number {number}."),
            deps: picked,
        });
    }
    (specs, Graph { deps })
}

#[test]
fn property_invalidation_propagates_exactly_downstream() {
    let mut rng = Rng(7);
    for _ in 0..200 {
        let count = 2 + rng.below(7) as u8;
        let (specs, graph) = random_document(&mut rng, count);
        let mut document = Document::new();
        document.update(&render(&specs, "  ", "\n")).unwrap();

        let target = 1 + rng.below(u64::from(count)) as u8;
        let mut edited = specs.clone();
        edited[usize::from(target) - 1].text.push_str(" Revised.");
        let change = document.update(&render(&edited, "  ", "\n")).unwrap();
        let dirty = document.dirty(&change);

        let mut expected: BTreeSet<BlockId> =
            graph.downstream(target).into_iter().map(id).collect();
        expected.insert(id(target));
        assert_eq!(dirty, expected, "changed block {target} in {count} blocks");
    }
}

#[test]
fn property_removal_invalidates_the_previous_dependents() {
    let mut rng = Rng(11);
    for _ in 0..100 {
        let count = 3 + rng.below(6) as u8;
        let (specs, graph) = random_document(&mut rng, count);
        let mut document = Document::new();
        document.update(&render(&specs, "  ", "\n")).unwrap();

        // Remove the last block: no later block can reference it, so only the earlier blocks that
        // referenced it would be affected. A removed block's previous dependents must be dirty.
        let removed = count;
        let kept: Vec<Spec> = specs
            .iter()
            .filter(|spec| spec.number != removed)
            .cloned()
            .collect();
        let change = document.update(&render(&kept, "  ", "\n")).unwrap();
        let dirty = document.dirty(&change);
        for number in graph.downstream(removed) {
            if number != removed {
                assert!(
                    dirty.contains(&id(number)) || kept.iter().all(|spec| spec.number != number),
                    "dependent {number} of removed {removed} stayed clean"
                );
            }
        }
        assert!(change.removed.contains(&id(removed)));
    }
}

#[test]
fn property_layout_edits_never_dirty_anything() {
    let mut rng = Rng(23);
    for _ in 0..150 {
        let count = 1 + rng.below(6) as u8;
        let (specs, _) = random_document(&mut rng, count);
        let mut document = Document::new();
        document.update(&render(&specs, "  ", "\n")).unwrap();
        let before: HashSet<String> = document
            .analysis()
            .blocks
            .iter()
            .map(|block| block.text_hash.clone())
            .collect();

        let indent = " ".repeat(rng.below(9) as usize);
        let gap = "\n".repeat(1 + rng.below(3) as usize);
        let change = document.update(&render(&specs, &indent, &gap)).unwrap();
        assert!(change.changed.is_empty(), "layout edit changed {change:?}");
        assert!(document.dirty(&change).is_empty());
        let after: HashSet<String> = document
            .analysis()
            .blocks
            .iter()
            .map(|block| block.text_hash.clone())
            .collect();
        assert_eq!(before, after, "layout must not change canonical text");
    }
}

#[test]
fn property_moves_dirty_nothing_and_keep_every_key() {
    let mut rng = Rng(31);
    for _ in 0..100 {
        let count = 2 + rng.below(6) as u8;
        let (specs, _) = random_document(&mut rng, count);
        let mut document = Document::new();
        document.update(&render(&specs, "  ", "\n")).unwrap();

        // Rotate the order. Every block keeps its text, label and references, only its position moves.
        let mut moved = specs.clone();
        moved.rotate_left(1);
        let change: ChangeSet = document.update(&render(&moved, "  ", "\n")).unwrap();
        assert!(change.changed.is_empty());
        assert!(change.added.is_empty() && change.removed.is_empty());
        assert!(document.dirty(&change).is_empty(), "a move dirtied a block");
        assert!(!change.moved.is_empty() || count == 1);
    }

    // Through the engine: a move after settling calls no model and keeps every cache key.
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    let sir_cache = engine.sir_cache().clone();
    let lean_cache = engine.lean_cache().clone();
    let mut reversed = blocks.clone();
    reversed.reverse();
    engine
        .on_change(&render(&reversed, "  ", "\n"), None, 10)
        .unwrap();
    assert!(engine.dirty().is_empty());
    settle(&mut engine, &mut models, &mut checker);
    assert_eq!(models.sir_count(), 3);
    assert_eq!(
        serde_json::to_string(engine.sir_cache()).unwrap(),
        serde_json::to_string(&sir_cache).unwrap()
    );
    assert_eq!(
        serde_json::to_string(engine.lean_cache()).unwrap(),
        serde_json::to_string(&lean_cache).unwrap()
    );
}

#[test]
fn oracle_holds_a_block_only_while_confident_and_within_the_defer_limit() {
    struct Fixed(OracleAnswer);
    impl FinishedOracle for Fixed {
        fn is_finished(&mut self, _: &FinishedQuery<'_>) -> OracleAnswer {
            self.0
        }
    }
    let source = render(&three("Truth holds."), "  ", "\n");
    let mut reworded = three("Truth holds.");
    reworded[2].text = "Independent claim, reworded.".into();
    let edited = render(&reworded, "  ", "\n");

    let mut engine = engine_with(0);
    engine.set_oracle(Box::new(Fixed(OracleAnswer {
        finished: Finished::No,
        confidence: 0.95,
    })));
    engine.on_change(&source, None, 0).unwrap();
    settle_none(&mut engine);
    engine.on_change(&edited, None, 100).unwrap();
    assert!(
        engine.next_job(100).is_none(),
        "held while confidently unfinished"
    );
    assert!(engine.next_job(29_999).is_none());
    assert_eq!(engine.next_job(30_100).unwrap().block, id(3));

    let mut engine = engine_with(0);
    engine.set_oracle(Box::new(Fixed(OracleAnswer {
        finished: Finished::No,
        confidence: 0.5,
    })));
    engine.on_change(&source, None, 0).unwrap();
    settle_none(&mut engine);
    engine.on_change(&edited, None, 100).unwrap();
    assert!(
        engine.next_job(100).is_some(),
        "low confidence falls back to the debounce"
    );
}

/// Only for oracle tests: drain jobs without a runner by completing them as failures.
fn settle_none(engine: &mut Engine) {
    while let Some(job) = engine.next_job(1_000_000) {
        engine.complete(
            job,
            JobOutcome::Failed {
                message: "skipped".into(),
                usage: UsageSummary::default(),
            },
        );
    }
}

#[test]
fn property_lru_eviction_is_bounded_and_keeps_the_newest() {
    let mut rng = Rng(41);
    for _ in 0..50 {
        let capacity = 1 + rng.below(5) as usize;
        let mut cache = SirCache::new(capacity);
        let mut lean = LeanCache::new(capacity);
        let total = 1 + rng.below(12);
        for revision in 1..=total {
            let key = format!("key-{revision}");
            cache.record(key.clone(), sir_for(revision), provenance(revision));
            lean.record(id(1), key, format!("decl {revision}"), revision);
            assert!(cache.len() <= capacity);
            assert!(lean.len() <= capacity);
        }
        let kept = total.min(capacity as u64);
        for revision in 1..=total {
            let present = revision + kept > total;
            assert_eq!(
                cache.lookup(&format!("key-{revision}")).is_some(),
                present,
                "revision {revision} of {total} with capacity {capacity}"
            );
        }
    }
}

#[test]
fn caches_serialize_deterministically_and_round_trip() {
    let mut first = SirCache::new(2);
    let mut second = SirCache::new(2);
    for revision in [3, 1, 2] {
        first.record(
            format!("k{revision}"),
            sir_for(revision),
            provenance(revision),
        );
        second.record(
            format!("k{revision}"),
            sir_for(revision),
            provenance(revision),
        );
    }
    let json = serde_json::to_string(&first).unwrap();
    assert_eq!(json, serde_json::to_string(&second).unwrap());
    let restored: SirCache = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), json);
    assert_eq!(restored.len(), 2);
    assert_eq!(restored.capacity(), 2);
}

#[test]
fn sir_cache_keys_depend_on_model_text_and_dependency_hashes_only() {
    let deps = vec![(id(1), "a".repeat(64))];
    let key = sir_cache_key("model-a", "t", &deps);
    assert_eq!(key, sir_cache_key("model-a", "t", &deps));
    assert_ne!(key, sir_cache_key("model-b", "t", &deps));
    assert_ne!(key, sir_cache_key("model-a", "u", &deps));
    assert_ne!(key, sir_cache_key("model-a", "t", &[]));
}

fn sir_for(revision: u64) -> Sir {
    Sir::from_json(
        &json!({
            "kind": "theorem",
            "label": format!("thm:n{revision}"),
            "variables": [],
            "hypotheses": [],
            "conclusion": format!("claim {revision}"),
            "uses": [],
            "assumptions_made": []
        })
        .to_string(),
    )
    .unwrap()
}

fn provenance(revision: u64) -> SirProvenance {
    SirProvenance {
        model: GENERATION_MODEL.into(),
        schema_version: "mls-sir-v1".into(),
        text_hash: format!("text-{revision}"),
        dependencies: Vec::new(),
        revision,
    }
}

#[test]
fn a_document_with_a_dependency_cycle_is_still_scheduled_without_hanging() {
    let blocks = [
        Spec {
            number: 1,
            text: "First.".into(),
            deps: vec![2],
        },
        Spec {
            number: 2,
            text: "Second.".into(),
            deps: vec![1],
        },
    ];
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    assert_eq!(settle(&mut engine, &mut models, &mut checker), 0);
    assert!(
        engine.dirty().is_empty(),
        "cycles are explicitly settled as blocked"
    );
    assert_eq!(models.sir_count(), 0);
    let view = engine.view();
    assert_eq!(view.blocks.len(), 2);
    assert!(view.blocks.iter().all(|block| {
        block
            .errors
            .iter()
            .any(|error| error.contains("dependency cycle"))
    }));
    assert!(
        view.blocks
            .iter()
            .all(|block| block.status != BlockStatus::StatementConfirmed)
    );
}

#[test]
fn cycle_errors_cover_downstream_blocks_and_clear_when_the_graph_is_repaired() {
    let mut blocks = three("First.");
    blocks[0].deps = vec![2];
    blocks[2].deps = vec![2];
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    assert!(engine.dirty().is_empty());
    assert!(engine.next_job(0).is_none());
    assert_eq!(models.sir_count(), 0);
    assert!(engine.view().blocks.iter().all(|block| {
        block.status == BlockStatus::NotFormalized
            && block.errors == ["dependency cycle prevents formalization"]
    }));

    blocks[0].deps.clear();
    models.set(1, &[], "claim 1");
    engine
        .on_change(&render(&blocks, "", "\n"), None, 10)
        .unwrap();
    assert_eq!(settle(&mut engine, &mut models, &mut checker), 3);
    assert!(engine.dirty().is_empty());
    assert!(
        engine
            .view()
            .blocks
            .iter()
            .all(|block| block.errors.is_empty())
    );
}

#[test]
fn a_self_dependency_is_an_explicit_cycle_error() {
    let blocks = [Spec {
        number: 1,
        text: "Self-referential.".into(),
        deps: vec![1],
    }];
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "", "\n"), None, 0)
        .unwrap();
    assert!(engine.dirty().is_empty());
    assert_eq!(
        engine.view().blocks[0].errors,
        ["dependency cycle prevents formalization"]
    );
}

#[test]
fn the_view_names_the_document_and_counts_every_formal_block() {
    let blocks = three("Truth holds.");
    let mut models = Model::for_blocks(&blocks);
    let mut checker = EchoChecker;
    let mut engine = engine_with(0);
    engine
        .on_change(&render(&blocks, "  ", "\n"), None, 0)
        .unwrap();
    settle(&mut engine, &mut models, &mut checker);
    let view = engine.view();
    assert_eq!(view.document, "doc.tm");
    assert_eq!(view.blocks.len(), 3);
    assert_eq!(view.counts.sir_drafted, 3);
    assert!(!view.needs_rerun);
}
