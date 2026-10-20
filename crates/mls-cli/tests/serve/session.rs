//! Session acceptance tests use the real pipeline, scripted model JSON and an in-memory checker.
use super::*;
use mls_core::BlockKind;
use mls_engine::decisions::{DecisionConfig, DecisionLayer, ProseKind};
use mls_engine::{BlockStatus, StatementChecker};
use mls_lean::ExtractedDeclaration;
use mls_models::{Answer, CallMetadata, ChoiceAnswer, DecisionReply, DecisionRequest, Usage};
use std::collections::VecDeque;

fn id(number: u8) -> BlockId {
    format!("00000000-0000-4000-8000-{number:012}")
        .parse()
        .unwrap()
}
fn source(blocks: &[(u8, &str)], gap: &str) -> String {
    let mut source = "<TeXmacs|2.1.4>\n<\\body>\n".to_owned();
    for (number, text) in blocks {
        source.push_str(&format!(
            "{gap}<\\with|mls-id|{}><\\theorem>{text}<label|b{number}></theorem></with>\n",
            id(*number)
        ));
    }
    source.push_str("</body>\n");
    source
}
fn prose_source(number: u8, text: &str) -> String {
    format!(
        "<TeXmacs|2.1.4>\n<\\body>\n<\\with|mls-id|{}>{text}</with>\n</body>\n",
        id(number)
    )
}
fn request(method: &str, params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":4,"method":method,"params":params})).unwrap()
}
fn call(session: &mut Session<Fake>, method: &str, params: Value, now: u64) -> Vec<Value> {
    session.handle(&request(method, params), now)
}
fn open(session: &mut Session<Fake>, blocks: &[(u8, &str)], active: Option<BlockId>) {
    open_with_source(session, source(blocks, ""), active);
}
fn open_with_source(session: &mut Session<Fake>, source: String, active: Option<BlockId>) {
    assert!(
        call(session, "initialize", json!({}), 0)[0]
            .get("result")
            .is_some()
    );
    assert!(
        call(
            session,
            "didOpen",
            json!({"document":"/tmp/serve-notes.tm","revision":7,
        "source":source,"active":active}),
            0
        )[0]
        .get("result")
        .is_some()
    );
}
fn change(
    session: &mut Session<Fake>,
    blocks: &[(u8, &str)],
    gap: &str,
    revision: u64,
) -> Vec<Value> {
    call(
        session,
        "didChange",
        json!({"document":"/tmp/serve-notes.tm","revision":revision,
        "source":source(blocks, gap)}),
        2000,
    )
}
fn action(
    session: &mut Session<Fake>,
    number: u8,
    action: &str,
    sir: Option<Value>,
    revision: u64,
) -> Vec<Value> {
    let mut params = json!({"document":"/tmp/serve-notes.tm","revision":revision,
        "block":id(number),"action":action});
    if let Some(sir) = sir {
        params["sir"] = sir;
    }
    call(session, "review", params, 2000)
}

