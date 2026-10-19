//! Offline contract, transport, validation, and record/replay tests.

use mls_models::{
    Answer, CallContext, CallMetadata, ChatMessage, ChoiceAnswer, ChoiceAssessment, DecisionReply,
    DecisionRequest, GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelConfig,
    ModelError, NoulAnswer, NoulAssessment, OpenRouterClient, ProviderFailure, Question,
    RecordedCall, RecordingClient, ReplayClient, Usage,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::mpsc, thread};
use tiny_http::{Header, Response, Server, StatusCode};

const GENERATION_MODEL: &str = "anthropic/claude-sonnet-5";
const DECISION_MODEL: &str = "typesafe/jev-1.14";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    })
}

fn generation_request() -> GenerationRequest {
    GenerationRequest {
        context: CallContext {
            block_id: "block-1".into(),
            attempt: 0,
        },
        stage: GenerationStage::Sir,
        messages: vec![ChatMessage {
            role: "user".into(),
            content: "Return a tiny object.".into(),
        }],
        schema_version: "test-v1".into(),
        schema: schema(),
        max_tokens: 32,
    }
}

fn decision_request() -> DecisionRequest {
    DecisionRequest {
        context: CallContext {
            block_id: "block-1".into(),
            attempt: 1,
        },
        state: json!({"block": "x"}),
        questions: BTreeMap::from([
            (
                "kind".into(),
                Question::Choice {
                    instructions: "Classify.".into(),
                    criteria: BTreeMap::from([
                        ("claim".into(), "A claim".into()),
                        ("commentary".into(), "Commentary".into()),
                    ]),
                },
            ),
            (
                "finished".into(),
                Question::Noul {
                    instructions: "Is it finished?".into(),
                    criteria: BTreeMap::from([
                        ("false".into(), "Incomplete".into()),
                        ("true".into(), "Complete".into()),
                    ]),
                },
            ),
        ]),
    }
}

fn config(base_url: String) -> ModelConfig {
    ModelConfig {
        generation_model: GENERATION_MODEL.into(),
        decision_model: DECISION_MODEL.into(),
        base_url,
        request_timeout_seconds: 5,
        default_max_tokens: 32,
    }
}

fn serve_once(
    status: u16,
    body: &'static str,
) -> (String, mpsc::Receiver<(String, Vec<Header>, Value)>) {
    let server = Server::http("127.0.0.1:0").expect("bind local mock server");
    let address = format!("http://{}", server.server_addr());
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut request = server.recv().expect("receive request");
        let mut bytes = Vec::new();
        request
            .as_reader()
            .read_to_end(&mut bytes)
            .expect("read request");
        let parsed = serde_json::from_slice(&bytes).expect("request JSON");
        let capture = (request.url().to_owned(), request.headers().to_vec(), parsed);
        request
            .respond(Response::from_string(body).with_status_code(StatusCode(status)))
            .expect("respond");
        let _ = sender.send(capture);
    });
    (address, receiver)
}

