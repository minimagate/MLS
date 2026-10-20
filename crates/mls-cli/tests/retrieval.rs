//! Offline index-builder tests with scripted embeddings and isolated filesystems.

#[path = "../src/retrieval.rs"]
mod retrieval;

use mls_core::source_hash;
use mls_engine::retrieval::{EmbeddingConfig, PremiseIndex, PremiseVectors};
use mls_models::{
    CallMetadata, EmbeddingClient, EmbeddingReply, EmbeddingRequest, ModelConfig, ModelError, Usage,
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const STATE: &str = ".mls/retrieval-index-v1";

fn config() -> ModelConfig {
    ModelConfig::from_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/models.json"))
        .unwrap()
}

fn fixture(directory: &Path, count: usize, long: bool) {
    let jsonl = (0..count)
        .map(|n| {
            format!(
                "{}\n",
                json!({
                    "name":format!("Claim{n:05}"),"kind":"theorem","module":"Mathlib.Test",
                    "instance":false,"deprecated":false,
                    "type":if long {"數".repeat(5000)} else {"True".into()},
                    "doc":if long {Some("數".repeat(5000))} else {None}
                })
            )
        })
        .collect::<String>();
    let manifest = json!({"schema_version":"mls-premises-v1","mathlib_commit":"a".repeat(40),
        "toolchain":"leanprover/lean4:v4.19.0","extractor_sha256":"b".repeat(64),
        "filter_version":1,"row_count":count,"sha256":source_hash(jsonl.as_bytes())});
    fs::write(directory.join("manifest.json"), manifest.to_string()).unwrap();
    fs::write(directory.join("premises.jsonl"), jsonl).unwrap();
}

#[derive(Default)]
struct Mock {
    requests: Vec<EmbeddingRequest>,
    fail_at: Option<usize>,
    zeros: bool,
    provider_failure: Option<(usize, f64)>,
}

impl EmbeddingClient for Mock {
    fn embed(&mut self, request: &EmbeddingRequest) -> Result<EmbeddingReply, ModelError> {
        self.requests.push(request.clone());
        if self.fail_at == Some(self.requests.len()) {
            return Err(ModelError::Transport("ambiguous scripted timeout".into()));
        }
        if let Some((at, cost)) = self.provider_failure
            && at == self.requests.len()
        {
            return Err(ModelError::Provider {
                message: "scripted provider accounting failure".into(),
                failure: Box::new(mls_models::ProviderFailure {
                    status: 502,
                    response: json!({"error":"scripted failure"}),
                    resolved_model: Some(request.model.clone()),
                    usage: Some(Usage {
                        input_tokens: Some(7),
                        output_tokens: None,
                        total_tokens: Some(7),
                        cost: Some(cost),
                    }),
                }),
            });
        }
        let embeddings: Vec<_> = request
            .input
            .iter()
            .map(|_| {
                let mut vector = vec![0.0; request.dimensions];
                if !self.zeros {
                    vector[0] = 1.0;
                }
                vector
            })
            .collect();
        let usage = Usage {
            input_tokens: Some(3),
            output_tokens: None,
            total_tokens: Some(3),
            cost: None,
        };
        let vector_hash = source_hash(&serde_json::to_vec(&embeddings).unwrap());
        let raw = json!({"model":request.model,"id":"fake-id","provider":"scripted","object":"list",
            "usage":{"prompt_tokens":3,"total_tokens":3},"embedding_count":embeddings.len(),"embedding_dimensions":request.dimensions,"embedding_sha256":vector_hash,
            "data":embeddings.iter().enumerate().map(|(index,values)|json!({"index":index,"object":"embedding","embedding":values})).collect::<Vec<_>>()});
        Ok(EmbeddingReply {
            embeddings,
            metadata: CallMetadata {
                http_status: 200,
                configured_model: request.model.clone(),
                resolved_model: request.model.clone(),
                request_id: Some("fake-id".into()),
                provider: Some("scripted".into()),
                latency_ms: 1,
                usage,
                provider_response: raw,
            },
        })
    }
}

fn report(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn build_resume_and_missing_shard_recovery_make_no_duplicate_calls() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 4, false);
    let mut client = Mock::default();
    let result =
        report(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap());
    assert_eq!(
        client
            .requests
            .iter()
            .map(|r| r.input.len())
            .collect::<Vec<_>>(),
        [1, 2, 1]
    );
    assert_eq!(result["provider_usage"]["calls"], 3);
    assert_eq!(result["provider_usage"]["unknown_cost_calls"], 3);
    assert_eq!(result["provider_usage"]["unknown_output_token_calls"], 3);
    assert_eq!(result["complete"], true);
    let state = temp.path().join(STATE);
    let bytes = fs::read(state.join("vectors.f32")).unwrap();
    let idx = PremiseIndex::parse(
        &fs::read(temp.path().join("manifest.json")).unwrap(),
        &fs::read(temp.path().join("premises.jsonl")).unwrap(),
    )
    .unwrap();
    PremiseVectors::from_bytes(
        &idx,
        EmbeddingConfig {
            model: config().retrieval.embedding_model,
            dimensions: 1024,
        },
        &bytes,
    )
    .unwrap();
    let again =
        report(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap());
    assert_eq!(client.requests.len(), 3);
    assert_eq!(again["new_calls"], 0);
    fs::remove_file(state.join("shards/batch-00000001.f32")).unwrap();
    fs::remove_file(state.join("vectors.f32")).unwrap();
    let recovered =
        report(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap());
    assert_eq!(client.requests.len(), 3);
    assert_eq!(recovered["recovered_batches"], 1);
    assert_eq!(fs::read(state.join("vectors.f32")).unwrap(), bytes);
}