#[derive(Default)]
struct Environment {
    declarations: Vec<BlockId>,
    events: Vec<String>,
    fail_replay: bool,
}
impl DeclarationEnvironment for Environment {
    type Snapshot = usize;
    fn snapshot(&self) -> usize {
        self.declarations.len()
    }
    fn restore(&mut self, snapshot: usize) -> Result<(), String> {
        self.events.push(format!("restore:{snapshot}"));
        self.declarations.truncate(snapshot);
        Ok(())
    }
    fn replay(&mut self, report: &BlockReport) -> Result<(), String> {
        if self.fail_replay {
            return Err("scripted replay failed".into());
        }
        assert!(!self.declarations.contains(&report.block_id));
        self.events.push(format!("replay:{}", report.block_id));
        self.declarations.push(report.block_id);
        Ok(())
    }
}
struct Fake {
    environment: Environment,
    journal: Journal<usize>,
    runs: Vec<BlockId>,
    stores: Vec<String>,
    store: ReviewStore,
    fail_persist: bool,
    fail_job: bool,
    unhealthy: bool,
    opens: usize,
    prose_decisions: Option<DecisionLayer<FixedProseDecider>>,
    prose_classifications: BTreeMap<BlockId, ProseClassification>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            environment: Environment::default(),
            journal: Journal::new(),
            runs: Vec::new(),
            stores: Vec::new(),
            store: ReviewStore::new(),
            fail_persist: false,
            fail_job: false,
            unhealthy: false,
            opens: 0,
            prose_decisions: None,
            prose_classifications: BTreeMap::new(),
        }
    }
}
impl Backend for Fake {
    fn model(&self) -> &str {
        "scripted"
    }
    fn open(&mut self, _: &Path) -> Result<ReviewStore, String> {
        self.opens += 1;
        ReviewStore::from_json(&self.store.to_json()).map_err(|error| error.to_string())
    }
    fn synchronize(&mut self, engine: &Engine) -> Result<(), String> {
        self.journal.synchronize(&mut self.environment, engine)
    }
    fn update_hints(&mut self, _engine: &Engine, source: &[u8]) -> UpdateHints {
        let Some(decisions) = &mut self.prose_decisions else {
            return UpdateHints::default();
        };
        let mut next = Document::new();
        if next.update(source).is_err() {
            return UpdateHints::default();
        }
        UpdateHints {
            prose_claims: classify_untagged_prose(
                &next,
                decisions,
                &mut self.prose_classifications,
            ),
            ..UpdateHints::default()
        }
    }
    fn run(&mut self, job: &Job, context: &JobContext<'_>) -> JobOutcome {
        self.runs.push(job.block);
        let before = self.environment.snapshot();
        assert!(
            !self.environment.declarations.contains(&job.block),
            "ghost declaration survived"
        );
        if self.fail_job {
            return JobOutcome::Failed {
                message: "scripted runner failed".into(),
                usage: UsageSummary::default(),
            };
        }
        let mut models = Scripted {
            label: context.block.label.clone(),
            uses: context.expected_uses.clone(),
            name: declaration_name(job.block),
            reviewed_sir: match context.review.entry(job.block) {
                Some(mls_engine::ReviewEntry::Confirmed { sir, .. }) => {
                    Some(serde_json::to_value(sir).unwrap())
                }
                _ => None,
            },
        };
        let mut checker = Checker;
        let outcome = PipelineRunner::new(&mut models, &mut checker, PipelineConfig::default())
            .run(job, context);
        if let JobOutcome::Completed(result) = &outcome {
            if !job.proof_only && result.report.accepted_speculatively {
                self.environment.declarations.push(job.block);
            }
            self.journal
                .record_completed(job.proof_only, before, &result.report);
        }
        outcome
    }
    fn persist(&mut self, store: &ReviewStore) -> Result<(), String> {
        if self.fail_persist {
            return Err("scripted disk full".into());
        }
        self.stores.push(store.to_json());
        Ok(())
    }
    fn health(&mut self) -> Result<(), String> {
        if self.unhealthy {
            Err("scripted recording failure".into())
        } else {
            Ok(())
        }
    }
    fn annotations(&self) -> Value {
        json!({
            "prose_classifications": self.prose_classifications.iter()
                .map(|(id, classification)| (id.to_string(), classification.annotation.clone()))
                .collect::<BTreeMap<_, _>>(),
            "usage": self.prose_decisions.as_ref().map(|decisions| decisions.usage().clone())
        })
    }
}

struct ProseChoice {
    choice: String,
    probability: f64,
    confidence: f64,
}

struct FixedProseDecider {
    replies: VecDeque<ProseChoice>,
}

impl mls_engine::decisions::Decider for FixedProseDecider {
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        let reply = self.replies.pop_front().expect("scripted prose decision");
        let mut probabilities: BTreeMap<String, f64> = [
            ("claim".to_owned(), 0.0),
            ("proof_step".to_owned(), 0.0),
            ("commentary".to_owned(), 0.0),
        ]
        .into();
        probabilities.insert(reply.choice.clone(), reply.probability);
        let others: Vec<_> = probabilities
            .keys()
            .filter(|key| **key != reply.choice)
            .cloned()
            .collect();
        for key in others {
            probabilities.insert(key, (1.0 - reply.probability) / 2.0);
        }
        Ok(DecisionReply {
            answers: BTreeMap::from([(
                "answer".into(),
                Answer::Choice(ChoiceAnswer {
                    choice: reply.choice,
                    probabilities: Some(probabilities),
                    confidence: Some(reply.confidence),
                }),
            )]),
            metadata: CallMetadata {
                http_status: 200,
                configured_model: "scripted-jev".into(),
                resolved_model: "scripted-jev".into(),
                request_id: None,
                provider: Some("mock".into()),
                latency_ms: 0,
                usage: Usage {
                    input_tokens: Some(3),
                    output_tokens: Some(2),
                    total_tokens: Some(5),
                    cost: Some(0.01),
                },
                provider_response: json!({"mock":true}),
            },
        })
    }
}

