//! Typed OpenRouter generation and decision clients with exact record/replay.

mod openrouter;
mod recording;
mod types;

pub use openrouter::OpenRouterClient;
pub use recording::{
    RECORDING_VERSION, RecordedCall, RecordedError, RecordingClient, ReplayClient,
};
pub use types::{
    Answer, CallContext, CallMetadata, ChatMessage, ChoiceAnswer, ChoiceAssessment, DecisionReply,
    DecisionRequest, GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelConfig,
    ModelError, NoulAnswer, NoulAssessment, ProviderFailure, Question, Usage,
};
