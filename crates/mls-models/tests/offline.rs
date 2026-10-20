//! Offline contract, transport, validation, and record/replay tests.

use mls_models::{
    Answer, CallContext, CallMetadata, ChatMessage, ChoiceAnswer, ChoiceAssessment, DecisionReply,
    DecisionRequest, EmbeddingClient, EmbeddingInputType, EmbeddingReply, EmbeddingRequest,
    GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelConfig, ModelError,
    NoulAnswer, NoulAssessment, OpenRouterClient, ProviderFailure, Question, RecordedCall,
    RecordingClient, ReplayClient, RetrievalConfig, Usage,
};
use serde_json::{Value, json};
use std::{cell::Cell, collections::BTreeMap, path::PathBuf, rc::Rc, sync::mpsc, thread};
use tiny_http::{Header, Response, Server, StatusCode};

const GENERATION_MODEL: &str = "anthropic/claude-sonnet-5";
const DECISION_MODEL: &str = "typesafe/jev-1.14";
const EMBEDDING_MODEL: &str = "baai/bge-m3";

fn embedding_request() -> EmbeddingRequest {
    EmbeddingRequest {
        context: CallContext {
            block_id: "retrieval-batch-1".into(),
            attempt: 0,
        },
        model: EMBEDDING_MODEL.into(),
        input: vec![
            "A continuous function on a compact space.".into(),
            "A convergent real sequence.".into(),
        ],
        dimensions: 3,
        input_type: EmbeddingInputType::SearchDocument,
    }
}

fn embedding_config(base_url: String) -> ModelConfig {
    let mut config = config(base_url);
    config.retrieval.embedding_dimensions = 3;
    config
}

fn embedding_response() -> Value {
    json!({
        "id": "emb-1", "model": EMBEDDING_MODEL, "object": "list",
        "data": [
            {"index": 1, "object": "embedding", "embedding": [0.0, 1.0, -1.0]},
            {"index": 0, "object": "embedding", "embedding": [1.0, 0.0, 2.0]}
        ],
        "usage": {"prompt_tokens": 8, "total_tokens": 8}
    })
}

fn serve_json(status: u16, body: Value) -> (String, mpsc::Receiver<(String, Vec<Header>, Value)>) {
    serve_once(status, Box::leak(body.to_string().into_boxed_str()))
}

#[test]
fn retrieval_configuration_defaults_without_changing_existing_models() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/models.json");
    let checked_in = ModelConfig::from_path(path).unwrap();
    assert_eq!(checked_in.generation_model, GENERATION_MODEL);
    assert_eq!(checked_in.decision_model, DECISION_MODEL);
    assert_eq!(checked_in.retrieval, RetrievalConfig::default());
    assert_eq!(checked_in.retrieval.embedding_model, EMBEDDING_MODEL);
    assert_eq!(checked_in.retrieval.embedding_dimensions, 1024);
    let mut legacy = serde_json::to_value(&checked_in).unwrap();
    legacy.as_object_mut().unwrap().remove("retrieval");
    assert_eq!(
        serde_json::from_value::<ModelConfig>(legacy).unwrap(),
        checked_in
    );
}

#[test]
fn embeddings_send_typed_batches_and_restore_input_order_without_inventing_usage() {
    let (base_url, captured) = serve_json(200, embedding_response());
    let mut client =
        OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
    let request = embedding_request();
    let reply = client.embed(&request).unwrap();
    assert_eq!(
        reply.embeddings,
        [vec![1.0, 0.0, 2.0], vec![0.0, 1.0, -1.0]]
    );
    assert_eq!(
        reply.metadata.usage,
        Usage {
            input_tokens: Some(8),
            total_tokens: Some(8),
            ..Usage::default()
        }
    );
    let (path, headers, body) = captured.recv().unwrap();
    assert_eq!(path, "/api/v1/embeddings");
    assert!(
        headers
            .iter()
            .any(|header| header.field.equiv("authorization"))
    );
    assert_eq!(body["model"], EMBEDDING_MODEL);
    assert_eq!(body["input"], json!(request.input));
    assert_eq!(body["dimensions"], 3);
    assert_eq!(body["encoding_format"], "float");
    assert_eq!(body["input_type"], "search_document");
    assert!(body.get("context").is_none());
    assert!(body.get("messages").is_none());

    let mut response = embedding_response();
    response.as_object_mut().unwrap().remove("usage");
    let (base_url, _) = serve_json(200, response);
    let mut client =
        OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
    assert_eq!(
        client.embed(&request).unwrap().metadata.usage,
        Usage::default()
    );
}

