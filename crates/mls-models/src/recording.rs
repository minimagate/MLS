use crate::openrouter::{
    bound_embedding_error, embedding_hash, validate_answers, validate_embedding_request,
    validate_embedding_resolved_model, validate_embedding_usage, validate_embeddings,
    validate_model,
};
use crate::{
    DecisionReply, DecisionRequest, EmbeddingClient, EmbeddingReply, EmbeddingRequest,
    GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelError, ProviderFailure,
    Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fmt::Write as _,
    path::{Path, PathBuf},
    time::Instant,
};

/// Current on-disk recording envelope version.
pub const RECORDING_VERSION: u32 = 1;

/// One sanitized, exact-match model call recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCall {
    /// Recording format version.
    pub version: u32,
    /// API surface: `generation`, `decision`, or `embedding`.
    pub endpoint_kind: String,
    /// Stable source block identifier.
    pub block_id: String,
    /// Zero-based attempt number.
    pub attempt: u32,
    /// Full canonical logical request including configured model.
    pub request: Value,
    /// SHA-256 of the canonical request bytes.
    pub request_hash: String,
    /// Model ID configured for this call.
    pub configured_model: String,
    /// Provider-returned model/version, when a valid response exposed it.
    pub resolved_model: Option<String>,
    /// Normalized usage, when a valid response exposed it.
    pub usage: Option<Usage>,
    /// End-to-end elapsed time observed by the recording wrapper.
    pub elapsed_ms: u64,
    /// HTTP status returned by the provider, including invalid responses.
    pub response_status: Option<u16>,
    /// Sanitized raw provider response retained for failed validation.
    pub provider_response: Option<Value>,
    /// Parsed success response, absent on failure.
    pub response: Option<Value>,
    /// Sanitized replayable failure, absent on success.
    pub error: Option<RecordedError>,
    /// Fixture provenance such as `synthetic` or a capture date.
    pub provenance: String,
}

/// A stable, sanitized error representation used by replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedError {
    /// Transport failed before a provider response.
    Transport {
        /// Sanitized transport summary.
        message: String,
    },
    /// Provider returned a non-success status.
    Http {
        /// Numeric provider HTTP status.
        status: u16,
        /// Redacted provider failure summary.
        message: String,
    },
    /// Provider success response was malformed or invalid.
    InvalidResponse {
        /// Sanitized validation summary.
        message: String,
    },
    /// Generated content failed local JSON Schema validation.
    Schema {
        /// JSON Schema failure summary.
        message: String,
    },
    /// Provider response failed validation while retaining audit evidence.
    Provider {
        /// Sanitized validation summary.
        message: String,
        /// Sanitized provider evidence.
        failure: ProviderFailure,
    },
    /// Other local error, replayed as an invalid response.
    Local {
        /// Sanitized local failure summary.
        message: String,
    },
}

impl RecordedError {
    fn capture(error: &ModelError) -> Self {
        match error {
            ModelError::Transport(message) => Self::Transport {
                message: message.clone(),
            },
            ModelError::Http { status, message } => Self::Http {
                status: *status,
                message: message.clone(),
            },
            ModelError::InvalidResponse(message) => Self::InvalidResponse {
                message: message.clone(),
            },
            ModelError::Schema(message) => Self::Schema {
                message: message.clone(),
            },
            ModelError::Provider { message, failure } => Self::Provider {
                message: message.clone(),
                failure: (**failure).clone(),
            },
            ModelError::Io(_)
            | ModelError::Json(_)
            | ModelError::ReplayMismatch
            | ModelError::RecordingStopped => Self::Local {
                message: error.to_string(),
            },
        }
    }

    fn replay(&self) -> ModelError {
        match self {
            Self::Transport { message } => ModelError::Transport(message.clone()),
            Self::Http { status, message } => ModelError::Http {
                status: *status,
                message: message.clone(),
            },
            Self::InvalidResponse { message } => ModelError::InvalidResponse(message.clone()),
            Self::Schema { message } => ModelError::Schema(message.clone()),
            Self::Provider { message, failure } => ModelError::Provider {
                message: message.clone(),
                failure: Box::new(failure.clone()),
            },
            Self::Local { message } => ModelError::InvalidResponse(message.clone()),
        }
    }
}

/// Wrapper that persists every attempted call without credentials or headers.
pub struct RecordingClient<C> {
    inner: C,
    directory: PathBuf,
    generation_model: String,
    decision_model: String,
    provenance: String,
    next_index: usize,
    unpersisted: usize,
    first_persistence_error: Option<String>,
}

