use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// Runtime model and transport configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Exact OpenRouter generation model request ID.
    pub generation_model: String,
    /// Exact pinned OpenRouter decisions model request ID.
    pub decision_model: String,
    /// Provider origin, configurable for local transport tests.
    pub base_url: String,
    /// Per-request wall-clock timeout.
    pub request_timeout_seconds: u64,
    /// Default bounded completion token limit.
    pub default_max_tokens: u32,
}

impl ModelConfig {
    /// Load and strictly parse a checked-in JSON configuration.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ModelError> {
        let bytes = std::fs::read(path).map_err(ModelError::Io)?;
        serde_json::from_slice(&bytes).map_err(ModelError::Json)
    }
}

/// Context used to identify and order a model attempt in logs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallContext {
    /// Stable source block identifier.
    pub block_id: String,
    /// Zero-based attempt index.
    pub attempt: u32,
}

/// A chat message sent to a generation model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    /// OpenRouter chat role.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// The generation pipeline stage, used for schema naming and audit logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStage {
    /// Generate a statement intermediate representation.
    Sir,
    /// Generate a Lean statement without a proof.
    LeanStatement,
    /// Repair a failed Lean statement while retaining the accepted SIR.
    LeanRepair,
}

impl GenerationStage {
    pub(crate) const fn schema_name(self) -> &'static str {
        match self {
            Self::Sir => "mls_statement_sir",
            Self::LeanStatement | Self::LeanRepair => "mls_lean_statement",
        }
    }
}

/// One strict structured-output generation request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRequest {
    /// Audit and replay identity.
    pub context: CallContext,
    /// Pipeline stage.
    pub stage: GenerationStage,
    /// Ordered chat messages.
    pub messages: Vec<ChatMessage>,
    /// Version assigned by MLS to the supplied JSON schema.
    pub schema_version: String,
    /// Complete JSON Schema used by provider and local validation.
    pub schema: Value,
    /// Bounded output limit.
    pub max_tokens: u32,
}

/// One typed Jev question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Boolean probability question.
    Noul {
        /// Self-contained question instructions.
        instructions: String,
        /// Descriptions for true and false.
        criteria: BTreeMap<String, String>,
    },
    /// Bounded single-selection question.
    Choice {
        /// Self-contained question instructions.
        instructions: String,
        /// Allowed option to semantic description mapping.
        criteria: BTreeMap<String, String>,
    },
}

/// One typed Jev decisions request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    /// Audit and replay identity.
    pub context: CallContext,
    /// Minimal bounded state: string, object, or array.
    pub state: Value,
    /// Questions keyed by caller-owned stable IDs.
    pub questions: BTreeMap<String, Question>,
}

/// Normalized token and cost accounting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Provider-reported input tokens, if known.
    pub input_tokens: Option<u64>,
    /// Provider-reported output tokens, if known.
    pub output_tokens: Option<u64>,
    /// Provider-reported total tokens, if known.
    pub total_tokens: Option<u64>,
    /// Provider-reported cost, if known. Missing is never changed to zero.
    pub cost: Option<f64>,
}

/// Sanitized evidence retained when a provider response cannot be accepted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFailure {
    /// HTTP status returned by the provider.
    pub status: u16,
    /// Redacted and size-bounded response snapshot.
    pub response: Value,
    /// Returned model/version when recoverable.
    pub resolved_model: Option<String>,
    /// Returned usage and cost when recoverable.
    pub usage: Option<Usage>,
}

/// Metadata retained for every successful provider call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallMetadata {
    /// Provider HTTP status for the accepted response.
    pub http_status: u16,
    /// Configured request model ID.
    pub configured_model: String,
    /// Provider-returned resolved model or canonical version.
    pub resolved_model: String,
    /// Optional provider request identifier.
    pub request_id: Option<String>,
    /// Optional serving provider name.
    pub provider: Option<String>,
    /// Measured transport latency.
    pub latency_ms: u64,
    /// Normalized usage and optional cost.
    pub usage: Usage,
    /// Sanitized raw provider response for audit and provenance.
    pub provider_response: Value,
}

/// Validated generation result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationReply {
    /// Locally schema-validated structured output.
    pub output: Value,
    /// Provider call metadata.
    pub metadata: CallMetadata,
    /// Completion finish reason.
    pub finish_reason: String,
}