#[test]
fn replay_imports_one_batch_at_a_time_and_reports_no_new_calls() {
    let source = tempfile::tempdir().unwrap();
    fixture(source.path(), 3, false);
    retrieval::build_with_client(source.path(), &config(), 2, &mut Mock::default()).unwrap();
    let target = tempfile::tempdir().unwrap();
    fixture(target.path(), 3, false);
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .arg("retrieval-index")
        .arg(target.path())
        .arg("--replay")
        .arg(source.path().join(STATE).join("recordings"))
        .args(["--batch-size", "2"])
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["new_calls"], 0);
    assert_eq!(result["provider_usage"]["calls"], 2);
    assert_eq!(
        fs::read(source.path().join(STATE).join("vectors.f32")).unwrap(),
        fs::read(target.path().join(STATE).join("vectors.f32")).unwrap()
    );
}

#[test]
fn recorded_timeout_is_terminal_and_never_automatically_retried() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 3, false);
    let mut client = Mock {
        fail_at: Some(2),
        ..Mock::default()
    };
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
    let ledger: Value =
        serde_json::from_slice(&fs::read(temp.path().join(STATE).join("ledger.json")).unwrap())
            .unwrap();
    assert!(ledger["batches"]["batch-00000001"]["usage"].is_null());
    assert!(
        ledger["batches"]["batch-00000001"]["error"]
            .as_str()
            .unwrap()
            .contains("timeout")
    );
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
}

#[test]
fn rejects_incompatible_identity_corruption_and_nonregular_paths_before_calls() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 2, false);
    let mut client = Mock::default();
    retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap();
    let mut changed = config();
    changed.retrieval.embedding_model = "different/model".into();
    assert!(retrieval::build_with_client(temp.path(), &changed, 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
    let shard = temp.path().join(STATE).join("shards/batch-00000000.f32");
    let mut bytes = fs::read(&shard).unwrap();
    bytes[0] ^= 1;
    fs::write(&shard, &bytes).unwrap();
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
    let other = tempfile::tempdir().unwrap();
    fixture(other.path(), 2, false);
    fs::create_dir(other.path().join(".mls")).unwrap();
    fs::create_dir(other.path().join(STATE)).unwrap();
    fs::create_dir(other.path().join(STATE).join("shards")).unwrap();
    fs::create_dir(other.path().join(STATE).join("shards/batch-00000001.f32")).unwrap();
    assert!(retrieval::build_with_client(other.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
}

#[test]
fn serialized_unicode_input_budget_splits_deterministically_after_canary() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 80, true);
    let mut cfg = config();
    cfg.retrieval.embedding_dimensions = 2;
    let mut client = Mock::default();
    retrieval::build_with_client(temp.path(), &cfg, 128, &mut client).unwrap();
    assert_eq!(client.requests[0].input.len(), 1);
    assert!(client.requests.len() > 2);
    assert!(
        client
            .requests
            .iter()
            .all(|r| serde_json::to_vec(&r.input).unwrap().len() <= 256 * 1024)
    );
    assert_eq!(
        client.requests.iter().map(|r| r.input.len()).sum::<usize>(),
        80
    );
    assert!(
        client
            .requests
            .iter()
            .all(|r| r.input_type == mls_models::EmbeddingInputType::SearchDocument)
    );
}

#[test]
fn cli_requires_exactly_one_transport_and_explicit_live_gates() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 1, false);
    for args in [
        vec![],
        vec!["--live", "--replay", "somewhere"],
        vec!["--live"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_mls"))
            .arg("retrieval-index")
            .arg(temp.path())
            .args(args)
            .env_remove("MLS_LIVE")
            .env_remove("OPENROUTER_API_KEY")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .arg("retrieval-index")
        .arg(temp.path())
        .arg("--live")
        .env("MLS_LIVE", "1")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!temp.path().join(STATE).exists());
    assert!(retrieval::run(&[]).is_err());
}

