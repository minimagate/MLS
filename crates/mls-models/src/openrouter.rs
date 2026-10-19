use crate::{
    Answer, CallMetadata, ChoiceAnswer, DecisionReply, DecisionRequest, GenerationReply,
    GenerationRequest, GenerationStage, ModelClient, ModelConfig, ModelError, NoulAnswer,
    ProviderFailure, Question, Usage,
};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

/// Blocking HTTPS OpenRouter client for the chat-completions and Decisions APIs.
pub struct OpenRouterClient {
    config: ModelConfig,
    api_key: String,
    http: Client,
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
        Ok(Self {
            config,
            api_key,
            http,
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
        let result = self
            .http
            .post(format!(
                "{}{}",
                self.config.base_url.trim_end_matches('/'),
                path
            ))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .map_err(|error| ModelError::Transport(sanitize_transport_error(&error.to_string())))?;
        let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok((result, latency))
    }
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
        ModelError::Provider {
            message: message.into(),
            failure: Box::new(ProviderFailure {
                status: self.status,
                response: self.response.clone(),
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
    let original = response
        .text()
        .map_err(|error| ModelError::Transport(sanitize_text(&error.to_string(), api_key)))?;
    let response = sanitize_response(&original, api_key);
    let raw = RawResponse { status, response };
    if !(200..300).contains(&status) {
        return Err(raw.failure(format!("provider HTTP status {status}"), None, None));
    }
    Ok(raw)
}

fn sanitize_transport_error(message: &str) -> String {
    message.chars().take(256).collect()
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
            for (key, value) in map {
                if ["authorization", "api_key", "token", "secret"]
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
                {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact_value(value, api_key);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn validate_model(configured: &str, resolved: &str) -> Result<(), String> {
    if resolved == configured
        || resolved
            .strip_prefix(configured)
            .is_some_and(|tail| tail.starts_with('-'))
    {
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