/// Boolean probability answer from Jev.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulAnswer {
    /// Probability of true/yes.
    pub noul: f64,
}

/// Bounded single-selection answer from Jev.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceAnswer {
    /// Selected option label.
    pub choice: String,
    /// Full option probability distribution, if returned.
    pub probabilities: Option<BTreeMap<String, f64>>,
    /// Distribution-concentration confidence, distinct from selected probability.
    pub confidence: Option<f64>,
}

/// A Jev answer whose type must match the corresponding question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// Noul Boolean probability.
    Noul(NoulAnswer),
    /// Choice selection and optional confidence data.
    Choice(ChoiceAnswer),
}

/// Validated decisions result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReply {
    /// Answers keyed exactly like the request questions.
    pub answers: BTreeMap<String, Answer>,
    /// Provider call metadata.
    pub metadata: CallMetadata,
}

/// Conservative Boolean interpretation of a Noul probability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoulAssessment {
    /// Probability met the yes threshold.
    Yes,
    /// Probability met the no threshold.
    No,
    /// Neither threshold was met, or the answer was malformed.
    Uncertain,
}

impl NoulAnswer {
    /// Interpret an answer with symmetric low/high thresholds.
    #[must_use]
    pub fn assess(&self, threshold: f64) -> NoulAssessment {
        if !valid_probability(self.noul) || !valid_probability(threshold) || threshold < 0.5 {
            NoulAssessment::Uncertain
        } else if self.noul >= threshold {
            NoulAssessment::Yes
        } else if self.noul <= 1.0 - threshold + f64::EPSILON {
            NoulAssessment::No
        } else {
            NoulAssessment::Uncertain
        }
    }
}

/// Conservative interpretation of a bounded choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChoiceAssessment {
    /// Both confidence and selected probability met the threshold.
    Selected(String),
    /// Evidence was absent, malformed, incomplete, or below threshold.
    Uncertain,
}

impl ChoiceAnswer {
    /// Require a complete valid distribution plus confidence and selected probability.
    #[must_use]
    pub fn assess(&self, allowed: &BTreeMap<String, String>, threshold: f64) -> ChoiceAssessment {
        let Some(probabilities) = &self.probabilities else {
            return ChoiceAssessment::Uncertain;
        };
        let Some(confidence) = self.confidence else {
            return ChoiceAssessment::Uncertain;
        };
        if !valid_probability(confidence)
            || !valid_probability(threshold)
            || probabilities.len() != allowed.len()
            || probabilities.keys().any(|key| !allowed.contains_key(key))
            || probabilities
                .values()
                .any(|value| !valid_probability(*value))
            || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
        {
            return ChoiceAssessment::Uncertain;
        }
        let Some(selected_probability) = probabilities.get(&self.choice) else {
            return ChoiceAssessment::Uncertain;
        };
        if confidence >= threshold && *selected_probability >= threshold {
            ChoiceAssessment::Selected(self.choice.clone())
        } else {
            ChoiceAssessment::Uncertain
        }
    }
}

fn valid_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// Provider-independent model boundary used by live, recording, and replay clients.
pub trait ModelClient {
    /// Generate and locally validate strict structured output.
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError>;
    /// Obtain and validate typed Jev decisions.
    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError>;
}

/// A sanitized model or replay failure.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// Configuration or recording I/O failed.
    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),
    /// JSON parsing failed without retaining a possibly sensitive response body.
    #[error("invalid JSON: {0}")]
    Json(#[source] serde_json::Error),
    /// Network transport failed.
    #[error("transport error: {0}")]
    Transport(String),
    /// Provider returned a non-success HTTP status.
    #[error("provider HTTP status {status}: {message}")]
    Http {
        /// Numeric HTTP status.
        status: u16,
        /// Redacted provider error summary.
        message: String,
    },
    /// A provider response was received but failed envelope or output validation.
    #[error("invalid provider response: {message}")]
    Provider {
        /// Sanitized summary safe for display.
        message: String,
        /// Non-displayed audit evidence.
        failure: Box<ProviderFailure>,
    },
    /// Provider response violated the expected envelope.
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),
    /// Structured generation output failed the supplied schema.
    #[error("structured output schema validation failed: {0}")]
    Schema(String),
    /// Replay contained no exact full-request match.
    #[error("no exact replay match for request")]
    ReplayMismatch,
}
