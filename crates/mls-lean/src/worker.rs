//! Transactional statement checking on branches of accepted Lean environments.

use crate::{ReplConfig, ReplDiagnostic, ReplError, protocol::ReplProcess};
use mls_core::BlockId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

const EXTRACT_PREFIX: &str = "MLS_EXTRACT:";
const VALIDATE_PREFIX: &str = "MLS_VALIDATE:";
const VALIDATE_PROOF_PREFIX: &str = "MLS_VALIDATE_PROOF:";
static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

/// One generated declaration submitted for checking.
#[derive(Clone, Debug)]
pub struct StatementRequest<'a> {
    /// Stable block identity used in generated comments and diagnostics.
    pub block_id: BlockId,
    /// Exact expected Lean declaration name.
    pub declaration_name: &'a str,
    /// Exactly one theorem or definition declaration.
    pub source: &'a str,
}

/// Trusted parse-only description of admitted source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Validation {
    /// Parsed declaration name.
    pub declaration: String,
    /// `theorem` or `definition`.
    pub kind: String,
}

/// One elaborated binder from the trusted extractor.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Binder {
    /// User-facing binder name.
    pub name: String,
    /// Pretty-printed type, for diagnostics only.
    #[serde(rename = "type")]
    pub type_text: String,
    /// Whether the binder type is proposition-valued.
    pub is_proposition: bool,
    /// Lean binder class.
    pub binder_info: String,
}

/// Trusted structural data extracted from the compiled declaration.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ExtractedDeclaration {
    /// Compiled constant name.
    pub declaration: String,
    /// Compiled declaration kind.
    pub kind: String,
    /// Header binders only.
    pub binders: Vec<Binder>,
    /// Pretty-printed conclusion/result type.
    pub goal: String,
    /// Conservative outer syntax class of the goal.
    pub goal_outer_head: String,
    /// Transitive axioms used by the declaration.
    pub axioms: Vec<String>,
    /// Whether transitive trust includes `sorryAx`.
    pub has_sorry: bool,
    /// Definition body, when checking a definition.
    #[serde(default)]
    pub body: Option<String>,
    /// Conservative body head, when checking a definition.
    #[serde(default)]
    pub body_outer_head: Option<String>,
}

/// A successfully checked but still uncommitted statement result.
#[derive(Debug)]
pub struct CheckResult {
    /// Opaque environment candidate. Accept it only after caller-side structural checks pass.
    pub candidate: CandidateEnvironment,
    /// Trusted source validation result.
    pub validation: Validation,
    /// Trusted compiled structure.
    pub extracted: ExtractedDeclaration,
    /// Non-error Lean messages, including the extractor info message.
    pub messages: Vec<ReplDiagnostic>,
}

/// An uncommitted environment produced from a specific accepted snapshot.
#[derive(Debug)]
pub struct CandidateEnvironment {
    environment: u64,
    parent_environment: u64,
    generation: u64,
    worker_id: u64,
    process_epoch: u64,
}

/// An opaque reference to an accepted environment of one worker's REPL process.
///
/// Snapshots can only be created by [`LeanWorker::snapshot`] and are valid only for the
/// worker that created them and the REPL process that was live at that time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvSnapshot {
    environment: u64,
    worker_id: u64,
    process_epoch: u64,
}