#[test]
fn embedding_query_purpose_and_reported_zero_cost_are_preserved() {
    let mut response = embedding_response();
    response["usage"]["cost"] = json!(0.0);
    let (base_url, captured) = serve_json(200, response);
    let mut client =
        OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
    let mut request = embedding_request();
    request.input_type = EmbeddingInputType::SearchQuery;
    assert_eq!(
        client.embed(&request).unwrap().metadata.usage.cost,
        Some(0.0)
    );
    assert_eq!(captured.recv().unwrap().2["input_type"], "search_query");
}

#[test]
fn embedding_canonical_provider_case_records_and_replays_without_changing_request_identity() {
    for (index, resolved) in ["BAAI/bge-m3", "BAAI/BGE-M3-V1"].into_iter().enumerate() {
        let directory = std::env::temp_dir().join(format!(
            "mls-embeddings-canonical-case-{index}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut body = embedding_response();
        body["model"] = json!(resolved);
        let (origin, captured) = serve_json(200, body);
        let client = OpenRouterClient::new(embedding_config(origin), "local-test-secret").unwrap();
        let mut recorder = RecordingClient::new(
            client,
            &directory,
            GENERATION_MODEL,
            DECISION_MODEL,
            "synthetic",
        );
        let reply = recorder.embed(&embedding_request()).unwrap();
        assert_eq!(reply.metadata.configured_model, EMBEDDING_MODEL);
        assert_eq!(reply.metadata.resolved_model, resolved);
        assert_eq!(reply.metadata.provider_response["model"], resolved);
        assert_eq!(captured.recv().unwrap().2["model"], EMBEDDING_MODEL);
        let mut replay =
            ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert_eq!(replay.embed(&embedding_request()).unwrap(), reply);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn rejected_embedding_costs_are_unknown_while_valid_tokens_still_aggregate() {
    let mut usages = Vec::new();
    for invalid_cost in [json!(-0.25), json!("NaN"), json!("Infinity")] {
        let mut body = embedding_response();
        body["usage"] = json!({"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10, "cost": invalid_cost});
        let (base_url, _) = serve_json(200, body);
        let mut client =
            OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        let ModelError::Provider { failure, .. } = client.embed(&embedding_request()).unwrap_err()
        else {
            panic!("provider failure expected")
        };
        let usage = failure.usage.unwrap();
        assert_eq!(
            usage.cost, None,
            "invalid reported cost must not become known spending"
        );
        assert_eq!(usage.total_tokens, Some(10));
        usages.push(usage);
    }
    assert_eq!(
        usages
            .iter()
            .filter_map(|usage| usage.input_tokens)
            .sum::<u64>(),
        24
    );
    assert_eq!(
        usages
            .iter()
            .filter_map(|usage| usage.output_tokens)
            .sum::<u64>(),
        6
    );
    assert_eq!(
        usages.iter().filter_map(|usage| usage.cost).sum::<f64>(),
        0.0
    );
    assert_eq!(
        usages.iter().filter(|usage| usage.cost.is_none()).count(),
        3
    );
}

#[test]
fn embedding_success_recordings_store_vectors_once_with_stable_audit_hashes() {
    let directory =
        std::env::temp_dir().join(format!("mls-embeddings-compact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut hashes = Vec::new();
    for index in 0..2 {
        let mut body = embedding_response();
        if index == 1 {
            body["data"].as_array_mut().unwrap().reverse();
        }
        let (base_url, _) = serve_once_with_headers(
            200,
            Box::leak(body.to_string().into_boxed_str()),
            vec![
                Header::from_bytes("X-RateLimit-Remaining", "99").unwrap(),
                Header::from_bytes("Authorization", "sensitive-other-header").unwrap(),
            ],
        );
        let live = OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        let mut recorder = RecordingClient::new(
            live,
            &directory,
            GENERATION_MODEL,
            DECISION_MODEL,
            "synthetic",
        );
        let reply = recorder.embed(&embedding_request()).unwrap();
        let summary = &reply.metadata.provider_response;
        assert!(summary.get("data").is_none());
        assert!(summary.get("embeddings").is_none());
        assert_eq!(summary["id"], "emb-1");
        assert_eq!(summary["model"], EMBEDDING_MODEL);
        assert_eq!(summary["object"], "list");
        assert_eq!(summary["usage"]["prompt_tokens"], 8);
        assert_eq!(summary["embedding_count"], 2);
        assert_eq!(summary["embedding_dimensions"], 3);
        assert_eq!(summary["response_headers"]["x-ratelimit-remaining"], "99");
        hashes.push(summary["embedding_sha256"].as_str().unwrap().to_owned());
        assert_eq!(hashes[index].len(), 64);
        let text =
            std::fs::read_to_string(directory.join(format!("call-{index:04}.json"))).unwrap();
        assert!(!text.contains("sensitive-other-header"));
        let call: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(call["provider_response"], *summary);
        assert_eq!(call["response"]["metadata"]["provider_response"], *summary);
        assert_eq!(call["response"]["embeddings"], json!(reply.embeddings));
    }
    assert_eq!(
        hashes[0], hashes[1],
        "wire row order must not change the vector hash"
    );
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(
        replay.embed(&embedding_request()).unwrap().embeddings,
        embedding_reply().embeddings
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn embedding_failures_keep_retry_headers_and_bound_large_evidence() {
    for large in [false, true] {
        let mut body = json!({"model": EMBEDDING_MODEL, "data": [], "usage": {"prompt_tokens": 8, "cost": 0.25}});
        if large {
            body["padding"] = json!("x".repeat(20_000));
        }
        let (base_url, _) = serve_once_with_headers(
            429,
            Box::leak(body.to_string().into_boxed_str()),
            vec![
                Header::from_bytes("Retry-After", "120").unwrap(),
                Header::from_bytes("X-RateLimit-Reset", "1900000000").unwrap(),
                Header::from_bytes("RateLimit-Limit", "500").unwrap(),
            ],
        );
        let mut client =
            OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        let ModelError::Provider { failure, .. } = client.embed(&embedding_request()).unwrap_err()
        else {
            panic!("provider failure expected")
        };
        assert_eq!(failure.usage.unwrap().cost, Some(0.25));
        let headers = &failure.response["_mls_response_headers"];
        assert_eq!(headers["retry-after"], "120");
        assert_eq!(headers["x-ratelimit-reset"], "1900000000");
        assert_eq!(headers["ratelimit-limit"], "500");
        assert!(failure.response.to_string().len() < 4096);
        assert_eq!(failure.response.get("truncated").is_some(), large);
    }
}

#[test]
fn remote_embeddings_require_explicit_environment_opt_in() {
    if std::env::var("MLS_LIVE").as_deref() == Ok("1") {
        return;
    }
    let mut client = OpenRouterClient::new(
        embedding_config("https://openrouter.ai".into()),
        "local-test-secret",
    )
    .unwrap();
    assert!(
        matches!(client.embed(&embedding_request()), Err(ModelError::InvalidResponse(message)) if message == "remote embeddings require MLS_LIVE=1")
    );
}

#[test]
fn embeddings_reject_redirects_without_contacting_a_second_origin() {
    let destination = Server::http("127.0.0.1:0").expect("bind redirect destination");
    let location = format!("http://{}/redirected", destination.server_addr());
    let (sent, received) = mpsc::channel();
    thread::spawn(move || {
        let request = destination
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let contacted = request.is_some();
        if let Some(request) = request {
            request
                .respond(Response::from_string(embedding_response().to_string()))
                .unwrap();
        }
        sent.send(contacted).unwrap();
    });
    let (base_url, captured) = serve_once_with_headers(
        307,
        "{}",
        vec![Header::from_bytes("Location", location.as_bytes()).unwrap()],
    );
    let mut client =
        OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
    let result = client.embed(&embedding_request());
    let contacted = received.recv().unwrap();
    assert!(
        !contacted,
        "embedding redirects must not send the batch to a second origin"
    );
    assert!(matches!(result, Err(ModelError::Provider { failure, .. }) if failure.status == 307));
    assert_eq!(captured.recv().unwrap().0, "/api/v1/embeddings");
}

#[test]
fn generation_keeps_its_existing_redirect_behavior() {
    let (destination, captured_destination) = serve_once(
        200,
        r#"{"model":"anthropic/claude-sonnet-5","choices":[{"message":{"content":"{\"answer\":\"ok\"}"},"finish_reason":"stop"}]}"#,
    );
    let (origin, captured_origin) = serve_once_with_headers(
        307,
        "{}",
        vec![
            Header::from_bytes("Location", format!("{destination}/redirected").as_bytes()).unwrap(),
        ],
    );
    let mut client = OpenRouterClient::new(config(origin), "local-test-secret").unwrap();
    assert_eq!(
        client.generate(&generation_request()).unwrap().output,
        json!({"answer": "ok"})
    );
    assert_eq!(
        captured_origin.recv().unwrap().0,
        "/api/v1/chat/completions"
    );
    assert_eq!(captured_destination.recv().unwrap().0, "/redirected");
}

#[test]
fn invalid_embedding_inputs_are_rejected_before_transport() {
    let mut client = OpenRouterClient::new(
        embedding_config("http://127.0.0.1:1".into()),
        "local-test-secret",
    )
    .unwrap();
    for mutation in ["empty", "blank", "model", "dimensions", "configuration"] {
        let mut request = embedding_request();
        match mutation {
            "empty" => request.input.clear(),
            "blank" => request.input[0] = " ".into(),
            "model" => request.model.clear(),
            "dimensions" => request.dimensions = 0,
            "configuration" => request.model = "openai/text-embedding-3-small".into(),
            _ => unreachable!(),
        }
        assert!(
            matches!(client.embed(&request), Err(ModelError::InvalidResponse(_))),
            "{mutation}"
        );
    }
}

#[test]
fn invalid_embedding_rows_models_and_cost_retain_known_failure_usage() {
    for mutation in [
        "missing",
        "extra",
        "duplicate",
        "range",
        "negative",
        "dimension",
        "base64",
        "coordinate",
        "model",
        "absent-model",
        "negative-cost",
    ] {
        let mut body = embedding_response();
        body["usage"]["cost"] = json!(0.25);
        match mutation {
            "missing" => {
                body["data"].as_array_mut().unwrap().pop();
            }
            "extra" => {
                let row = body["data"][0].clone();
                body["data"].as_array_mut().unwrap().push(row);
            }
            "duplicate" => body["data"][0]["index"] = json!(0),
            "range" => body["data"][0]["index"] = json!(2),
            "negative" => body["data"][0]["index"] = json!(-1),
            "dimension" => body["data"][0]["embedding"] = json!([1.0, 0.0]),
            "base64" => body["data"][0]["embedding"] = json!("AAAA"),
            "coordinate" => body["data"][0]["embedding"][0] = json!("NaN"),
            "model" => body["model"] = json!("baai/bge-m30"),
            "absent-model" => {
                body.as_object_mut().unwrap().remove("model");
            }
            "negative-cost" => body["usage"]["cost"] = json!(-0.25),
            _ => unreachable!(),
        }
        let (base_url, _) = serve_json(200, body);
        let mut client =
            OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        let ModelError::Provider { failure, .. } = client.embed(&embedding_request()).unwrap_err()
        else {
            panic!("{mutation}: provider failure expected")
        };
        assert_eq!(failure.status, 200);
        assert_eq!(
            failure.usage.as_ref().unwrap().input_tokens,
            Some(8),
            "{mutation}"
        );
        if mutation != "negative-cost" {
            assert_eq!(failure.usage.unwrap().cost, Some(0.25), "{mutation}");
        }
    }
    for coordinate in ["NaN", "Infinity", "1e400"] {
        let body = embedding_response().to_string().replacen(
            "[0.0,1.0,-1.0]",
            &format!("[{coordinate},1.0,-1.0]"),
            1,
        );
        let (base_url, _) = serve_once(200, Box::leak(body.into_boxed_str()));
        let mut client =
            OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        assert!(client.embed(&embedding_request()).is_err(), "{coordinate}");
    }
}

struct EmbeddingFake {
    result: Option<Result<EmbeddingReply, ModelError>>,
    calls: Rc<Cell<usize>>,
}

impl EmbeddingClient for EmbeddingFake {
    fn embed(&mut self, _: &EmbeddingRequest) -> Result<EmbeddingReply, ModelError> {
        self.calls.set(self.calls.get() + 1);
        self.result.take().expect("one scripted embedding call")
    }
}

fn embedding_reply() -> EmbeddingReply {
    let mut metadata = generation_reply().metadata;
    metadata.configured_model = EMBEDDING_MODEL.into();
    metadata.resolved_model = EMBEDDING_MODEL.into();
    metadata.provider_response = json!({
        "synthetic": true,
        "embedding_count": 2,
        "embedding_dimensions": 3,
        "embedding_sha256": "e382e02d6ca6f00ecc16e6cc441e61c8894660c5daea2ead7274c3c9f5efdc3e",
    });
    EmbeddingReply {
        embeddings: vec![vec![1.0, 0.0, 2.0], vec![0.0, 1.0, -1.0]],
        metadata,
    }
}

fn embedding_record(name: &str) -> RecordedCall {
    let directory =
        std::env::temp_dir().join(format!("mls-embeddings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut recorder = RecordingClient::new(
        EmbeddingFake {
            result: Some(Ok(embedding_reply())),
            calls: Rc::new(Cell::new(0)),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.embed(&embedding_request()).unwrap(),
        embedding_reply()
    );
    let record =
        serde_json::from_slice(&std::fs::read(directory.join("call-0000.json")).unwrap()).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    record
}

#[test]
fn embedding_record_replay_requires_every_canonical_request_field() {
    let call = embedding_record("exact");
    assert_eq!(call.endpoint_kind, "embedding");
    assert_eq!(call.request["endpoint"], "/api/v1/embeddings");
    assert_eq!(call.request["encoding_format"], "float");
    assert_eq!(call.request["input_type"], "search_document");
    let mut replay =
        ReplayClient::new(vec![call.clone()], GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(
        replay.embed(&embedding_request()).unwrap(),
        embedding_reply()
    );
    for mutation in [
        "block",
        "attempt",
        "model",
        "input",
        "order",
        "dimensions",
        "purpose",
    ] {
        let mut request = embedding_request();
        match mutation {
            "block" => request.context.block_id.push('2'),
            "attempt" => request.context.attempt += 1,
            "model" => request.model.push('2'),
            "input" => request.input[0].push('!'),
            "order" => request.input.reverse(),
            "dimensions" => request.dimensions += 1,
            "purpose" => request.input_type = EmbeddingInputType::SearchQuery,
            _ => unreachable!(),
        }
        let mut replay =
            ReplayClient::new(vec![call.clone()], GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert!(
            matches!(replay.embed(&request), Err(ModelError::ReplayMismatch)),
            "{mutation}"
        );
        assert_eq!(
            replay.embed(&embedding_request()).unwrap(),
            embedding_reply(),
            "a mismatch must not consume the recording"
        );
    }
}

#[test]
fn embedding_replay_revalidates_vector_shape_model_and_accounting_envelope() {
    let call = embedding_record("validation");
    for mutation in [
        "rows",
        "dimension",
        "coordinate",
        "finite-coordinate",
        "summary-count",
        "summary-dimensions",
        "summary-hash",
        "missing-hash",
        "model",
        "configured",
        "status",
        "usage",
        "response",
    ] {
        let mut corrupt = call.clone();
        let response = corrupt.response.as_mut().unwrap();
        match mutation {
            "rows" => response["embeddings"] = json!([]),
            "dimension" => response["embeddings"][0] = json!([1.0]),
            "coordinate" => response["embeddings"][0][0] = json!(null),
            "finite-coordinate" => response["embeddings"][0][0] = json!(0.5),
            "summary-count" | "summary-dimensions" | "summary-hash" | "missing-hash" => {
                let summary = &mut response["metadata"]["provider_response"];
                match mutation {
                    "summary-count" => summary["embedding_count"] = json!(3),
                    "summary-dimensions" => summary["embedding_dimensions"] = json!(4),
                    "summary-hash" => summary["embedding_sha256"] = json!("0".repeat(64)),
                    "missing-hash" => {
                        summary.as_object_mut().unwrap().remove("embedding_sha256");
                    }
                    _ => unreachable!(),
                }
                corrupt.provider_response = Some(summary.clone());
            }
            "model" => response["metadata"]["resolved_model"] = json!("unrelated/model"),
            "configured" => response["metadata"]["configured_model"] = json!("unrelated/model"),
            "status" => response["metadata"]["http_status"] = json!(429),
            "usage" => corrupt.usage.as_mut().unwrap().cost = Some(99.0),
            "response" => corrupt.provider_response = Some(json!({"different": true})),
            _ => unreachable!(),
        }
        let mut replay =
            ReplayClient::new(vec![corrupt], GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert!(replay.embed(&embedding_request()).is_err(), "{mutation}");
    }
}

#[test]
fn rejected_successful_embedding_replay_normalizes_cost_and_preserves_evidence() {
    let original = embedding_record("invalid-replayed-cost");
    for rejection in ["cost", "model", "shape"] {
        let mut call = original.clone();
        call.usage.as_mut().unwrap().cost = Some(-0.25);
        let response = call.response.as_mut().unwrap();
        response["metadata"]["usage"]["cost"] = json!(-0.25);
        match rejection {
            "model" => {
                response["metadata"]["resolved_model"] = json!("unrelated/model");
                call.resolved_model = Some("unrelated/model".into());
            }
            "shape" => response["embeddings"][0] = json!([]),
            "cost" => {}
            _ => unreachable!(),
        }
        let evidence = call.provider_response.clone().unwrap();
        let mut expected_usage = call.usage.clone().unwrap();
        expected_usage.cost = None;
        let mut replay = ReplayClient::new(vec![call], GENERATION_MODEL, DECISION_MODEL).unwrap();
        let ModelError::Provider { failure, .. } = replay.embed(&embedding_request()).unwrap_err()
        else {
            panic!("{rejection}: provider failure expected")
        };
        assert_eq!(failure.usage, Some(expected_usage), "{rejection}");
        assert_eq!(failure.response, evidence, "{rejection}");
    }
}

#[test]
fn embedding_provider_failures_record_replay_accounting_and_redact_credentials() {
    for status in [200, 429] {
        let directory = std::env::temp_dir().join(format!(
            "mls-embeddings-error-{status}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let body = json!({"model": EMBEDDING_MODEL, "data": [], "secret": "local-test-secret", "debug": "local-test-secret", "local-test-secret": "reflected key", "usage": {"prompt_tokens": 8, "cost": 0.25}});
        let (base_url, _) = serve_json(status, body);
        let live = OpenRouterClient::new(embedding_config(base_url), "local-test-secret").unwrap();
        let mut recorder = RecordingClient::new(
            live,
            &directory,
            GENERATION_MODEL,
            DECISION_MODEL,
            "synthetic",
        );
        let error = recorder.embed(&embedding_request()).unwrap_err();
        assert!(!format!("{error:?}{error}").contains("local-test-secret"));
        let serialized = std::fs::read_to_string(directory.join("call-0000.json")).unwrap();
        assert!(!serialized.contains("local-test-secret"));
        let recorded: RecordedCall = serde_json::from_str(&serialized).unwrap();
        assert_eq!(recorded.usage.as_ref().unwrap().cost, Some(0.25));
        assert_eq!(recorded.response_status, Some(status));
        let mut replay =
            ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
        let ModelError::Provider { failure, .. } = replay.embed(&embedding_request()).unwrap_err()
        else {
            panic!("provider failure expected")
        };
        assert_eq!(failure.usage.unwrap().input_tokens, Some(8));
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn embedding_recording_failure_returns_paid_result_and_stops_future_calls() {
    let path = blocked_recording_path("mls-embeddings-blocked");
    let calls = Rc::new(Cell::new(0));
    let mut recorder = RecordingClient::new(
        EmbeddingFake {
            result: Some(Ok(embedding_reply())),
            calls: Rc::clone(&calls),
        },
        &path,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.embed(&embedding_request()).unwrap(),
        embedding_reply()
    );
    assert!(recorder.persistence_failure().is_some());
    assert!(matches!(
        recorder.embed(&embedding_request()),
        Err(ModelError::RecordingStopped)
    ));
    assert_eq!(calls.get(), 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn embedding_transport_failure_records_and_replays_unknown_usage() {
    let directory =
        std::env::temp_dir().join(format!("mls-embeddings-transport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut recorder = RecordingClient::new(
        EmbeddingFake {
            result: Some(Err(ModelError::Transport("connection reset".into()))),
            calls: Rc::new(Cell::new(0)),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert!(matches!(
        recorder.embed(&embedding_request()),
        Err(ModelError::Transport(_))
    ));
    let call: RecordedCall =
        serde_json::from_slice(&std::fs::read(directory.join("call-0000.json")).unwrap()).unwrap();
    assert_eq!(call.usage, None);
    assert_eq!(call.response_status, None);
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert!(
        matches!(replay.embed(&embedding_request()), Err(ModelError::Transport(message)) if message == "connection reset")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

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
        retrieval: RetrievalConfig::default(),
        base_url,
        request_timeout_seconds: 5,
        default_max_tokens: 32,
    }
}

fn serve_once(
    status: u16,
    body: &'static str,
) -> (String, mpsc::Receiver<(String, Vec<Header>, Value)>) {
    serve_once_with_headers(status, body, Vec::new())
}

fn serve_once_with_headers(
    status: u16,
    body: &'static str,
    response_headers: Vec<Header>,
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
        let mut response = Response::from_string(body).with_status_code(StatusCode(status));
        for header in response_headers {
            response.add_header(header);
        }
        request.respond(response).expect("respond");
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
    let call: RecordedCall = serde_json::from_str(&text).unwrap();
    for mutation in [
        "tokens",
        "cost",
        "status",
        "model",
        "response",
        "error-cost",
    ] {
        let mut corrupted = call.clone();
        match mutation {
            "tokens" => corrupted.usage.as_mut().unwrap().input_tokens = Some(999),
            "cost" => corrupted.usage.as_mut().unwrap().cost = Some(99.0),
            "status" => corrupted.response_status = Some(503),
            "model" => corrupted.resolved_model = Some("unrelated/model".into()),
            "response" => corrupted.provider_response = Some(json!({"tampered": true})),
            "error-cost" => {
                let Some(mls_models::RecordedError::Provider { failure, .. }) =
                    &mut corrupted.error
                else {
                    unreachable!()
                };
                failure.usage.as_mut().unwrap().cost = Some(99.0);
            }
            _ => unreachable!(),
        }
        let mut replay =
            ReplayClient::new(vec![corrupted], GENERATION_MODEL, DECISION_MODEL).unwrap();
        assert!(
            matches!(
                replay.generate(&generation_request()),
                Err(ModelError::InvalidResponse(_))
            ),
            "{mutation}"
        );
    }
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

/// Fake client whose calls always fail at transport level.
struct TransportFake;

impl ModelClient for TransportFake {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        Err(ModelError::Transport("connection reset".into()))
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        Err(ModelError::Transport("connection reset".into()))
    }
}

/// Create a regular file at a path where a recording directory would be created,
/// so every call record write fails.
fn blocked_recording_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, b"not a directory").unwrap();
    path
}

#[test]
fn persistence_failure_keeps_successful_reply_and_counts_records() {
    let path = blocked_recording_path("mls-models-blocked-success");
    let mut recorder = RecordingClient::new(
        FakeClient {
            generation: Ok(generation_reply()),
        },
        &path,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert!(recorder.persistence_failure().is_none());
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    let failure = recorder.persistence_failure().expect("persistence failure");
    assert!(failure.contains("1 model call record"), "{failure}");
    let error = recorder.generate(&generation_request()).unwrap_err();
    assert!(matches!(error, ModelError::RecordingStopped), "{error}");
    let failure = recorder.persistence_failure().expect("persistence failure");
    assert!(failure.contains("1 model call record"), "{failure}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn persistence_failure_does_not_replace_inner_error() {
    let path = blocked_recording_path("mls-models-blocked-error");
    let mut recorder = RecordingClient::new(
        TransportFake,
        &path,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    let error = recorder.generate(&generation_request()).unwrap_err();
    assert!(
        matches!(&error, ModelError::Transport(message) if message == "connection reset"),
        "{error}"
    );
    let error = recorder.decide(&decision_request()).unwrap_err();
    assert!(matches!(error, ModelError::RecordingStopped), "{error}");
    let failure = recorder.persistence_failure().expect("persistence failure");
    assert!(failure.contains("1 model call record"), "{failure}");
    std::fs::remove_file(path).unwrap();
}

/// Fake client that counts every call it receives.
struct CountingFake {
    calls: Rc<Cell<usize>>,
}

impl ModelClient for CountingFake {
    fn generate(&mut self, _: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        self.calls.set(self.calls.get() + 1);
        Ok(generation_reply())
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        self.calls.set(self.calls.get() + 1);
        Err(ModelError::InvalidResponse("synthetic failure".into()))
    }
}

#[test]
fn persistence_failure_stops_later_live_calls() {
    let path = blocked_recording_path("mls-models-blocked-stop");
    let calls = Rc::new(Cell::new(0));
    let mut recorder = RecordingClient::new(
        CountingFake {
            calls: Rc::clone(&calls),
        },
        &path,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    // The call whose record failed still returns its real result.
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    assert_eq!(calls.get(), 1);

    let error = recorder.generate(&generation_request()).unwrap_err();
    assert!(matches!(error, ModelError::RecordingStopped), "{error}");
    let error = recorder.decide(&decision_request()).unwrap_err();
    assert!(matches!(error, ModelError::RecordingStopped), "{error}");
    assert_eq!(calls.get(), 1, "no live call after a persistence failure");
    let failure = recorder.persistence_failure().expect("persistence failure");
    assert!(failure.contains("1 model call record"), "{failure}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn healthy_recording_reports_no_persistence_failure() {
    let directory = std::env::temp_dir().join(format!("mls-models-healthy-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let mut recorder = RecordingClient::new(
        FakeClient {
            generation: Ok(generation_reply()),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    assert!(recorder.persistence_failure().is_none());
    assert!(directory.join("call-0000.json").is_file());
    assert!(partial_records(&directory).is_empty());
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(
        replay.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

/// Names of leftover `.partial` files in a recording directory.
fn partial_records(directory: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".partial"))
        .collect()
}

#[test]
fn occupied_record_name_is_skipped_without_overwrite_or_partial() {
    let directory =
        std::env::temp_dir().join(format!("mls-models-occupied-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("call-0000.json"), b"existing record").unwrap();
    let mut recorder = RecordingClient::new(
        FakeClient {
            generation: Ok(generation_reply()),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    assert!(recorder.persistence_failure().is_none());
    assert_eq!(
        std::fs::read(directory.join("call-0000.json")).unwrap(),
        b"existing record"
    );
    assert!(directory.join("call-0001.json").is_file());
    assert!(partial_records(&directory).is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_record_create_keeps_reply_leaves_no_partial_and_stops_recording() {
    let directory =
        std::env::temp_dir().join(format!("mls-models-readonly-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir_all(&directory).unwrap();
    let set_writable = |writable: bool| {
        let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
        permissions.set_readonly(!writable);
        std::fs::set_permissions(&directory, permissions).unwrap();
    };
    set_writable(false);
    // Privileged processes ignore directory permissions; the failure cannot be simulated there.
    let privileged = std::fs::write(directory.join("probe"), b"").is_ok();
    if privileged {
        let _ = std::fs::remove_file(directory.join("probe"));
        set_writable(true);
        std::fs::remove_dir_all(directory).unwrap();
        return;
    }

    let mut recorder = RecordingClient::new(
        FakeClient {
            generation: Ok(generation_reply()),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    let failure = recorder.persistence_failure().expect("persistence failure");
    assert!(failure.contains("1 model call record"), "{failure}");

    set_writable(true);
    assert!(partial_records(&directory).is_empty());
    assert!(!directory.join("call-0000.json").exists());
    // The recorder stopped after the failed write, so the next call makes no live request
    // and writes no record, even though the directory is writable again.
    let error = recorder.generate(&generation_request()).unwrap_err();
    assert!(matches!(error, ModelError::RecordingStopped), "{error}");
    assert!(!directory.join("call-0000.json").exists());
    assert!(!directory.join("call-0001.json").exists());
    assert!(partial_records(&directory).is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn replay_ignores_stray_partial_records() {
    let directory = std::env::temp_dir().join(format!("mls-models-stray-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    let mut recorder = RecordingClient::new(
        FakeClient {
            generation: Ok(generation_reply()),
        },
        &directory,
        GENERATION_MODEL,
        DECISION_MODEL,
        "synthetic",
    );
    assert_eq!(
        recorder.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    std::fs::write(
        directory.join(".call-0001.json.partial"),
        b"{\"version\": 1, \"tru",
    )
    .unwrap();
    let mut replay =
        ReplayClient::from_directory(&directory, GENERATION_MODEL, DECISION_MODEL).unwrap();
    assert_eq!(
        replay.generate(&generation_request()).unwrap(),
        generation_reply()
    );
    std::fs::remove_dir_all(directory).unwrap();
}