#[test]
fn generation_uses_strict_schema_endpoint_and_auth_without_temperature() {
    let body = r#"{"id":"gen-1","model":"anthropic/claude-sonnet-5-20261001","provider":"Anthropic","choices":[{"message":{"content":"{\"answer\":\"ok\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#;
    let (base_url, captured) = serve_once(200, body);
    let mut client = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
    let reply = client.generate(&generation_request()).unwrap();
    assert_eq!(reply.output, json!({"answer": "ok"}));
    assert_eq!(reply.metadata.usage.cost, None);
    assert_eq!(reply.metadata.provider_response["id"], "gen-1");
    assert_eq!(
        reply.metadata.resolved_model,
        "anthropic/claude-sonnet-5-20261001"
    );

    let (path, headers, request) = captured.recv().unwrap();
    assert_eq!(path, "/api/v1/chat/completions");
    assert!(
        headers
            .iter()
            .any(|header| header.field.equiv("authorization"))
    );
    assert_eq!(request["model"], GENERATION_MODEL);
    assert_eq!(request["provider"]["require_parameters"], true);
    assert_eq!(request["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        request["response_format"]["json_schema"]["schema"],
        schema()
    );
    assert!(request.get("temperature").is_none());
}

#[test]
fn decisions_use_alpha_route_and_preserve_confidence_separately() {
    let body = r#"{"id":"dec-1","model":"typesafe/jev-1.14-20261014","answers":{"finished":{"type":"noul","noul":0.93},"kind":{"type":"choice","choice":"claim","probabilities":{"claim":0.91,"commentary":0.09},"confidence":0.97}},"usage":{"input_tokens":12,"output_tokens":0}}"#;
    let (base_url, captured) = serve_once(200, body);
    let request = decision_request();
    let mut client = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
    let reply = client.decide(&request).unwrap();
    let Answer::Choice(choice) = &reply.answers["kind"] else {
        panic!("expected choice")
    };
    assert_eq!(choice.confidence, Some(0.97));
    assert_eq!(choice.probabilities.as_ref().unwrap()["claim"], 0.91);
    assert_eq!(reply.metadata.usage.cost, None);
    let (path, _, wire) = captured.recv().unwrap();
    assert_eq!(path, "/api/alpha/decisions");
    assert_eq!(wire["model"], DECISION_MODEL);
    assert_eq!(wire["questions"]["finished"]["type"], "noul");
}

#[test]
fn decisions_reject_malformed_present_distribution() {
    let body = r#"{"model":"typesafe/jev-1.14","answers":{"finished":{"type":"noul","noul":0.93},"kind":{"type":"choice","choice":"claim","probabilities":{"claim":1.2,"commentary":-0.2},"confidence":0.97}},"usage":{"input_tokens":12,"output_tokens":0}}"#;
    let (base_url, _) = serve_once(200, body);
    let mut client = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
    assert!(client.decide(&decision_request()).is_err());
}

#[test]
fn malformed_null_refusal_truncation_schema_and_http_errors_are_rejected() {
    let cases = [
        (200, "not-json", "malformed provider response JSON"),
        (
            200,
            r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#,
            "null or absent",
        ),
        (
            200,
            r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":"{}","refusal":"no"},"finish_reason":"stop"}]}"#,
            "refused",
        ),
        (
            200,
            r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":"{}"},"finish_reason":"length"}]}"#,
            "did not finish normally",
        ),
        (
            200,
            r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":"{}"},"finish_reason":"stop"}]}"#,
            "schema validation failed",
        ),
        (
            429,
            r#"{"error":{"message":"local-test-secret"}}"#,
            "HTTP status 429",
        ),
    ];
    for (status, body, expected) in cases {
        let leaked: &'static str = Box::leak(body.to_owned().into_boxed_str());
        let (base_url, _) = serve_once(status, leaked);
        let mut client = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
        let error = client.generate(&generation_request()).unwrap_err();
        if let ModelError::Provider { failure, .. } = &error {
            assert!(!failure.response.to_string().contains("local-test-secret"));
        }
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!error.to_string().contains("sensitive upstream detail"));
        assert!(!error.to_string().contains("local-test-secret"));
    }
}

#[test]
fn confidence_helpers_are_conservative_at_boundaries() {
    assert_eq!(NoulAnswer { noul: 0.9 }.assess(0.9), NoulAssessment::Yes);
    assert_eq!(NoulAnswer { noul: 0.1 }.assess(0.9), NoulAssessment::No);
    assert_eq!(
        NoulAnswer { noul: 0.5 }.assess(0.9),
        NoulAssessment::Uncertain
    );
    let allowed = BTreeMap::from([("a".into(), "A".into()), ("b".into(), "B".into())]);
    let missing = ChoiceAnswer {
        choice: "a".into(),
        probabilities: None,
        confidence: Some(0.99),
    };
    assert_eq!(missing.assess(&allowed, 0.9), ChoiceAssessment::Uncertain);
    let confidence_differs = ChoiceAnswer {
        choice: "a".into(),
        probabilities: Some(BTreeMap::from([("a".into(), 0.89), ("b".into(), 0.11)])),
        confidence: Some(0.99),
    };
    assert_eq!(
        confidence_differs.assess(&allowed, 0.9),
        ChoiceAssessment::Uncertain
    );
}

#[derive(Clone)]
struct FakeClient {
    generation: Result<GenerationReply, String>,
}

impl ModelClient for FakeClient {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        self.generation.clone().map_err(ModelError::InvalidResponse)
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        Err(ModelError::InvalidResponse("synthetic failure".into()))
    }
}