/// A rejected check; the worker's committed environment remains unchanged.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// Transport or outer REPL protocol failure.
    #[error(transparent)]
    Repl(#[from] ReplError),
    /// A previous transport failure made the REPL unusable; call [`LeanWorker::restart`].
    #[error("Lean REPL is unavailable after a transport failure; restart the worker")]
    NeedsRestart,
    /// Lean emitted one or more error diagnostics.
    #[error("Lean rejected the request")]
    Diagnostics(Vec<ReplDiagnostic>),
    /// A trusted payload was absent, duplicated, malformed, or inconsistent.
    #[error("invalid trusted {stage} payload: {message}")]
    TrustedPayload {
        /// Validation, extraction, or response decoding stage.
        stage: &'static str,
        /// Specific invariant violation.
        message: String,
    },
    /// The REPL omitted its environment snapshot.
    #[error("Lean REPL response omitted numeric env")]
    MissingEnvironment,
    /// The candidate was not produced from the worker's current accepted state.
    #[error("candidate environment is stale or belongs to another accepted snapshot")]
    StaleCandidate,
    /// The snapshot was taken from a different worker.
    #[error("snapshot belongs to another Lean worker")]
    ForeignSnapshot,
    /// The snapshot was taken before a REPL restart and its environment id is no longer meaningful.
    #[error("snapshot was invalidated by a Lean REPL restart")]
    StaleSnapshot,
    /// The requested declaration name is outside MLS's generated identifier subset.
    #[error("declaration name must match mls_[a-z0-9_]+")]
    InvalidDeclarationName,
}

/// A persistent initialized Lean worker with transactional snapshots.
pub struct LeanWorker {
    repl: Option<ReplProcess>,
    config: ReplConfig,
    base_environment: u64,
    accepted_environment: u64,
    generation: u64,
    worker_id: u64,
    process_epoch: u64,
}