impl<C> RecordingClient<C> {
    /// Create a recorder around another client.
    pub fn new(
        inner: C,
        directory: impl Into<PathBuf>,
        generation_model: impl Into<String>,
        decision_model: impl Into<String>,
        provenance: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            directory: directory.into(),
            generation_model: generation_model.into(),
            decision_model: decision_model.into(),
            provenance: provenance.into(),
            next_index: 0,
            unpersisted: 0,
            first_persistence_error: None,
        }
    }

    /// Describe call-record persistence failures, if any occurred.
    ///
    /// Provider results are returned to the caller even when their record cannot
    /// be written, so a run can succeed while its recording is incomplete.
    /// `Some(..)` means at least one call record is missing and the recording is
    /// not replayable. Callers must treat `Some(..)` as a failed run after saving
    /// their report. After the first failure, later calls return
    /// `ModelError::RecordingStopped` without contacting the inner client.
    pub fn persistence_failure(&self) -> Option<String> {
        self.first_persistence_error.as_ref().map(|first| {
            format!(
                "{} model call record(s) not persisted; first error: {first}",
                self.unpersisted
            )
        })
    }

    /// Whether a record failed earlier, so no further live call may be made.
    fn recording_stopped(&self) -> bool {
        self.first_persistence_error.is_some()
    }

    /// Count a failed record write, warning on stderr only for the first one.
    fn note_persist_result(&mut self, result: Result<(), ModelError>) {
        let Err(error) = result else {
            return;
        };
        self.unpersisted += 1;
        if self.first_persistence_error.is_none() {
            let message = error.to_string();
            eprintln!("MLS warning: failed to persist model call record: {message}");
            self.first_persistence_error = Some(message);
        }
    }

    /// Publish the next free `call-NNNN.json` record, all-or-nothing.
    ///
    /// Occupied names are skipped, so existing records are never overwritten.
    /// Any other failure returns without advancing `next_index`, so the next call
    /// retries the same index and the numbering gets no hole from a failed write.
    fn persist(&mut self, call: &RecordedCall) -> Result<(), ModelError> {
        std::fs::create_dir_all(&self.directory).map_err(ModelError::Io)?;
        let bytes = serde_json::to_vec_pretty(call).map_err(ModelError::Json)?;
        loop {
            let name = format!("call-{:04}.json", self.next_index);
            let partial = self.directory.join(format!(".{name}.partial"));
            let published = self.directory.join(&name);
            match publish_record(&partial, &published, &bytes) {
                Ok(()) => {
                    self.next_index += 1;
                    return Ok(());
                }
                // Name taken by a published record (or a stale partial): try the next index.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.next_index += 1;
                }
                Err(error) => return Err(ModelError::Io(error)),
            }
        }
    }
}

/// Write `bytes` to a fresh `partial` sibling, sync it, then hard-link it to
/// `published`. `hard_link` refuses an existing target, so nothing is overwritten
/// and the record appears under its final name only once it is complete.
///
/// If `partial` already exists, nothing is touched and `AlreadyExists` is returned.
/// Otherwise `partial` is removed on every path; after a successful link the
/// record survives under `published`. A leftover `.partial` is never a record.
fn publish_record(partial: &Path, published: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| std::fs::hard_link(partial, published));
    let _ = std::fs::remove_file(partial);
    result
}

impl<C: ModelClient> ModelClient for RecordingClient<C> {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        if self.recording_stopped() {
            return Err(ModelError::RecordingStopped);
        }
        let canonical = generation_request(request, &self.generation_model);
        let started = Instant::now();
        let result = self.inner.generate(request);
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let metadata = result.as_ref().ok().map(|reply| &reply.metadata);
        let call = recorded(RecordedAttempt {
            endpoint_kind: "generation",
            block_id: &request.context.block_id,
            attempt: request.context.attempt,
            request: canonical,
            configured_model: &self.generation_model,
            resolved_model: metadata.map(|value| value.resolved_model.as_str()),
            usage: metadata.map(|value| &value.usage),
            provider_response: metadata.map(|value| &value.provider_response),
            response_status: metadata.map(|value| value.http_status),
            elapsed_ms,
            result: &result,
            provenance: &self.provenance,
        });
        let persisted = self.persist(&call);
        self.note_persist_result(persisted);
        result
    }

    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        if self.recording_stopped() {
            return Err(ModelError::RecordingStopped);
        }
        let canonical = decision_request(request, &self.decision_model);
        let started = Instant::now();
        let result = self.inner.decide(request);
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let metadata = result.as_ref().ok().map(|reply| &reply.metadata);
        let call = recorded(RecordedAttempt {
            endpoint_kind: "decision",
            block_id: &request.context.block_id,
            attempt: request.context.attempt,
            request: canonical,
            configured_model: &self.decision_model,
            resolved_model: metadata.map(|value| value.resolved_model.as_str()),
            usage: metadata.map(|value| &value.usage),
            provider_response: metadata.map(|value| &value.provider_response),
            response_status: metadata.map(|value| value.http_status),
            elapsed_ms,
            result: &result,
            provenance: &self.provenance,
        });
        let persisted = self.persist(&call);
        self.note_persist_result(persisted);
        result
    }
}