fn prose_session(answers: impl IntoIterator<Item = ProseChoice>) -> Session<Fake> {
    let decisions = DecisionLayer::new(
        FixedProseDecider {
            replies: answers.into_iter().collect(),
        },
        DecisionConfig::default(),
    )
    .unwrap();
    Session::new(Fake {
        prose_decisions: Some(decisions),
        ..Fake::default()
    })
}
struct Scripted {
    label: Option<String>,
    uses: Vec<String>,
    name: String,
    reviewed_sir: Option<Value>,
}
impl ModelClient for Scripted {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        let _ = request;
        let sir = self.reviewed_sir.clone().unwrap_or_else(|| {
            json!({
                "kind":"theorem","label":self.label,"variables":[],
                "hypotheses":[],"conclusion":"True","uses":self.uses,"assumptions_made":[]
            })
        });
        let conclusion = sir["conclusion"].as_str().unwrap_or("True");
        let output = json!({"sir":sir,
            "lean_statement":format!("theorem {} : {conclusion} := by sorry",self.name),
            "lean_proof":null,"notes":[]});
        Ok(GenerationReply {
            output,
            finish_reason: "stop".into(),
            metadata: CallMetadata {
                http_status: 200,
                configured_model: "scripted".into(),
                resolved_model: "scripted".into(),
                request_id: None,
                provider: None,
                latency_ms: 0,
                usage: Usage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    total_tokens: Some(2),
                    cost: None,
                },
                provider_response: json!({}),
            },
        })
    }
    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        panic!("no decision calls in skeleton session")
    }
}
struct Checker;
impl StatementChecker for Checker {
    fn check(
        &mut self,
        _: BlockId,
        name: &str,
        source: &str,
    ) -> Result<ExtractedDeclaration, String> {
        let goal = source
            .split_once(':')
            .map(|(_, remainder)| remainder.split(":=").next().unwrap_or("True").trim())
            .unwrap_or("True");
        let goal_outer_head = if goal.contains('∧') {
            "and"
        } else if goal.contains('∨') {
            "or"
        } else if goal.contains('→') {
            "implication"
        } else if goal.contains('=') {
            "equality"
        } else {
            "atom"
        };
        Ok(ExtractedDeclaration {
            declaration: name.into(),
            kind: "theorem".into(),
            binders: Vec::new(),
            goal: goal.into(),
            goal_outer_head: goal_outer_head.into(),
            axioms: vec!["sorryAx".into()],
            has_sorry: true,
            body: None,
            body_outer_head: None,
        })
    }
    fn finish(&mut self, _: bool) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn ids_initialization_and_strict_requests_have_structured_errors() {
    let mut session = Session::new(Fake::default());
    assert_eq!(session.handle(b"{", 0)[0]["error"]["code"], -32700);
    for value in [
        json!([]),
        json!({"jsonrpc":"2.0","method":"initialize"}),
        json!({"jsonrpc":"2.0","id":null,"method":"initialize"}),
        json!({"jsonrpc":"1.0","id":1,"method":"initialize"}),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","extra":true}),
    ] {
        assert_eq!(
            session.handle(&serde_json::to_vec(&value).unwrap(), 0)[0]["error"]["code"],
            -32600
        );
    }
    assert_eq!(
        call(&mut session, "cursor", json!({}), 0)[0]["error"]["code"],
        -32000
    );
    let response = session.handle(br#"{"jsonrpc":"2.0","id":"init","method":"initialize"}"#, 0);
    assert_eq!(response[0]["id"], "init");
    assert_eq!(
        response[0]["result"]["capabilities"]["decisionOracle"],
        true
    );
    assert!(
        call(&mut session, "initialize", json!({}), 0)[0]
            .get("error")
            .is_some()
    );
    assert_eq!(
        call(&mut session, "unknown", json!({}), 0)[0]["error"]["code"],
        -32601
    );
    assert_eq!(
        call(&mut session, "didOpen", json!({"extra":true}), 0)[0]["error"]["code"],
        -32602
    );
    assert_eq!(session.backend.opens, 0);
}

#[test]
fn confident_prose_claim_runs_as_a_theorem_and_stays_a_human_review_draft() {
    let mut session = prose_session([ProseChoice {
        choice: "claim".into(),
        probability: 0.97,
        confidence: 0.98,
    }]);
    open_with_source(
        &mut session,
        prose_source(9, "Every object equals itself."),
        None,
    );
    assert_eq!(
        session
            .engine
            .as_ref()
            .unwrap()
            .document()
            .block(id(9))
            .unwrap()
            .kind,
        BlockKind::ProseClaim
    );
    let repeat = call(
        &mut session,
        "didChange",
        json!({"document":"/tmp/serve-notes.tm","revision":8,
            "source":prose_source(9,"Every object equals itself."),"active":null}),
        1,
    );
    assert!(repeat[0].get("result").is_some());
    assert_eq!(
        session
            .backend
            .prose_decisions
            .as_ref()
            .unwrap()
            .usage()
            .calls,
        1
    );

    session.tick(10_000);
    assert_eq!(session.backend.runs, [id(9)]);
    let block = &session.engine.as_ref().unwrap().view().blocks[0];
    assert_eq!(block.status, BlockStatus::SirDrafted);
    assert_eq!(
        serde_json::to_value(block.sir.as_ref().unwrap()).unwrap()["kind"],
        "theorem"
    );
    assert!(
        session
            .engine
            .as_ref()
            .unwrap()
            .review()
            .entry(id(9))
            .is_none()
    );
    assert_eq!(
        session.backend.prose_classifications[&id(9)].kind,
        ProseKind::Claim
    );
    assert_eq!(
        session
            .backend
            .prose_decisions
            .as_ref()
            .unwrap()
            .usage()
            .calls,
        1
    );
    let annotations = session.backend.annotations();
    assert_eq!(annotations["usage"]["calls"], 1);
    assert_eq!(
        annotations["prose_classifications"][id(9).to_string()]["value"],
        "claim"
    );
}

#[test]
fn low_confidence_classification_falls_back_to_commentary_without_scheduling() {
    let mut session = prose_session([ProseChoice {
        choice: "claim".into(),
        probability: 0.6,
        confidence: 0.6,
    }]);
    open_with_source(
        &mut session,
        prose_source(9, "Hence the claim follows."),
        None,
    );
    assert_eq!(
        session
            .engine
            .as_ref()
            .unwrap()
            .document()
            .block(id(9))
            .unwrap()
            .kind,
        BlockKind::Commentary
    );
    assert!(session.engine.as_ref().unwrap().dirty().is_empty());
    assert!(session.tick(10_000).is_empty());
    assert!(session.backend.runs.is_empty());
    assert_eq!(
        session.backend.prose_classifications[&id(9)].kind,
        ProseKind::Commentary
    );
    assert_eq!(
        session
            .backend
            .prose_decisions
            .as_ref()
            .unwrap()
            .usage()
            .calls,
        1
    );
    let annotations = session.backend.annotations();
    assert_eq!(annotations["usage"]["calls"], 1);
    assert_eq!(
        annotations["prose_classifications"][id(9).to_string()]["value"],
        "commentary"
    );
    assert_eq!(
        annotations["prose_classifications"][id(9).to_string()]["basis"]["fallback"],
        "low_confidence"
    );
}

#[test]
fn prose_without_a_persisted_id_is_not_sent_for_classification() {
    let mut session = prose_session([]);
    open_with_source(
        &mut session,
        "<TeXmacs|2.1.4><\\body>Some untagged prose.</body>".into(),
        None,
    );
    assert!(session.backend.prose_classifications.is_empty());
    assert_eq!(
        session
            .backend
            .prose_decisions
            .as_ref()
            .unwrap()
            .usage()
            .calls,
        0
    );
    assert!(session.engine.as_ref().unwrap().dirty().is_empty());
    assert!(session.tick(10_000).is_empty());
    assert!(session.backend.runs.is_empty());
}

#[test]
fn proof_step_classification_never_becomes_a_claim_and_text_edits_drop_old_classifications() {
    let mut session = prose_session([
        ProseChoice {
            choice: "proof_step".into(),
            probability: 0.97,
            confidence: 0.98,
        },
        ProseChoice {
            choice: "commentary".into(),
            probability: 0.97,
            confidence: 0.98,
        },
    ]);
    open_with_source(
        &mut session,
        prose_source(9, "By reflexivity, the result follows."),
        None,
    );
    assert_eq!(
        session.backend.prose_classifications[&id(9)].kind,
        ProseKind::ProofStep
    );
    assert_eq!(
        session
            .engine
            .as_ref()
            .unwrap()
            .document()
            .block(id(9))
            .unwrap()
            .kind,
        BlockKind::Commentary
    );
    assert!(session.tick(10_000).is_empty());
    assert!(session.backend.runs.is_empty());

    let changed = call(
        &mut session,
        "didChange",
        json!({"document":"/tmp/serve-notes.tm","revision":8,
            "source":prose_source(9,"A different paragraph."),"active":null}),
        20_000,
    );
    assert!(changed[0].get("result").is_some());
    assert_eq!(
        session.backend.prose_classifications[&id(9)].kind,
        ProseKind::Commentary
    );
    assert_eq!(
        session
            .backend
            .prose_decisions
            .as_ref()
            .unwrap()
            .usage()
            .calls,
        2
    );

    let removed = call(
        &mut session,
        "didChange",
        json!({"document":"/tmp/serve-notes.tm","revision":9,
            "source":"<TeXmacs|2.1.4><\\body></body>","active":null}),
        30_000,
    );
    assert!(removed[0].get("result").is_some());
    assert!(session.backend.prose_classifications.is_empty());
}

#[test]
fn invalid_source_or_active_block_opens_no_resources_and_change_is_transactional() {
    let mut session = Session::new(Fake::default());
    call(&mut session, "initialize", json!({}), 0);
    for source in [
        "<\\body>".to_owned(),
        "<TeXmacs|2.1.4><\\body><theorem|True></body>".to_owned(),
    ] {
        let result = call(
            &mut session,
            "didOpen",
            json!({"document":"/tmp/x.tm","revision":1,"source":source}),
            0,
        );
        assert!(result[0].get("error").is_some());
    }
    assert_eq!(session.backend.opens, 0);
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True")], Some(id(1)));
    let bad = call(
        &mut session,
        "didChange",
        json!({"document":"/tmp/serve-notes.tm","revision":8,
        "source":source(&[(1,"False")],""),"active":id(2)}),
        1,
    );
    assert!(bad[0].get("error").is_some());
    assert_eq!(session.revision, 7);
    assert_eq!(
        session.engine.as_ref().unwrap().document().source(),
        source(&[(1, "True")], "").as_bytes()
    );
    assert!(
        call(
            &mut session,
            "didOpen",
            json!({"document":"/tmp/other.tm","revision":8,"source":source(&[],"")}),
            1
        )[0]
        .get("error")
        .is_some()
    );
}

#[test]
fn cursor_admission_drafts_confirmation_rejection_and_edit_use_engine_rules() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True")], Some(id(1)));
    assert!(session.tick(10000).is_empty());
    assert!(session.backend.runs.is_empty());
    call(
        &mut session,
        "cursor",
        json!({"document":"/tmp/serve-notes.tm","revision":7,"active":null}),
        10000,
    );
    let pushes = session.tick(10000);
    assert_eq!(
        pushes
            .iter()
            .map(|push| push["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["blockStatus", "reviewRequest", "diagnostics"]
    );
    assert_eq!(session.backend.runs, [id(1)]);
    let block = &session.engine.as_ref().unwrap().view().blocks[0];
    assert_eq!(block.status, BlockStatus::SirDrafted);
    assert!(block.lean_statement.is_some());
    assert_eq!(
        pushes[1]["params"]["data"]["blocks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        action(&mut session, 1, "confirm", None, 7)[0]
            .get("result")
            .is_some()
    );
    assert_eq!(
        session.engine.as_ref().unwrap().view().blocks[0].status,
        BlockStatus::StatementConfirmed
    );
    let store = ReviewStore::from_json(session.backend.stores.last().unwrap()).unwrap();
    assert!(store.entry(id(1)).is_some());
    let mut edited = serde_json::to_value(
        session.engine.as_ref().unwrap().view().blocks[0]
            .sir
            .clone()
            .unwrap(),
    )
    .unwrap();
    assert!(
        action(&mut session, 1, "reject", None, 7)[0]
            .get("result")
            .is_some()
    );
    assert!(
        session
            .environment_events()
            .contains(&"restore:0".to_owned())
    );
    session.tick(20000);
    edited["conclusion"] = json!("True ∧ True");
    assert!(
        action(&mut session, 1, "edit", Some(edited), 7)[0]
            .get("result")
            .is_some()
    );
    session.tick(30000);
    // An explicit human edit is reviewed; semantic text comparison remains unchecked by Engine.
    assert_eq!(
        session.engine.as_ref().unwrap().view().blocks[0].status,
        BlockStatus::StatementConfirmed
    );
    assert_eq!(
        session.engine.as_ref().unwrap().view().blocks[0]
            .sir
            .as_ref()
            .unwrap()
            .conclusion(),
        "True ∧ True"
    );
}

impl Session<Fake> {
    fn environment_events(&self) -> &[String] {
        &self.backend.environment.events
    }
}

#[test]
fn stale_revisions_and_wrong_documents_do_not_mutate_or_run() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True")], Some(id(1)));
    assert_eq!(
        change(&mut session, &[(1, "False")], "", 7)[0]["error"]["code"],
        -32001
    );
    for method in ["cursor", "retry", "review"] {
        let params = match method {
            "cursor" => json!({"document":"/tmp/serve-notes.tm","revision":6,"active":null}),
            "retry" => json!({"document":"/tmp/serve-notes.tm","revision":6,"block":id(1)}),
            _ => {
                json!({"document":"/tmp/serve-notes.tm","revision":6,"block":id(1),"action":"confirm"})
            }
        };
        assert_eq!(
            call(&mut session, method, params, 1)[0]["error"]["code"],
            -32001
        );
    }
    assert!(
        call(
            &mut session,
            "cursor",
            json!({"document":"other","revision":7,"active":null}),
            1
        )[0]
        .get("error")
        .is_some()
    );
    assert!(session.tick(10000).is_empty());
    assert!(session.backend.runs.is_empty());
}

#[test]
fn earliest_dirty_snapshot_restores_and_all_retained_later_declarations_replay() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True"), (2, "True"), (3, "True")], None);
    for _ in 0..3 {
        session.tick(10000);
    }
    assert_eq!(
        session.backend.environment.declarations,
        [id(1), id(2), id(3)]
    );
    assert!(
        change(
            &mut session,
            &[(1, "False"), (2, "True"), (3, "True")],
            "",
            8
        )[0]
        .get("result")
        .is_some()
    );
    assert_eq!(
        session.environment_events(),
        [
            "restore:0",
            &format!("replay:{}", id(2)),
            &format!("replay:{}", id(3))
        ]
    );
    assert_eq!(session.backend.environment.declarations, [id(2), id(3)]);
    session.tick(10000);
    assert_eq!(
        session.backend.environment.declarations,
        [id(2), id(3), id(1)]
    );
    assert_eq!(session.backend.runs, [id(1), id(2), id(3), id(1)]);
    // Newly rebuilt snapshots exclude the obsolete declaration: changing the middle of the
    // original source now restores the prefix of the actual accepted execution order.
    change(
        &mut session,
        &[(1, "False"), (2, "False"), (3, "True")],
        "",
        9,
    );
    assert_eq!(session.backend.environment.declarations, [id(3), id(1)]);
    session.tick(10000);
    assert_eq!(session.backend.runs.last(), Some(&id(2)));
}

#[test]
fn earlier_invalidation_never_replays_a_proof_only_duplicate_declaration() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True"), (2, "True"), (3, "True")], None);
    for _ in 0..3 {
        session.tick(10000);
    }
    assert_eq!(session.backend.journal.accepted.len(), 3);

    // A proof-only job reports the same accepted statement, but it never installs a declaration.
    let duplicate = session.backend.journal.accepted[1].report.clone();
    let proof_snapshot = session.backend.environment.snapshot();
    session
        .backend
        .journal
        .record_completed(true, proof_snapshot, &duplicate);
    assert_eq!(session.backend.journal.accepted.len(), 3);

    change(
        &mut session,
        &[(1, "False"), (2, "True"), (3, "True")],
        "",
        8,
    );
    assert_eq!(
        session.environment_events(),
        [
            "restore:0",
            &format!("replay:{}", id(2)),
            &format!("replay:{}", id(3))
        ]
    );
    assert_eq!(session.backend.environment.declarations, [id(2), id(3)]);
}

#[test]
fn moves_and_layout_do_no_model_or_environment_work_and_removal_drops_ghosts() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True"), (2, "True")], None);
    session.tick(10000);
    session.tick(10000);
    change(&mut session, &[(2, "True"), (1, "True")], "   ", 8);
    assert!(session.tick(20000).is_empty());
    assert_eq!(session.backend.runs, [id(1), id(2)]);
    assert!(session.environment_events().is_empty());
    action(&mut session, 1, "confirm", None, 8);
    change(&mut session, &[(2, "True")], "", 9);
    assert_eq!(session.backend.environment.declarations, [id(2)]);
    assert!(
        session
            .engine
            .as_ref()
            .unwrap()
            .review()
            .entry(id(1))
            .is_none()
    );
    assert_eq!(
        session.environment_events(),
        ["restore:0", &format!("replay:{}", id(2))]
    );
}