impl LeanWorker {
    /// Spawn the REPL and import the fixed trusted baseline exactly once.
    pub fn spawn(config: ReplConfig) -> Result<Self, WorkerError> {
        let (repl, environment) = start_repl(config.clone())?;
        Ok(Self {
            repl: Some(repl),
            config,
            base_environment: environment,
            accepted_environment: environment,
            generation: 0,
            worker_id: NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed),
            process_epoch: 0,
        })
    }

    /// Return the last fully accepted environment id, even if its process has failed.
    #[must_use]
    pub const fn accepted_environment(&self) -> u64 {
        self.accepted_environment
    }

    /// Capture the currently accepted environment for a later [`LeanWorker::restore`].
    /// A failed worker cannot restore this snapshot; restarting invalidates it.
    #[must_use]
    pub const fn snapshot(&self) -> EnvSnapshot {
        EnvSnapshot {
            environment: self.accepted_environment,
            worker_id: self.worker_id,
            process_epoch: self.process_epoch,
        }
    }

    /// Make an earlier snapshot of this worker the accepted environment.
    ///
    /// Every candidate created before this call, from any other accepted environment, is
    /// refused by [`LeanWorker::accept`].
    pub fn restore(&mut self, snapshot: EnvSnapshot) -> Result<(), WorkerError> {
        self.ensure_ready()?;
        if snapshot.worker_id != self.worker_id {
            return Err(WorkerError::ForeignSnapshot);
        }
        if snapshot.process_epoch != self.process_epoch {
            return Err(WorkerError::StaleSnapshot);
        }
        self.set_accepted(snapshot.environment);
        Ok(())
    }

    /// Reset future checks to the import-only baseline (a restore to the base snapshot).
    pub fn reset_to_base(&mut self) {
        self.set_accepted(self.base_environment);
    }

    /// Replace the REPL process with a fresh import-only baseline.
    ///
    /// Snapshots and candidates from the previous process become invalid. If the new process
    /// cannot start, the worker keeps its previous process and accepted state (including an
    /// unavailable state after a transport failure). The caller must replay accepted source
    /// and replace its saved snapshots after a successful restart.
    ///
    /// The REPL retains all environments until restart. Compaction must be coordinated by
    /// the caller: an automatic restart here would invalidate its incremental snapshots.
    pub fn restart(&mut self) -> Result<(), WorkerError> {
        let (repl, environment) = start_repl(self.config.clone())?;
        self.repl = Some(repl);
        self.base_environment = environment;
        self.process_epoch = self.process_epoch.wrapping_add(1);
        self.set_accepted(environment);
        Ok(())
    }

    /// Commit a candidate after the caller has completed all mechanical SIR checks.
    ///
    /// Refuses candidates from another worker or REPL process, candidates whose parent is not
    /// the current accepted environment, and candidates created before the last restore,
    /// reset, or accepted environment change.
    pub fn accept(&mut self, candidate: CandidateEnvironment) -> Result<u64, WorkerError> {
        self.ensure_ready()?;
        if candidate.parent_environment != self.accepted_environment
            || candidate.generation != self.generation
            || candidate.worker_id != self.worker_id
            || candidate.process_epoch != self.process_epoch
        {
            return Err(WorkerError::StaleCandidate);
        }
        self.set_accepted(candidate.environment);
        Ok(self.accepted_environment)
    }

    /// Validate, elaborate, and extract one declaration without committing its snapshot.
    ///
    /// Transport failures make the worker unavailable until an explicit [`Self::restart`].
    /// Lean diagnostics and rejected trusted payloads leave the accepted environment usable.
    pub fn check_statement(
        &mut self,
        request: StatementRequest<'_>,
    ) -> Result<CheckResult, WorkerError> {
        self.check_declaration(request, false)
    }

    /// Check one complete theorem proof using the proof-specific admission path. This permits
    /// tactic syntax in the theorem body while retaining one-declaration and blocked-syntax
    /// checks; statement formalization continues to require the exact `by sorry` placeholder.
    pub fn check_proof(
        &mut self,
        request: StatementRequest<'_>,
    ) -> Result<CheckResult, WorkerError> {
        if request.source.contains("--") || request.source.contains("/-") {
            return Err(payload_error("proof validation", "comments are forbidden"));
        }
        self.check_declaration(request, true)
    }

    fn check_declaration(
        &mut self,
        request: StatementRequest<'_>,
        proof: bool,
    ) -> Result<CheckResult, WorkerError> {
        self.ensure_ready()?;
        if request.source.contains('\0') {
            return Err(payload_error("validation", "NUL bytes are forbidden"));
        }
        if !valid_declaration_name(request.declaration_name) {
            return Err(WorkerError::InvalidDeclarationName);
        }
        let validator = if proof {
            "#mls_validate_proof_source"
        } else {
            "#mls_validate_source"
        };
        let validation_response = self.request(&json!({
            "env": self.accepted_environment,
            "cmd": format!("{validator} {} from_source {}", request.declaration_name, lean_string(request.source)),
        }))?;
        let validation_messages = messages(&validation_response)?;
        reject_errors(validation_messages.clone())?;
        let validation: Validation = unique_payload(
            &validation_messages,
            if proof {
                VALIDATE_PROOF_PREFIX
            } else {
                VALIDATE_PREFIX
            },
            if proof {
                "proof validation"
            } else {
                "validation"
            },
        )?;
        if validation.declaration != request.declaration_name {
            return Err(payload_error(
                "validation",
                "declaration name does not match request",
            ));
        }
        if validation.kind != "theorem" && validation.kind != "definition" {
            return Err(payload_error("validation", "unsupported declaration kind"));
        }

        let command = format!(
            "-- block:{}\n{}\n#mls_extract {} from_source {}",
            request.block_id,
            request.source,
            request.declaration_name,
            lean_string(request.source)
        );
        let response =
            self.request(&json!({ "env": self.accepted_environment, "cmd": command }))?;
        let response_messages = messages(&response)?;
        reject_errors(response_messages.clone())?;
        let extracted: ExtractedDeclaration =
            unique_payload(&response_messages, EXTRACT_PREFIX, "extraction")?;
        if extracted.declaration != request.declaration_name || extracted.kind != validation.kind {
            return Err(payload_error(
                "extraction",
                "payload identity differs from validated source",
            ));
        }
        let next_environment = environment(&response)?;
        let candidate = CandidateEnvironment {
            environment: next_environment,
            parent_environment: self.accepted_environment,
            generation: self.generation,
            worker_id: self.worker_id,
            process_epoch: self.process_epoch,
        };
        Ok(CheckResult {
            candidate,
            validation,
            extracted,
            messages: response_messages,
        })
    }

    /// Move the accepted environment and invalidate every outstanding candidate.
    fn set_accepted(&mut self, environment: u64) {
        self.accepted_environment = environment;
        self.generation = self.generation.wrapping_add(1);
    }

    fn ensure_ready(&self) -> Result<(), WorkerError> {
        self.repl.as_ref().ok_or(WorkerError::NeedsRestart)?;
        Ok(())
    }

    /// A transport failure can leave the stream uncorrelated or the process dead. Drop it
    /// immediately so subsequent operations report a stable error and cannot commit state.
    fn request(&mut self, request: &Value) -> Result<Value, WorkerError> {
        let result = self
            .repl
            .as_mut()
            .ok_or(WorkerError::NeedsRestart)?
            .request(request);
        if result.is_err() {
            self.repl = None;
        }
        result.map_err(WorkerError::Repl)
    }
}

