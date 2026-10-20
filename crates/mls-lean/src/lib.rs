//! Persistent Lean checking, trusted extraction, shadow generation, and diagnostics.

mod diagnostic;
mod protocol;
mod shadow;
mod worker;

pub use diagnostic::{MappedDiagnostic, Position, ReplDiagnostic, Severity, map_diagnostics};
pub use protocol::{ReplConfig, ReplError};
pub use shadow::{
    DeclarationSpan, ShadowDeclaration, ShadowError, ShadowFile, ShadowSection, generate_shadow,
    generate_shadow_project, shadow_path_for_document,
};
pub use worker::{
    Binder, CandidateEnvironment, CheckResult, EnvSnapshot, ExtractedDeclaration, LeanWorker,
    StatementRequest, Validation, WorkerError,
};