#[test]
fn replay_and_persistence_failures_stop_all_further_work_but_allow_shutdown() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True"), (2, "True")], None);
    session.tick(10000);
    session.tick(10000);
    session.backend.environment.fail_replay = true;
    assert!(
        change(&mut session, &[(1, "False"), (2, "True")], "", 8)[0]
            .get("error")
            .is_some()
    );
    assert!(session.tick(20000).is_empty());
    assert_eq!(session.backend.runs.len(), 2);
    assert!(
        call(&mut session, "shutdown", json!({}), 20000)[0]
            .get("result")
            .is_some()
    );
    assert!(session.shutdown);

    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True")], None);
    session.tick(10000);
    session.backend.fail_persist = true;
    assert!(
        action(&mut session, 1, "confirm", None, 7)[0]
            .get("error")
            .is_some()
    );
    assert!(session.fatal.as_ref().unwrap().contains("disk full"));
    assert!(session.tick(20000).is_empty());
}

#[test]
fn failures_are_terminal_until_explicit_retry_and_recording_failure_keeps_usage() {
    let mut session = Session::new(Fake {
        fail_job: true,
        ..Fake::default()
    });
    open(&mut session, &[(1, "True")], None);
    session.tick(10000);
    assert_eq!(session.backend.runs.len(), 1);
    assert!(session.tick(20000).is_empty());
    assert!(
        call(
            &mut session,
            "retry",
            json!({"document":"/tmp/serve-notes.tm","revision":7,"block":id(1)}),
            30000
        )[0]
        .get("result")
        .is_some()
    );
    session.backend.fail_job = false;
    session.backend.unhealthy = true;
    let pushes = session.tick(40000);
    assert!(
        session
            .fatal
            .as_ref()
            .unwrap()
            .contains("recording failure")
    );
    assert_eq!(session.backend.runs.len(), 2);
    assert!(session.engine.as_ref().unwrap().usage().calls > 0);
    assert!(pushes[2]["params"]["data"]["fatal"].is_string());
    assert!(session.tick(50000).is_empty());
}

