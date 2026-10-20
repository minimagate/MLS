use crate::{
    Answer, CallMetadata, ChoiceAnswer, DecisionReply, DecisionRequest, EmbeddingClient,
    EmbeddingReply, EmbeddingRequest, GenerationReply, GenerationRequest, GenerationStage,
    ModelClient, ModelConfig, ModelError, NoulAnswer, ProviderFailure, Question, Usage,
};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Instant};

/// Blocking HTTPS OpenRouter client for generation, decisions, and embeddings.
pub struct OpenRouterClient {
    config: ModelConfig,
    api_key: String,
    http: Client,
    embedding_http: Client,
}

impl OpenRouterClient {
    /// Construct a client from explicit configuration and credential.
    pub fn new(config: ModelConfig, api_key: impl Into<String>) -> Result<Self, ModelError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ModelError::InvalidResponse("API key is empty".into()));
        }
        if config.request_timeout_seconds == 0 {
            return Err(ModelError::InvalidResponse(
                "request timeout must be positive".into(),
            ));
        }
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.request_timeout_seconds,
            ))
            .build()
            .map_err(|error| ModelError::Transport(error.to_string()))?;
        // Keep the existing generation/decision redirect policy. Embedding batches must stay
        // at the origin checked by the live gate, even when a loopback server redirects them.
        let embedding_http = Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.request_timeout_seconds,
            ))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ModelError::Transport(error.to_string()))?;
        Ok(Self {
            config,
            api_key,
            http,
            embedding_http,
        })
    }

    /// Construct a client using `OPENROUTER_API_KEY` without logging its value.
    pub fn from_env(config: ModelConfig) -> Result<Self, ModelError> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| ModelError::InvalidResponse("OPENROUTER_API_KEY is not set".into()))?;
        Self::new(config, api_key)
    }

    fn post(&self, path: &str, body: &Value) -> Result<(Response, u64), ModelError> {
        let started = Instant::now();
        let http = if path == "/api/v1/embeddings" {
            &self.embedding_http
        } else {
            &self.http
        };
        let result = http
            .post(format!(
                "{}{}",
                self.config.base_url.trim_end_matches('/'),
                path
            ))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .map_err(|error| {
                ModelError::Transport(
                    sanitize_text(&error.to_string(), &self.api_key)
                        .chars()
                        .take(256)
                        .collect(),
                )
            })?;
        let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok((result, latency))
    }
}