/// Spawn a REPL process and import the trusted baseline, returning the baseline environment.
fn start_repl(config: ReplConfig) -> Result<(ReplProcess, u64), WorkerError> {
    let mut repl = ReplProcess::spawn(config)?;
    let response = repl.request(&json!({
        "cmd": "import Mathlib\nimport MlsExtract\nset_option autoImplicit false"
    }))?;
    let messages = messages(&response)?;
    reject_errors(messages)?;
    let environment = environment(&response)?;
    Ok((repl, environment))
}

fn valid_declaration_name(name: &str) -> bool {
    name.strip_prefix("mls_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn environment(response: &Value) -> Result<u64, WorkerError> {
    response
        .get("env")
        .and_then(Value::as_u64)
        .ok_or(WorkerError::MissingEnvironment)
}

fn messages(response: &Value) -> Result<Vec<ReplDiagnostic>, WorkerError> {
    serde_json::from_value(
        response
            .get("messages")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(|error| payload_error("response", &error.to_string()))
}

fn reject_errors(messages: Vec<ReplDiagnostic>) -> Result<(), WorkerError> {
    let errors: Vec<_> = messages
        .into_iter()
        .filter(|message| message.severity == crate::Severity::Error)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WorkerError::Diagnostics(errors))
    }
}

fn unique_payload<T: for<'de> Deserialize<'de>>(
    messages: &[ReplDiagnostic],
    prefix: &str,
    stage: &'static str,
) -> Result<T, WorkerError> {
    let payloads: Vec<_> = messages
        .iter()
        .filter(|message| message.severity == crate::Severity::Info)
        .filter_map(|message| message.data.strip_prefix(prefix))
        .collect();
    if payloads.len() != 1 {
        return Err(payload_error(
            stage,
            &format!("expected one {prefix} info message, got {}", payloads.len()),
        ));
    }
    serde_json::from_str(payloads[0]).map_err(|error| payload_error(stage, &error.to_string()))
}

fn payload_error(stage: &'static str, message: &str) -> WorkerError {
    WorkerError::TrustedPayload {
        stage,
        message: message.to_owned(),
    }
}

fn lean_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Duration};

    fn config() -> ReplConfig {
        ReplConfig {
            program: PathBuf::from("python3"),
            args: vec![format!(
                "{}/tests/fixtures/fake_repl.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            env: vec![("FAKE_REPL_MODE".into(), "worker".into())],
            timeout: Duration::from_secs(5),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 64,
        }
    }

    #[test]
    fn check_after_restore_builds_on_the_restored_parent() {
        let mut worker = LeanWorker::spawn(config()).unwrap();
        let base = worker.snapshot();
        let first = worker
            .check_statement(StatementRequest {
                block_id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
                declaration_name: "mls_demo",
                source: "theorem mls_demo : True := by sorry",
            })
            .unwrap();
        worker.accept(first.candidate).unwrap();
        worker.restore(base).unwrap();
        let second = worker
            .check_statement(StatementRequest {
                block_id: "00000000-0000-4000-8000-000000000002".parse().unwrap(),
                declaration_name: "mls_demo",
                source: "theorem mls_demo : True := by sorry",
            })
            .unwrap();
        assert_eq!(second.candidate.parent_environment, base.environment);
    }
}