fn generation_reply() -> GenerationReply {
    GenerationReply {
        output: json!({"answer": "recorded"}),
        metadata: CallMetadata {
            http_status: 200,
            configured_model: GENERATION_MODEL.into(),
            resolved_model: "anthropic/claude-sonnet-5-20261001".into(),
            request_id: Some("id".into()),
            provider: Some("synthetic".into()),
            latency_ms: 7,
            usage: Usage {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
                cost: None,
            },
            provider_response: json!({"synthetic": true}),
        },
        finish_reason: "stop".into(),
    }
}

#[test]
fn record_replay_round_trip_errors_and_exact_mismatch() {
    let directory = std::env::temp_dir().join(format!("mls-models-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let fake = FakeClient {
        generation: Ok(generation_reply()),
    };
    let mut recorder = RecordingClient::new(
        fake,
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    let request = generation_request();
    assert_eq!(recorder.generate(&request).unwrap(), generation_reply());
    assert!(
        recorder
            .decide(&decision_request())
            .unwrap_err()
            .to_string()
            .contains("synthetic failure")
    );

    let files = std::fs::read_dir(&directory).unwrap().count();
    assert_eq!(files, 2);
    let serialized = std::fs::read_to_string(directory.join("call-0000.json")).unwrap();
    assert!(!serialized.contains("secret"));
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(replay.generate(&request).unwrap(), generation_reply());
    assert!(
        replay
            .decide(&decision_request())
            .unwrap_err()
            .to_string()
            .contains("synthetic failure")
    );

    let mut mismatch =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    let mut changed = request;
    changed.max_tokens += 1;
    assert!(matches!(
        mismatch.generate(&changed),
        Err(ModelError::ReplayMismatch)
    ));
    for changed in [
        {
            let mut value = generation_request();
            value.messages[0].content.push('!');
            value
        },
        {
            let mut value = generation_request();
            value.schema["required"] = json!([]);
            value
        },
    ] {
        let mut mismatch =
            ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert!(matches!(
            mismatch.generate(&changed),
            Err(ModelError::ReplayMismatch)
        ));
    }
    let mut wrong_model =
        ReplayClient::from_directory(&directory, "anthropic/other", DECISION_MODEL).unwrap();
    assert!(matches!(
        wrong_model.generate(&generation_request()),
        Err(ModelError::ReplayMismatch)
    ));

    let original = std::fs::read(directory.join("call-0000.json")).unwrap();
    let fake = FakeClient {
        generation: Ok(generation_reply()),
    };
    let mut second = RecordingClient::new(
        fake,
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic-second",
    );
    second.generate(&generation_request()).unwrap();
    assert_eq!(
        std::fs::read(directory.join("call-0000.json")).unwrap(),
        original
    );
    assert!(directory.join("call-0002.json").exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn provider_failure_recording_keeps_redacted_audit_data_and_replays_error() {
    let directory = std::env::temp_dir().join(format!("mls-model-errors-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let failure = ProviderFailure {
        status: 200,
        response: json!({"model": GENERATION_MODEL, "content": "[REDACTED]"}),
        resolved_model: Some(GENERATION_MODEL.into()),
        usage: Some(Usage {
            input_tokens: Some(2),
            output_tokens: Some(1),
            total_tokens: Some(3),
            cost: Some(0.01),
        }),
    };
    struct ProviderFailFake(ProviderFailure);
    impl ModelClient for ProviderFailFake {
        fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
            Err(ModelError::Provider {
                message: "refused".into(),
                failure: Box::new(self.0.clone()),
            })
        }
        fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
            unreachable!()
        }
    }
    let mut recorder = RecordingClient::new(
        ProviderFailFake(failure),
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert!(recorder.generate(&generation_request()).is_err());
    let text = std::fs::read_to_string(directory.join("call-0000.json")).unwrap();
    assert!(text.contains("0.01"));
    assert!(text.contains("[REDACTED]"));
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert!(matches!(
        replay.generate(&generation_request()),
        Err(ModelError::Provider { .. })
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn replay_revalidates_recorded_output_and_integrity() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/recordings/models/success/call-0000.json"
    );
    let mut call: RecordedCall = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    call.response.as_mut().unwrap()["output"]["answer"] = json!(7);
    let mut replay = ReplayClient::new(vec![call], GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert!(matches!(
        replay.generate(&generation_request()),
        Err(ModelError::Schema(_))
    ));

    let mut corrupt: RecordedCall = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    corrupt.version += 1;
    assert!(ReplayClient::new(vec![corrupt], GENERATION_MODEL, DECISION_MODEL).is_err());

    for mutation in ["finish", "status", "usage", "model"] {
        let mut call: RecordedCall = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        match mutation {
            "finish" => call.response.as_mut().unwrap()["finish_reason"] = json!("length"),
            "status" => call.response.as_mut().unwrap()["metadata"]["http_status"] = json!(429),
            "usage" => call.usage.as_mut().unwrap().cost = Some(99.0),
            "model" => call.resolved_model = Some("anthropic/claude-sonnet-5-wrong".into()),
            _ => unreachable!(),
        }
        let mut replay = ReplayClient::new(vec![call], GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert!(
            replay.generate(&generation_request()).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn provider_evidence_is_recovered_and_credentials_are_redacted() {
    let directory =
        std::env::temp_dir().join(format!("mls-model-redaction-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let reflected = r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":"{}"},"finish_reason":"local-test-secret"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"cost":0.25}}"#;
    let (base_url, _) = serve_once(200, reflected);
    let live = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
    let mut recorder = RecordingClient::new(
        live,
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    let error = recorder.generate(&generation_request()).unwrap_err();
    assert!(!format!("{error:?}{error}").contains("local-test-secret"));
    let log = std::fs::read_to_string(directory.join("call-0000.json")).unwrap();
    assert!(!log.contains("local-test-secret"));
    assert!(log.contains("0.25"));
    std::fs::remove_dir_all(directory).unwrap();

    let long_padding = "x".repeat(5000);
    let bodies = [
        (
            200,
            r#"{"model":"local-test-secret","choices":[],"usage":{"cost":0.3}}"#.to_owned(),
            None,
        ),
        (
            200,
            format!(
                r#"{{"model":"{GENERATION_MODEL}","usage":{{"prompt_tokens":4,"cost":0.4}},"padding":"{long_padding}"}}"#
            ),
            Some(0.4),
        ),
        (
            429,
            format!(r#"{{"model":"{GENERATION_MODEL}","usage":{{"input_tokens":5,"cost":0.5}}}}"#),
            Some(0.5),
        ),
    ];
    for (status, body, expected_cost) in bodies {
        let body: &'static str = Box::leak(body.into_boxed_str());
        let (base_url, _) = serve_once(status, body);
        let mut client = OpenRouterClient::new(config(base_url), "local-test-secret").unwrap();
        let error = client.generate(&generation_request()).unwrap_err();
        let ModelError::Provider { failure, .. } = error else {
            panic!("provider evidence")
        };
        assert!(!format!("{failure:?}").contains("local-test-secret"));
        if let Some(cost) = expected_cost {
            assert_eq!(failure.usage.unwrap().cost, Some(cost));
        }
    }
}

#[test]
fn checked_in_synthetic_fixture_replays() {
    let directory = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/recordings/models/success"
    );
    let mut replay =
        ReplayClient::from_directory(directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(
        replay.generate(&generation_request()).unwrap().output,
        json!({"answer": "recorded"})
    );
}

#[test]
fn live_openrouter_smoke_explicitly_gated() {
    if std::env::var("MLS_LIVE").as_deref() != Ok("1") {
        return;
    }
    let config = ModelConfig::from_path(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/models.json"
    ))
    .unwrap();
    let generation_model = config.generation_model.clone();
    let decision_model = config.decision_model.clone();
    let live = OpenRouterClient::from_env(config).unwrap();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.mls/log/live-smoke")
        .join(format!("{}-{unique}", std::process::id()));
    let mut client = RecordingClient::new(
        live,
        directory,
        generation_model,
        decision_model,
        "live-openrouter",
    );
    let reply = client.generate(&generation_request()).unwrap();
    assert!(reply.output.get("answer").is_some());
    let decision = client.decide(&decision_request()).unwrap();
    assert_eq!(decision.answers.len(), 2);
    eprintln!(
        "live smoke: generation_model={} generation_cost={:?} generation_latency_ms={} decision_model={} decision_cost={:?} decision_latency_ms={}",
        reply.metadata.resolved_model,
        reply.metadata.usage.cost,
        reply.metadata.latency_ms,
        decision.metadata.resolved_model,
        decision.metadata.usage.cost,
        decision.metadata.latency_ms
    );
}