#[test]
fn bounded_framing_ascii_roundtrip_and_eof() {
    let mut reader = io::Cursor::new(b"{}\r\n{}\n");
    assert_eq!(read_line(&mut reader).unwrap().unwrap(), b"{}\r\n");
    assert_eq!(read_line(&mut reader).unwrap().unwrap(), b"{}\n");
    assert!(read_line(&mut reader).unwrap().is_none());
    assert!(read_line(&mut io::Cursor::new(b"{}")).is_err());
    assert!(read_line(&mut io::Cursor::new(vec![b'x'; MAX_LINE_BYTES + 1])).is_err());
    let original = json!({"jsonrpc":"2.0","method":"diagnostics","params":{"text":"∀ x 😀\n"}});
    let mut output = Vec::new();
    write_messages(&mut output, vec![original.clone()]).unwrap();
    assert!(output.is_ascii());
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), original);
}

#[test]
fn transitive_dependents_leave_the_environment_and_need_reviewed_ancestors() {
    let mut session = Session::new(Fake::default());
    let blocks = [
        (1, "True"),
        (2, "True <reference|b1>"),
        (3, "True <reference|b2>"),
        (4, "True"),
    ];
    open(&mut session, &blocks, None);
    for _ in 0..4 {
        session.tick(10000);
    }
    for number in [3, 2] {
        assert!(
            action(&mut session, number, "confirm", None, 7)[0]
                .get("result")
                .is_some()
        );
        assert_ne!(
            session.engine.as_ref().unwrap().view().blocks[(number - 1) as usize].status,
            BlockStatus::StatementConfirmed
        );
    }
    action(&mut session, 1, "confirm", None, 7);
    assert!(
        session.engine.as_ref().unwrap().view().blocks[..3]
            .iter()
            .all(|block| block.status == BlockStatus::StatementConfirmed)
    );
    let edited = [(1, "False"), blocks[1], blocks[2], blocks[3]];
    change(&mut session, &edited, "", 8);
    assert_eq!(session.backend.environment.declarations, [id(4)]);
    assert_eq!(
        session.environment_events(),
        ["restore:0", &format!("replay:{}", id(4))]
    );
    assert_eq!(
        session
            .engine
            .as_ref()
            .unwrap()
            .dirty()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [id(1), id(2), id(3)]
    );
    for _ in 0..3 {
        session.tick(10000);
    }
    assert_eq!(&session.backend.runs[4..], [id(1), id(2), id(3)]);
}