#[test]
fn zero_vectors_remain_recorded_but_never_publish_a_cache() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 1, false);
    let mut client = Mock {
        zeros: true,
        ..Mock::default()
    };
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert!(!temp.path().join(STATE).join("vectors.f32").exists());
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 1);
}

#[test]
fn interruption_markers_and_ledger_corruption_block_all_further_calls() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 3, false);
    let mut client = Mock::default();
    retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap();
    let root = temp.path().join(STATE);
    let ledger_bytes = fs::read(root.join("ledger.json")).unwrap();
    let mut ledger: Value = serde_json::from_slice(&ledger_bytes).unwrap();
    ledger["batches"]["batch-00000000"]["usage"]["input_tokens"] = json!(999);
    fs::write(root.join("ledger.json"), ledger.to_string()).unwrap();
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(client.requests.len(), 2);
    fs::write(root.join("ledger.json"), ledger_bytes).unwrap();
    // Simulate a crash after recording the last batch but before its shard/ledger checkpoint.
    fs::remove_file(root.join("ledger.json")).unwrap();
    fs::remove_file(root.join("shards/batch-00000001.f32")).unwrap();
    fs::remove_file(root.join("vectors.f32")).unwrap();
    let recovered =
        report(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).unwrap());
    assert_eq!(recovered["provider_usage"]["calls"], 2);
    assert_eq!(client.requests.len(), 2);
    // A durable marker with no outcome must prevent resubmission, even if another batch is a gap.
    fs::remove_file(root.join("ledger.json")).unwrap();
    fs::remove_file(root.join("vectors.f32")).unwrap();
    fs::remove_file(root.join("shards/batch-00000000.f32")).unwrap();
    fs::remove_file(root.join("shards/batch-00000001.f32")).unwrap();
    fs::remove_file(root.join("recordings/batch-00000000/attempt-0000/call-0000.json")).unwrap();
    fs::remove_file(root.join("recordings/batch-00000000/attempt-0000/pending")).unwrap();
    fs::remove_file(root.join("recordings/batch-00000001/attempt-0000/call-0000.json")).unwrap();
    let error = retrieval::build_with_client(temp.path(), &config(), 2, &mut client)
        .err()
        .unwrap();
    assert!(error.to_string().contains("no automatic network retry"));
    assert_eq!(client.requests.len(), 2);
}

#[cfg(unix)]
#[test]
fn symlink_outputs_and_existing_build_locks_are_refused() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 1, false);
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), temp.path().join(".mls")).unwrap();
    let mut client = Mock::default();
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert!(client.requests.is_empty());
    fs::remove_file(temp.path().join(".mls")).unwrap();
    fs::create_dir(temp.path().join(".mls")).unwrap();
    fs::create_dir(temp.path().join(STATE)).unwrap();
    fs::write(temp.path().join(STATE).join("build.lock"), "pid=unknown").unwrap();
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert!(client.requests.is_empty());
    assert!(temp.path().join(STATE).join("build.lock").exists());
}

#[test]
fn review_probe_orphan_failed_record_should_block_an_earlier_gap_before_calls() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 3, false);
    let mut client = Mock {
        fail_at: Some(2),
        ..Mock::default()
    };
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    let root = temp.path().join(STATE);
    fs::remove_file(root.join("ledger.json")).unwrap();
    fs::remove_file(root.join("recordings/batch-00000000/attempt-0000/call-0000.json")).unwrap();
    fs::remove_file(root.join("recordings/batch-00000000/attempt-0000/pending")).unwrap();
    fs::remove_file(root.join("shards/batch-00000000.f32")).unwrap();
    let before = client.requests.len();
    assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
    assert_eq!(
        client.requests.len(),
        before,
        "a later durable failed attempt should block all further calls before an earlier gap"
    );
}