impl<C: EmbeddingClient> EmbeddingClient for RecordingClient<C> {
    fn embed(&mut self, request: &EmbeddingRequest) -> Result<EmbeddingReply, ModelError> {
        if self.recording_stopped() {
            return Err(ModelError::RecordingStopped);
        }
        let canonical = embedding_request(request);
        let started = Instant::now();
        let result = self.inner.embed(request);
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let metadata = result.as_ref().ok().map(|reply| &reply.metadata);
        let call = recorded(RecordedAttempt {
            endpoint_kind: "embedding",
            block_id: &request.context.block_id,
            attempt: request.context.attempt,
            request: canonical,
            configured_model: &request.model,
            resolved_model: metadata.map(|value| value.resolved_model.as_str()),
            usage: metadata.map(|value| &value.usage),
            provider_response: metadata.map(|value| &value.provider_response),
            response_status: metadata.map(|value| value.http_status),
            elapsed_ms,
            result: &result,
            provenance: &self.provenance,
        });
        let persisted = self.persist(&call);
        self.note_persist_result(persisted);
        result
    }
}

struct RecordedAttempt<'a, T> {
    endpoint_kind: &'a str,
    block_id: &'a str,
    attempt: u32,
    request: Value,
    configured_model: &'a str,
    resolved_model: Option<&'a str>,
    usage: Option<&'a Usage>,
    provider_response: Option<&'a Value>,
    response_status: Option<u16>,
    elapsed_ms: u64,
    result: &'a Result<T, ModelError>,
    provenance: &'a str,
}

fn recorded<T: Serialize>(attempt: RecordedAttempt<'_, T>) -> RecordedCall {
    let request_hash = hash_value(&attempt.request);
    let (response, error) = match attempt.result {
        Ok(reply) => (
            Some(serde_json::to_value(reply).expect("model replies are serializable")),
            None,
        ),
        Err(error) => (None, Some(RecordedError::capture(error))),
    };
    let provider_failure = match attempt.result {
        Err(ModelError::Provider { failure, .. }) => Some(failure),
        _ => None,
    };
    RecordedCall {
        version: RECORDING_VERSION,
        endpoint_kind: attempt.endpoint_kind.into(),
        block_id: attempt.block_id.into(),
        attempt: attempt.attempt,
        request: attempt.request,
        request_hash,
        configured_model: attempt.configured_model.into(),
        resolved_model: attempt
            .resolved_model
            .map(str::to_owned)
            .or_else(|| provider_failure.and_then(|failure| failure.resolved_model.clone())),
        usage: attempt
            .usage
            .cloned()
            .or_else(|| provider_failure.and_then(|failure| failure.usage.clone())),
        elapsed_ms: attempt.elapsed_ms,
        response_status: match attempt.result {
            Ok(_) => attempt.response_status,
            Err(ModelError::Provider { failure, .. }) => Some(failure.status),
            _ => None,
        },
        provider_response: provider_failure
            .map(|failure| failure.response.clone())
            .or_else(|| attempt.provider_response.cloned()),
        response,
        error,
        provenance: attempt.provenance.into(),
    }
}

/// Offline client that consumes ordered recordings and requires exact requests.
pub struct ReplayClient {
    calls: VecDeque<RecordedCall>,
    generation_model: String,
    decision_model: String,
}