impl EmbeddingClient for OpenRouterClient {
    /// Remote embedding calls require explicit client construction and `MLS_LIVE=1`.
    /// Loopback origins support offline transport tests without that environment flag.
    fn embed(&mut self, request: &EmbeddingRequest) -> Result<EmbeddingReply, ModelError> {
        validate_embedding_request(request)?;
        if request.model != self.config.retrieval.embedding_model
            || request.dimensions != self.config.retrieval.embedding_dimensions
        {
            return Err(ModelError::InvalidResponse(
                "embedding request model or dimensions differ from retrieval configuration".into(),
            ));
        }
        let loopback = reqwest::Url::parse(&self.config.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"));
        if !loopback && std::env::var("MLS_LIVE").as_deref() != Ok("1") {
            return Err(ModelError::InvalidResponse(
                "remote embeddings require MLS_LIVE=1".into(),
            ));
        }
        let body = json!({
            "model": request.model,
            "input": request.input,
            "dimensions": request.dimensions,
            "input_type": request.input_type,
            "encoding_format": "float"
        });
        let (response, latency_ms) = self.post("/api/v1/embeddings", &body)?;
        let raw = read_response(response, &self.api_key).map_err(bound_embedding_error)?;
        let mut wire: EmbeddingsResponse = raw.parse().map_err(bound_embedding_error)?;
        let usage: Usage = (&wire.usage).into();
        let fail = |message: String| {
            bound_embedding_error(raw.failure(message, Some(&wire.model), Some(usage.clone())))
        };
        validate_embedding_resolved_model(&wire.model).map_err(fail)?;
        if wire.data.len() != request.input.len() {
            return Err(fail("embedding row count differs from input count".into()));
        }
        wire.data.sort_by_key(|row| row.index);
        if wire
            .data
            .iter()
            .enumerate()
            .any(|(index, row)| row.index != index)
        {
            return Err(fail(
                "embedding indexes must identify every input exactly once".into(),
            ));
        }
        let embeddings: Vec<_> = wire.data.into_iter().map(|row| row.embedding).collect();
        validate_embeddings(request, &embeddings).map_err(fail)?;
        validate_embedding_usage(&usage).map_err(fail)?;
        let vector_hash = embedding_hash(&embeddings);
        let summary = json!({
            "id": wire.id,
            "model": wire.model,
            "object": raw.response.get("object"),
            "usage": raw.response.get("usage"),
            "embedding_count": embeddings.len(),
            "embedding_dimensions": request.dimensions,
            "embedding_sha256": vector_hash,
            "response_headers": raw.response_headers,
        });
        Ok(EmbeddingReply {
            embeddings,
            metadata: CallMetadata {
                http_status: raw.status,
                configured_model: request.model.clone(),
                resolved_model: wire.model,
                request_id: wire.id,
                provider: wire.provider,
                latency_ms,
                usage,
                provider_response: summary,
            },
        })
    }
}

/// Bound rejected embedding evidence after recovering its model and accounting. Large
/// malformed vector payloads must not be copied into every failure record.
pub(crate) fn bound_embedding_error(mut error: ModelError) -> ModelError {
    if let ModelError::Provider { failure, .. } = &mut error {
        if let Some(usage) = &mut failure.usage
            && usage
                .cost
                .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
        {
            usage.cost = None;
        }
        let text = failure.response.to_string();
        if text.len() > 4096 {
            let headers = failure.response.get("_mls_response_headers").cloned();
            failure.response = json!({
                "truncated": true,
                "body": text.chars().take(1024).collect::<String>(),
                "_mls_response_headers": headers,
            });
        }
    }
    error
}

pub(crate) fn embedding_hash(embeddings: &[Vec<f64>]) -> String {
    let bytes = serde_json::to_vec(embeddings)
        .expect("validated finite embedding coordinates are serializable");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn validate_embedding_request(request: &EmbeddingRequest) -> Result<(), ModelError> {
    if request.model.trim().is_empty()
        || request.dimensions == 0
        || request.input.is_empty()
        || request.input.iter().any(|text| text.trim().is_empty())
    {
        return Err(ModelError::InvalidResponse(
            "embedding model, positive dimensions, and nonempty input texts are required".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_embeddings(
    request: &EmbeddingRequest,
    embeddings: &[Vec<f64>],
) -> Result<(), String> {
    if embeddings.len() != request.input.len() {
        return Err("embedding row count differs from input count".into());
    }
    if embeddings
        .iter()
        .any(|vector| vector.len() != request.dimensions)
    {
        return Err("embedding vector dimensions differ from the request".into());
    }
    if embeddings.iter().flatten().any(|value| !value.is_finite()) {
        return Err("embedding vector contains non-finite coordinates".into());
    }
    Ok(())
}

pub(crate) fn validate_embedding_usage(usage: &Usage) -> Result<(), String> {
    if usage
        .cost
        .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
    {
        return Err("embedding usage cost must be finite and nonnegative when reported".into());
    }
    Ok(())
}

impl ModelClient for OpenRouterClient {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        if request.max_tokens == 0 {
            return Err(ModelError::InvalidResponse(
                "max_tokens must be positive".into(),
            ));
        }
        jsonschema::validator_for(&request.schema)
            .map_err(|error| ModelError::Schema(format!("invalid request schema: {error}")))?;
        let body = json!({
            "model": self.config.generation_model,
            "messages": request.messages,
            "max_tokens": request.max_tokens,
            "provider": {"require_parameters": true},
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": request.stage.schema_name(),
                    "strict": true,
                    "schema": request.schema
                }
            }
        });
        let (response, latency_ms) = self.post("/api/v1/chat/completions", &body)?;
        let raw = read_response(response, &self.api_key)?;
        let wire: ChatResponse = raw.parse()?;
        validate_model(&self.config.generation_model, &wire.model).map_err(|message| {
            raw.failure(message, Some(&wire.model), Some((&wire.usage).into()))
        })?;
        let failure_usage: Usage = (&wire.usage).into();
        let failure_model = wire.model.clone();
        let choice = wire.choices.into_iter().next().ok_or_else(|| {
            raw.failure(
                "missing completion choice",
                Some(&failure_model),
                Some(failure_usage.clone()),
            )
        })?;
        if choice
            .message
            .refusal
            .as_deref()
            .is_some_and(|text| !text.is_empty())
        {
            return Err(raw.failure(
                "completion was refused",
                Some(&failure_model),
                Some(failure_usage.clone()),
            ));
        }
        if choice.finish_reason != "stop" {
            return Err(raw.failure(
                format!(
                    "completion did not finish normally: {}",
                    choice.finish_reason
                ),
                Some(&failure_model),
                Some(failure_usage.clone()),
            ));
        }
        let content = choice.message.content.ok_or_else(|| {
            raw.failure(
                "assistant content is null or absent",
                Some(&failure_model),
                Some(failure_usage.clone()),
            )
        })?;
        let output: Value = serde_json::from_str(&content).map_err(|_| {
            raw.failure(
                "assistant content is malformed JSON",
                Some(&failure_model),
                Some(failure_usage.clone()),
            )
        })?;
        let validator = jsonschema::validator_for(&request.schema)
            .map_err(|error| ModelError::Schema(format!("invalid request schema: {error}")))?;
        if let Err(error) = validator.validate(&output) {
            return Err(raw.failure(
                format!(
                    "structured output schema validation failed at {}",
                    error.instance_path
                ),
                Some(&failure_model),
                Some(failure_usage.clone()),
            ));
        }
        if matches!(
            request.stage,
            GenerationStage::LeanStatement | GenerationStage::LeanRepair
        ) && output
            .get("lean_proof")
            .is_some_and(|proof| !proof.is_null())
        {
            return Err(raw.failure(
                "Lean statement generation returned unexpected proof content",
                Some(&failure_model),
                Some(failure_usage.clone()),
            ));
        }
        Ok(GenerationReply {
            output,
            metadata: CallMetadata {
                http_status: raw.status,
                configured_model: self.config.generation_model.clone(),
                resolved_model: wire.model,
                request_id: wire.id,
                provider: wire.provider,
                latency_ms,
                usage: failure_usage,
                provider_response: raw.response.clone(),
            },
            finish_reason: choice.finish_reason,
        })
    }

    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        validate_questions(&request.questions)?;
        let body = json!({
            "model": self.config.decision_model,
            "state": request.state,
            "questions": request.questions
        });
        let (response, latency_ms) = self.post("/api/alpha/decisions", &body)?;
        let raw = read_response(response, &self.api_key)?;
        let wire: DecisionsResponse = raw.parse()?;
        let failure_usage: Usage = (&wire.usage).into();
        validate_model(&self.config.decision_model, &wire.model).map_err(|message| {
            raw.failure(message, Some(&wire.model), Some(failure_usage.clone()))
        })?;
        validate_answers(&request.questions, &wire.answers).map_err(|error| {
            raw.failure(
                error.to_string(),
                Some(&wire.model),
                Some(failure_usage.clone()),
            )
        })?;
        Ok(DecisionReply {
            answers: wire.answers,
            metadata: CallMetadata {
                http_status: raw.status,
                configured_model: self.config.decision_model.clone(),
                resolved_model: wire.model,
                request_id: wire.id,
                provider: wire.provider,
                latency_ms,
                usage: failure_usage,
                provider_response: raw.response.clone(),
            },
        })
    }
}

struct RawResponse {
    status: u16,
    response: Value,
    response_headers: BTreeMap<String, String>,
}

impl RawResponse {
    fn parse<T: for<'de> Deserialize<'de>>(&self) -> Result<T, ModelError> {
        serde_json::from_value(self.response.clone())
            .map_err(|_| self.failure("malformed provider response JSON", None, None))
    }

    fn failure(
        &self,
        message: impl Into<String>,
        model: Option<&str>,
        usage: Option<Usage>,
    ) -> ModelError {
        let mut evidence = self.response.clone();
        if !self.response_headers.is_empty() {
            if !evidence.is_object() {
                evidence = json!({"body": evidence});
            }
            evidence["_mls_response_headers"] = json!(self.response_headers);
        }
        ModelError::Provider {
            message: message.into(),
            failure: Box::new(ProviderFailure {
                status: self.status,
                response: evidence,
                resolved_model: model.map(str::to_owned).or_else(|| self.recovered_model()),
                usage: usage.or_else(|| self.recovered_usage()),
            }),
        }
    }

    fn recovered_model(&self) -> Option<String> {
        self.response.get("model")?.as_str().map(str::to_owned)
    }

    fn recovered_usage(&self) -> Option<Usage> {
        let usage = self.response.get("usage")?;
        Some(Usage {
            input_tokens: usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(Value::as_u64),
            output_tokens: usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(Value::as_u64),
            total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
            cost: usage.get("cost").and_then(Value::as_f64),
        })
    }
}

fn read_response(response: Response, api_key: &str) -> Result<RawResponse, ModelError> {
    let status = response.status().as_u16();
    let response_headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if matches!(
                name,
                "retry-after"
                    | "x-ratelimit-limit"
                    | "x-ratelimit-remaining"
                    | "x-ratelimit-reset"
                    | "ratelimit-limit"
                    | "ratelimit-remaining"
                    | "ratelimit-reset"
            ) {
                value.to_str().ok().map(|value| {
                    (
                        name.to_owned(),
                        sanitize_text(value, api_key).chars().take(256).collect(),
                    )
                })
            } else {
                None
            }
        })
        .collect();
    let original = response
        .text()
        .map_err(|error| ModelError::Transport(sanitize_text(&error.to_string(), api_key)))?;
    let response = sanitize_response(&original, api_key);
    let raw = RawResponse {
        status,
        response,
        response_headers,
    };
    if !(200..300).contains(&status) {
        return Err(raw.failure(format!("provider HTTP status {status}"), None, None));
    }
    Ok(raw)
}