#[test]
fn review_probe_replay_invalid_cost_should_not_reenter_build_ledger() {
    let original = tempfile::tempdir().unwrap();
    fixture(original.path(), 1, false);
    retrieval::build_with_client(original.path(), &config(), 2, &mut Mock::default()).unwrap();
    let record = original
        .path()
        .join(STATE)
        .join("recordings/batch-00000000/attempt-0000/call-0000.json");
    let mut call: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    call["usage"]["cost"] = json!(-0.25);
    call["response"]["metadata"]["usage"]["cost"] = json!(-0.25);
    fs::write(&record, serde_json::to_vec(&call).unwrap()).unwrap();
    let target = tempfile::tempdir().unwrap();
    fixture(target.path(), 1, false);
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .arg("retrieval-index")
        .arg(target.path())
        .arg("--replay")
        .arg(original.path().join(STATE).join("recordings"))
        .args(["--batch-size", "2"])
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let ledger: Value =
        serde_json::from_slice(&fs::read(target.path().join(STATE).join("ledger.json")).unwrap())
            .unwrap();
    assert!(
        ledger["batches"]["batch-00000000"]["usage"]["cost"].is_null(),
        "normalized replay failure cost must remain unknown in ledger"
    );
    assert_eq!(
        ledger["batches"]["batch-00000000"]["usage"]["input_tokens"],
        3
    );
}

#[test]
fn nonfinite_provider_failure_cost_is_unknown_without_losing_completed_tokens() {
    for cost in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let temp = tempfile::tempdir().unwrap();
        fixture(temp.path(), 3, false);
        let mut client = Mock {
            provider_failure: Some((2, cost)),
            ..Mock::default()
        };
        assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
        assert_eq!(client.requests.len(), 2);
        assert!(retrieval::build_with_client(temp.path(), &config(), 2, &mut client).is_err());
        assert_eq!(client.requests.len(), 2);
        let ledger: Value =
            serde_json::from_slice(&fs::read(temp.path().join(STATE).join("ledger.json")).unwrap())
                .unwrap();
        assert_eq!(
            ledger["batches"]["batch-00000000"]["usage"]["input_tokens"],
            3
        );
        assert_eq!(
            ledger["batches"]["batch-00000001"]["usage"]["input_tokens"],
            7
        );
        assert!(ledger["batches"]["batch-00000001"]["usage"]["cost"].is_null());
    }
}

#[test]
fn terminal_preflight_keeps_usage_for_completed_recordings_later_in_the_plan() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), 4, false);
    retrieval::build_with_client(temp.path(), &config(), 1, &mut Mock::default()).unwrap();
    let root = temp.path().join(STATE);
    let record = root.join("recordings/batch-00000001/attempt-0000/call-0000.json");
    let mut call: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    call["response"] = Value::Null;
    call["error"] = json!({"kind":"transport","message":"recorded terminal timeout"});
    for field in [
        "usage",
        "resolved_model",
        "response_status",
        "provider_response",
    ] {
        call[field] = Value::Null;
    }
    fs::write(record, serde_json::to_vec(&call).unwrap()).unwrap();
    fs::remove_file(root.join("ledger.json")).unwrap();
    fs::remove_file(root.join("vectors.f32")).unwrap();
    for number in 0..4 {
        fs::remove_file(root.join(format!("shards/batch-{number:08}.f32"))).unwrap();
    }
    let mut client = Mock::default();
    assert!(retrieval::build_with_client(temp.path(), &config(), 1, &mut client).is_err());
    assert!(client.requests.is_empty());
    let ledger: Value =
        serde_json::from_slice(&fs::read(root.join("ledger.json")).unwrap()).unwrap();
    assert_eq!(ledger["batches"].as_object().unwrap().len(), 4);
    for number in [0, 2, 3] {
        assert_eq!(
            ledger["batches"][format!("batch-{number:08}")]["usage"]["input_tokens"],
            3
        );
    }
    assert!(
        ledger["batches"]["batch-00000001"]["error"]
            .as_str()
            .unwrap()
            .contains("timeout")
    );
}
