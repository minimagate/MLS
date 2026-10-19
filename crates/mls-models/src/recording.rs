use crate::openrouter::{validate_answers, validate_model};
use crate::{
    DecisionReply, DecisionRequest, GenerationReply, GenerationRequest, GenerationStage,
    ModelClient, ModelError, ProviderFailure, Usage,
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
    /// API surface: `generation` or `decision`.
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
            ModelError::Io(_) | ModelError::Json(_) | ModelError::ReplayMismatch => Self::Local {
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
        }
    }

    fn persist(&mut self, call: &RecordedCall) -> Result<(), ModelError> {
        std::fs::create_dir_all(&self.directory).map_err(ModelError::Io)?;
        let bytes = serde_json::to_vec_pretty(call).map_err(ModelError::Json)?;
        loop {
            let path = self
                .directory
                .join(format!("call-{:04}.json", self.next_index));
            self.next_index += 1;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    return file.write_all(&bytes).map_err(ModelError::Io);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(ModelError::Io(error)),
            }
        }
    }
}

impl<C: ModelClient> ModelClient for RecordingClient<C> {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
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
        self.persist(&call)?;
        result
    }

    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
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
        self.persist(&call)?;
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

fn replay<T: for<'de> Deserialize<'de>>(call: RecordedCall) -> Result<T, ModelError> {
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

fn hash_value(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("canonical requests are serializable");
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}
