//! Typed OpenRouter generation, decision, and embedding clients with exact record/replay.

mod openrouter;
mod recording;
mod types;

pub use openrouter::OpenRouterClient;
pub use recording::{
    RECORDING_VERSION, RecordedCall, RecordedError, RecordingClient, ReplayClient,
};
pub use types::{
    Answer, CallContext, CallMetadata, ChatMessage, ChoiceAnswer, ChoiceAssessment, DecisionReply,
    DecisionRequest, EmbeddingClient, EmbeddingInputType, EmbeddingReply, EmbeddingRequest,
    GenerationReply, GenerationRequest, GenerationStage, ModelClient, ModelConfig, ModelError,
    NoulAnswer, NoulAssessment, ProviderFailure, Question, RetrievalConfig, Usage,
};