impl ReplayClient {
    /// Load every JSON recording in lexical path order.
    pub fn from_directory(
        directory: impl AsRef<Path>,
        generation_model: impl Into<String>,
        decision_model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let mut paths = std::fs::read_dir(directory)
            .map_err(ModelError::Io)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ModelError::Io)?;
        // Only `*.json` records count; a `.partial` left by an interrupted write is ignored.
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        });
        paths.sort();
        let mut calls = VecDeque::new();
        for path in paths {
            let bytes = std::fs::read(path).map_err(ModelError::Io)?;
            let call: RecordedCall = serde_json::from_slice(&bytes).map_err(ModelError::Json)?;
            if call.version != RECORDING_VERSION || call.request_hash != hash_value(&call.request) {
                return Err(ModelError::InvalidResponse(
                    "unsupported or corrupted recording".into(),
                ));
            }
            calls.push_back(call);
        }
        Ok(Self {
            calls,
            generation_model: generation_model.into(),
            decision_model: decision_model.into(),
        })
    }

    /// Construct a replay client directly from recordings.
    pub fn new(
        calls: Vec<RecordedCall>,
        generation_model: impl Into<String>,
        decision_model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let client = Self {
            calls: calls.into(),
            generation_model: generation_model.into(),
            decision_model: decision_model.into(),
        };
        client.validate_integrity()?;
        Ok(client)
    }

    fn validate_integrity(&self) -> Result<(), ModelError> {
        if self.calls.iter().any(|call| {
            call.version != RECORDING_VERSION || call.request_hash != hash_value(&call.request)
        }) {
            Err(ModelError::InvalidResponse(
                "unsupported or corrupted recording".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn take(&mut self, kind: &str, request: Value) -> Result<RecordedCall, ModelError> {
        let Some(next) = self.calls.front() else {
            return Err(ModelError::ReplayMismatch);
        };
        if next.endpoint_kind != kind
            || next.request != request
            || next.request_hash != hash_value(&request)
            || next.configured_model != request["model"].as_str().unwrap_or_default()
            || next.block_id != request["context"]["block_id"].as_str().unwrap_or_default()
            || u64::from(next.attempt) != request["context"]["attempt"].as_u64().unwrap_or(u64::MAX)
        {
            return Err(ModelError::ReplayMismatch);
        }
        self.calls.pop_front().ok_or(ModelError::ReplayMismatch)
    }
}

impl ModelClient for ReplayClient {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        let canonical = generation_request(request, &self.generation_model);
        let call = self.take("generation", canonical)?;
        let envelope = (
            call.resolved_model.clone(),
            call.usage.clone(),
            call.response_status,
            call.provider_response.clone(),
        );
        let reply: GenerationReply = replay(call)?;
        if reply.finish_reason != "stop" || !(200..300).contains(&reply.metadata.http_status) {
            return Err(ModelError::InvalidResponse(
                "recorded generation did not finish successfully".into(),
            ));
        }
        validate_model(&self.generation_model, &reply.metadata.resolved_model)
            .map_err(ModelError::InvalidResponse)?;
        if reply.metadata.configured_model != self.generation_model {
            return Err(ModelError::InvalidResponse(
                "recorded configured model mismatch".into(),
            ));
        }
        if envelope
            != (
                Some(reply.metadata.resolved_model.clone()),
                Some(reply.metadata.usage.clone()),
                Some(reply.metadata.http_status),
                Some(reply.metadata.provider_response.clone()),
            )
        {
            return Err(ModelError::InvalidResponse(
                "recording envelope metadata mismatch".into(),
            ));
        }
        let validator = jsonschema::validator_for(&request.schema)
            .map_err(|error| ModelError::Schema(format!("invalid request schema: {error}")))?;
        validator
            .validate(&reply.output)
            .map_err(|error| ModelError::Schema(error.to_string()))?;
        if matches!(
            request.stage,
            GenerationStage::LeanStatement | GenerationStage::LeanRepair
        ) && reply
            .output
            .get("lean_proof")
            .is_some_and(|value| !value.is_null())
        {
            return Err(ModelError::InvalidResponse(
                "recorded Lean statement contains proof content".into(),
            ));
        }
        Ok(reply)
    }

    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        let canonical = decision_request(request, &self.decision_model);
        let call = self.take("decision", canonical)?;
        let envelope = (
            call.resolved_model.clone(),
            call.usage.clone(),
            call.response_status,
            call.provider_response.clone(),
        );
        let reply: DecisionReply = replay(call)?;
        validate_model(&self.decision_model, &reply.metadata.resolved_model)
            .map_err(ModelError::InvalidResponse)?;
        if reply.metadata.configured_model != self.decision_model {
            return Err(ModelError::InvalidResponse(
                "recorded configured model mismatch".into(),
            ));
        }
        if !(200..300).contains(&reply.metadata.http_status)
            || envelope
                != (
                    Some(reply.metadata.resolved_model.clone()),
                    Some(reply.metadata.usage.clone()),
                    Some(reply.metadata.http_status),
                    Some(reply.metadata.provider_response.clone()),
                )
        {
            return Err(ModelError::InvalidResponse(
                "recording envelope metadata mismatch".into(),
            ));
        }
        validate_answers(&request.questions, &reply.answers)?;
        Ok(reply)
    }
}

impl EmbeddingClient for ReplayClient {
    fn embed(&mut self, request: &EmbeddingRequest) -> Result<EmbeddingReply, ModelError> {
        let call = self.take("embedding", embedding_request(request))?;
        let envelope = (
            call.resolved_model.clone(),
            call.usage.clone(),
            call.response_status,
            call.provider_response.clone(),
        );
        let reply: EmbeddingReply = replay(call).map_err(bound_embedding_error)?;
        validate_embedding_request(request)?;
        let fail = |message: String| {
            bound_embedding_error(ModelError::Provider {
                message,
                failure: Box::new(ProviderFailure {
                    status: reply.metadata.http_status,
                    response: reply.metadata.provider_response.clone(),
                    resolved_model: Some(reply.metadata.resolved_model.clone()),
                    usage: Some(reply.metadata.usage.clone()),
                }),
            })
        };
        validate_embedding_resolved_model(&reply.metadata.resolved_model).map_err(fail)?;
        if reply.metadata.configured_model != request.model
            || !(200..300).contains(&reply.metadata.http_status)
            || envelope
                != (
                    Some(reply.metadata.resolved_model.clone()),
                    Some(reply.metadata.usage.clone()),
                    Some(reply.metadata.http_status),
                    Some(reply.metadata.provider_response.clone()),
                )
        {
            return Err(fail(
                "embedding recording envelope metadata mismatch".into(),
            ));
        }
        validate_embeddings(request, &reply.embeddings).map_err(fail)?;
        validate_embedding_usage(&reply.metadata.usage).map_err(fail)?;
        let summary = &reply.metadata.provider_response;
        if summary["embedding_count"] != json!(reply.embeddings.len())
            || summary["embedding_dimensions"] != json!(request.dimensions)
            || summary["embedding_sha256"].as_str()
                != Some(embedding_hash(&reply.embeddings).as_str())
        {
            return Err(fail(
                "embedding recording vector audit summary mismatch".into(),
            ));
        }
        Ok(reply)
    }
}

fn replay<T: for<'de> Deserialize<'de>>(call: RecordedCall) -> Result<T, ModelError> {
    if let Some(error) = &call.error {
        let consistent = match error {
            RecordedError::Provider { failure, .. } => {
                call.resolved_model == failure.resolved_model
                    && call.usage == failure.usage
                    && call.response_status == Some(failure.status)
                    && call.provider_response.as_ref() == Some(&failure.response)
            }
            _ => {
                call.resolved_model.is_none()
                    && call.usage.is_none()
                    && call.response_status.is_none()
                    && call.provider_response.is_none()
            }
        };
        if !consistent {
            return Err(ModelError::InvalidResponse(
                "recording error envelope metadata mismatch".into(),
            ));
        }
    }
    match (call.response, call.error) {
        (Some(response), None) => serde_json::from_value(response).map_err(ModelError::Json),
        (None, Some(error)) => Err(error.replay()),
        _ => Err(ModelError::InvalidResponse(
            "recording must contain exactly one response or error".into(),
        )),
    }
}

fn generation_request(request: &GenerationRequest, model: &str) -> Value {
    json!({
        "endpoint": "/api/v1/chat/completions",
        "model": model,
        "context": request.context,
        "stage": request.stage,
        "messages": request.messages,
        "schema_version": request.schema_version,
        "schema": request.schema,
        "max_tokens": request.max_tokens,
        "provider": {"require_parameters": true},
        "response_format": {"type": "json_schema", "name": request.stage.schema_name(), "strict": true}
    })
}

fn decision_request(request: &DecisionRequest, model: &str) -> Value {
    json!({
        "endpoint": "/api/alpha/decisions",
        "model": model,
        "context": request.context,
        "state": request.state,
        "questions": request.questions
    })
}

fn embedding_request(request: &EmbeddingRequest) -> Value {
    json!({
        "endpoint": "/api/v1/embeddings",
        "context": request.context,
        "model": request.model,
        "input": request.input,
        "dimensions": request.dimensions,
        "input_type": request.input_type,
        "encoding_format": "float"
    })
}

fn hash_value(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("canonical requests are serializable");
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}