fn sanitize_text(text: &str, api_key: &str) -> String {
    text.replace(api_key, "[REDACTED]")
        .chars()
        .take(4096)
        .collect()
}

fn sanitize_response(text: &str, api_key: &str) -> Value {
    let mut value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return json!({"body": sanitize_text(text, api_key)}),
    };
    redact_value(&mut value, api_key);
    value
}

fn redact_value(value: &mut Value, api_key: &str) {
    match value {
        Value::String(text) => *text = text.replace(api_key, "[REDACTED]"),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| redact_value(value, api_key)),
        Value::Object(map) => {
            let original = std::mem::take(map);
            for (key, mut value) in original {
                if ["authorization", "api_key", "token", "secret"]
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
                {
                    value = Value::String("[REDACTED]".into());
                } else {
                    redact_value(&mut value, api_key);
                }
                map.insert(key.replace(api_key, "[REDACTED]"), value);
            }
        }
        _ => {}
    }
}

pub(crate) fn validate_embedding_resolved_model(resolved: &str) -> Result<(), String> {
    // Embedding routers may return a backend ID such as parasail-bge-m3 for baai/bge-m3.
    // Keep that reported identity for audit; the configured request model remains exact.
    if resolved.is_empty() || resolved.trim() != resolved || resolved.len() > 256 {
        Err("embedding response model ID must be nonempty, trimmed, and at most 256 bytes".into())
    } else {
        Ok(())
    }
}