#[test]
fn persisted_human_review_is_reused_only_after_the_statement_is_rechecked() {
    let mut first = Session::new(Fake::default());
    open(&mut first, &[(1, "True")], None);
    first.tick(10000);
    action(&mut first, 1, "confirm", None, 7);
    let store = ReviewStore::from_json(first.backend.stores.last().unwrap()).unwrap();
    let mut second = Session::new(Fake {
        store,
        ..Fake::default()
    });
    open(&mut second, &[(1, "True")], None);
    assert_ne!(
        second.engine.as_ref().unwrap().view().blocks[0].status,
        BlockStatus::StatementConfirmed
    );
    second.tick(10000);
    assert_eq!(
        second.engine.as_ref().unwrap().view().blocks[0].status,
        BlockStatus::StatementConfirmed
    );
    assert_eq!(second.backend.environment.declarations, [id(1)]);
}

#[test]
fn retry_pushes_pending_status_for_every_removed_declaration_until_reapplied() {
    let mut session = Session::new(Fake::default());
    open(
        &mut session,
        &[(1, "True"), (2, "True <reference|b1>")],
        None,
    );
    session.tick(10000);
    session.tick(10000);
    action(&mut session, 1, "confirm", None, 7);
    action(&mut session, 2, "confirm", None, 7);
    assert_eq!(
        session
            .engine
            .as_ref()
            .unwrap()
            .view()
            .counts
            .statement_confirmed,
        2
    );
    let messages = call(
        &mut session,
        "retry",
        json!({"document":"/tmp/serve-notes.tm", "revision":7,"block":id(1)}),
        20000,
    );
    assert!(messages[0].get("result").is_some());
    assert!(session.backend.environment.declarations.is_empty());
    let view = &messages[1]["params"]["data"]["view"];
    assert_eq!(view["needs_rerun"], true);
    assert_eq!(view["counts"]["statement_confirmed"], 0);
    for block in view["blocks"].as_array().unwrap() {
        assert_eq!(block["needs_rerun"], true);
        assert_eq!(block["status"], "sir_drafted");
        assert!(block["lean_statement"].is_null());
    }
    session.tick(30000);
    let view = session.engine.as_ref().unwrap().view();
    assert_eq!(view.counts.statement_confirmed, 1);
    assert!(
        view.blocks
            .iter()
            .find(|block| block.block_id == id(2))
            .unwrap()
            .needs_rerun
    );
    session.tick(30000);
    let view = session.engine.as_ref().unwrap().view();
    assert_eq!(view.counts.statement_confirmed, 2);
    assert!(!view.needs_rerun);
}