pub(crate) fn validate_model(configured: &str, resolved: &str) -> Result<(), String> {
    // OpenRouter may canonicalize vendor/model capitalization. Preserve the reported ID in
    // metadata while accepting only the complete configured ID or its hyphen-delimited version.
    let matching_prefix = resolved
        .get(..configured.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(configured));
    let valid_suffix = resolved
        .get(configured.len()..)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('-'));
    if !configured.trim().is_empty() && matching_prefix && valid_suffix {
        Ok(())
    } else {
        Err("provider returned an unrelated or empty model ID".into())
    }
}

fn validate_questions(questions: &BTreeMap<String, Question>) -> Result<(), ModelError> {
    if questions.is_empty() {
        return Err(ModelError::InvalidResponse(
            "questions must not be empty".into(),
        ));
    }
    for (id, question) in questions {
        if id.is_empty() {
            return Err(ModelError::InvalidResponse(
                "question ID must not be empty".into(),
            ));
        }
        let criteria = match question {
            Question::Noul { criteria, .. } => {
                if criteria.keys().map(String::as_str).collect::<Vec<_>>() != ["false", "true"] {
                    return Err(ModelError::InvalidResponse(
                        "Noul criteria must contain exactly false and true".into(),
                    ));
                }
                criteria
            }
            Question::Choice { criteria, .. } => criteria,
        };
        if criteria.is_empty() || criteria.values().any(String::is_empty) {
            return Err(ModelError::InvalidResponse(
                "question criteria must be non-empty".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_answers(
    questions: &BTreeMap<String, Question>,
    answers: &BTreeMap<String, Answer>,
) -> Result<(), ModelError> {
    if questions.len() != answers.len() || questions.keys().ne(answers.keys()) {
        return Err(ModelError::InvalidResponse(
            "answer IDs do not exactly match question IDs".into(),
        ));
    }
    for (id, question) in questions {
        let answer = &answers[id];
        match (question, answer) {
            (Question::Noul { .. }, Answer::Noul(answer))
                if answer.noul.is_finite() && (0.0..=1.0).contains(&answer.noul) => {}
            (Question::Choice { criteria, .. }, Answer::Choice(answer))
                if criteria.contains_key(&answer.choice) =>
            {
                if let Some(probabilities) = &answer.probabilities
                    && (probabilities.len() != criteria.len()
                        || probabilities.keys().any(|key| !criteria.contains_key(key))
                        || probabilities
                            .values()
                            .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
                        || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01)
                {
                    return Err(ModelError::InvalidResponse(
                        "choice probability distribution is malformed".into(),
                    ));
                }
                if answer
                    .confidence
                    .is_some_and(|p| !p.is_finite() || !(0.0..=1.0).contains(&p))
                {
                    return Err(ModelError::InvalidResponse(
                        "choice confidence is out of range".into(),
                    ));
                }
            }
            (Question::Noul { .. }, Answer::Noul(_)) => {
                return Err(ModelError::InvalidResponse(
                    "Noul probability is out of range".into(),
                ));
            }
            (Question::Choice { .. }, Answer::Choice(_)) => {
                return Err(ModelError::InvalidResponse(
                    "choice answer is outside allowed criteria".into(),
                ));
            }
            _ => {
                return Err(ModelError::InvalidResponse(format!(
                    "answer type does not match question {id}"
                )));
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct ChatResponse {
    id: Option<String>,
    model: String,
    provider: Option<String>,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: ChatUsage,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: AssistantMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
struct ChatUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    id: Option<String>,
    model: String,
    provider: Option<String>,
    data: Vec<EmbeddingRow>,
    #[serde(default)]
    usage: ChatUsage,
}

#[derive(Deserialize)]
struct EmbeddingRow {
    index: usize,
    embedding: Vec<f64>,
}

impl From<&ChatUsage> for Usage {
    fn from(value: &ChatUsage) -> Self {
        Self {
            input_tokens: value.prompt_tokens,
            output_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cost: value.cost,
        }
    }
}

#[derive(Deserialize)]
struct DecisionsResponse {
    id: Option<String>,
    model: String,
    provider: Option<String>,
    answers: BTreeMap<String, Answer>,
    usage: DecisionsUsage,
}

#[derive(Clone, Deserialize)]
struct DecisionsUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost: Option<f64>,
}

impl From<&DecisionsUsage> for Usage {
    fn from(value: &DecisionsUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: match (value.input_tokens, value.output_tokens) {
                (Some(input), Some(output)) => input.checked_add(output),
                _ => None,
            },
            cost: value.cost,
        }
    }
}

// Keep concrete types named in rustdoc-generated API descriptions.
const _: Option<(NoulAnswer, ChoiceAnswer)> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_model_identity_accepts_ascii_case_and_hyphen_versions() {
        for (configured, resolved) in [
            ("baai/bge-m3", "BAAI/bge-m3"),
            ("BAAI/BGE-M3", "baai/bge-m3"),
            ("baai/bge-m3", "BAAI/BGE-M3-V1"),
            (
                "anthropic/claude-sonnet-5",
                "ANTHROPIC/CLAUDE-SONNET-5-20261001",
            ),
            ("typesafe/jev-1.14", "TypeSafe/JEV-1.14-VERSION"),
        ] {
            assert!(
                validate_model(configured, resolved).is_ok(),
                "{configured} / {resolved}"
            );
        }
    }

    #[test]
    fn provider_model_identity_rejects_empty_unrelated_and_near_prefixes() {
        for (configured, resolved) in [
            ("", ""),
            ("", "BAAI/bge-m3"),
            (" ", " "),
            ("baai/bge-m3", ""),
            ("baai/bge-m3", "unrelated/model"),
            ("baai/bge-m3", "BAAI/bge-m30"),
            ("baai/bge-m3", "BAAI/bge-m3_other"),
            ("baai/bge-m3", "BAAI/bge-m3/other"),
            ("baai/bge-m3", "BAAI/bge-m3:other"),
            ("baai/bge-m3", "BAAI/bge-m3 "),
            ("baai/bge-m3", "BAAI/bgé-m3"),
            ("anthropic/claude-sonnet-5", "ANTHROPIC/CLAUDE-SONNET-55"),
            ("anthropic/claude-sonnet-5", "parasail-bge-m3"),
            ("typesafe/jev-1.14", "TYPESAFE/JEV-1.140"),
            ("typesafe/jev-1.14", "parasail-bge-m3"),
        ] {
            assert!(
                validate_model(configured, resolved).is_err(),
                "{configured:?} / {resolved:?}"
            );
        }
    }

    #[test]
    fn embedding_backend_identity_is_trimmed_and_bounded_without_a_model_prefix_requirement() {
        for resolved in [
            "parasail-bge-m3".to_owned(),
            "BAAI/bge-m3".into(),
            "x".repeat(256),
        ] {
            assert!(validate_embedding_resolved_model(&resolved).is_ok());
        }
        for resolved in [
            "".to_owned(),
            " \t\n".into(),
            " parasail-bge-m3".into(),
            "parasail-bge-m3 ".into(),
            "x".repeat(257),
        ] {
            assert!(validate_embedding_resolved_model(&resolved).is_err());
        }
    }

    #[test]
    fn rejected_nonfinite_embedding_cost_preserves_tokens_as_cost_becomes_unknown() {
        // Nonfinite floats cannot be represented in JSON, so exercise the normalized failure
        // boundary directly as well as the malformed-cost HTTP coverage in the offline suite.
        for cost in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = ModelError::Provider {
                message: "invalid embedding usage".into(),
                failure: Box::new(ProviderFailure {
                    status: 200,
                    response: json!({"usage": {"cost": cost.to_string()}}),
                    resolved_model: Some("baai/bge-m3".into()),
                    usage: Some(Usage {
                        input_tokens: Some(8),
                        output_tokens: Some(2),
                        total_tokens: Some(10),
                        cost: Some(cost),
                    }),
                }),
            };
            let ModelError::Provider { failure, .. } = bound_embedding_error(error) else {
                unreachable!()
            };
            let usage = failure.usage.unwrap();
            assert_eq!(usage.cost, None);
            assert_eq!(usage.input_tokens, Some(8));
            assert_eq!(usage.output_tokens, Some(2));
            assert_eq!(usage.total_tokens, Some(10));
            assert!(failure.response["usage"]["cost"].is_string());
        }
    }
}