#[test]
fn continuously_queued_cursor_and_unknown_requests_cannot_starve_unrelated_work() {
    let mut session = Session::new(Fake::default());
    open(&mut session, &[(1, "True"), (2, "True")], Some(id(1)));
    // Keep every receive immediately ready, so no scheduling depends on a recv timeout. The
    // production reader retains its bounded sync_channel(16); this preloaded receiver makes the
    // starvation regression deterministic without relying on thread timing.
    let (sender, receiver) = mpsc::channel();
    for index in 0..1000 {
        let mut value = if index % 2 == 0 {
            json!({"jsonrpc":"2.0","method":"cursor","params":{"document":"/tmp/serve-notes.tm", "revision":7,"active":id(1)}})
        } else {
            json!({"jsonrpc":"2.0","method":"unknown","params":{}})
        };
        value["id"] = json!(index);
        sender
            .send(Ok(Some(serde_json::to_vec(&value).unwrap())))
            .unwrap();
    }
    sender
        .send(Ok(Some(
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1000,"method":"shutdown"})).unwrap(),
        )))
        .unwrap();
    let mut time = 0;
    let mut output = Vec::new();
    drive(&mut session, &receiver, &mut output, || {
        time += 1;
        time
    })
    .unwrap();
    assert_eq!(session.backend.runs, [id(2)]);
    assert!(session.shutdown);
    let messages: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let ids: Vec<_> = messages
        .iter()
        .filter_map(|value| value.get("id").and_then(Value::as_u64))
        .collect();
    assert_eq!(
        ids,
        (0..=1000).collect::<Vec<_>>(),
        "request responses stay in arrival order"
    );
    let applied = messages
        .iter()
        .position(|message| {
            message["method"] == "blockStatus"
                && message["params"]["data"]["view"]["blocks"]
                    .as_array()
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["block_id"] == id(2).to_string()
                                && block["status"] == "sir_drafted"
                        })
                    })
        })
        .unwrap();
    let shutdown = messages
        .iter()
        .position(|message| message["id"] == 1000)
        .unwrap();
    assert!(applied < shutdown);
}

#[test]
fn open_followed_by_queued_shutdown_or_eof_never_runs_a_job() {
    for shutdown in [true, false] {
        let mut session = Session::new(Fake::default());
        call(&mut session, "initialize", json!({}), 0);
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(Some(request(
                "didOpen",
                json!({"document":"/tmp/serve-notes.tm","revision":7,
            "source":source(&[(1,"True")],"")}),
            ))))
            .unwrap();
        sender
            .send(if shutdown {
                Ok(Some(request("shutdown", json!({}))))
            } else {
                Ok(None)
            })
            .unwrap();
        let mut time = 0;
        let mut output = Vec::new();
        // Opening consumes enough synthetic time for a job to become eligible before the next
        // receive. Pending shutdown/EOF must still win over that scheduling opportunity.
        drive(&mut session, &receiver, &mut output, || {
            time += 1000;
            time
        })
        .unwrap();
        assert!(session.engine.is_some());
        assert!(session.backend.runs.is_empty());
        assert_eq!(session.shutdown, shutdown);
        let responses: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .filter(|value: &Value| value.get("id").is_some())
            .collect();
        assert_eq!(responses.len(), if shutdown { 2 } else { 1 });
        assert!(responses.iter().all(|value| value.get("result").is_some()));
    }
}

#[test]
fn a_queued_change_is_applied_before_the_first_job_and_keeps_response_order() {
    let mut session = Session::new(Fake::default());
    call(&mut session, "initialize", json!({}), 0);
    let (sender, receiver) = mpsc::channel();
    let mut inputs = vec![
        json!({"jsonrpc":"2.0","id":0,"method":"didOpen","params":{"document":"/tmp/serve-notes.tm","revision":7,"source":source(&[(1,"Old text")],"")}}),
        json!({"jsonrpc":"2.0","id":1,"method":"didChange","params":{"document":"/tmp/serve-notes.tm","revision":8,"source":source(&[(1,"Latest text")],"")}}),
    ];
    // More than one bounded input turn, so eligible work gets a scheduling opportunity while
    // traffic remains queued, and shutdown arrives only after that opportunity.
    for index in 2..64 {
        inputs.push(json!({"jsonrpc":"2.0","id":index,"method":"unknown"}));
    }
    inputs.push(json!({"jsonrpc":"2.0","id":64,"method":"shutdown"}));
    for input in inputs {
        sender
            .send(Ok(Some(serde_json::to_vec(&input).unwrap())))
            .unwrap();
    }
    let mut time = 0;
    let mut output = Vec::new();
    drive(&mut session, &receiver, &mut output, || {
        time += 1000;
        time
    })
    .unwrap();
    assert_eq!(session.revision, 8);
    assert_eq!(
        session.backend.runs,
        [id(1)],
        "never formalize the obsolete open revision"
    );
    let engine = session.engine.as_ref().unwrap();
    assert!(engine.dirty().is_empty());
    assert!(
        String::from_utf8_lossy(engine.document().canonical(id(1)).unwrap())
            .contains("Latest text")
    );
    let messages: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let ids: Vec<_> = messages
        .iter()
        .filter_map(|value| value.get("id").and_then(Value::as_u64))
        .collect();
    assert_eq!(ids, (0..=64).collect::<Vec<_>>());
    let applied: Vec<_> = messages
        .iter()
        .filter(|value| {
            value["method"] == "blockStatus"
                && value["params"]["data"]["view"]["blocks"]
                    .as_array()
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| block["status"] == "sir_drafted")
                    })
        })
        .collect();
    assert!(!applied.is_empty());
    assert!(applied.iter().all(|value| value["params"]["revision"] == 8));
}
