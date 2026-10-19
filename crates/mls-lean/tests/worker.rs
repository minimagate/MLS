//! Subprocess-level transport and transactional worker tests.

use mls_lean::{LeanWorker, ReplConfig, ReplError, StatementRequest, WorkerError};
use std::{path::PathBuf, time::Duration};

fn block_id() -> mls_core::BlockId {
    "00000000-0000-4000-8000-000000000001".parse().unwrap()
}

fn config(mode: &str) -> ReplConfig {
    ReplConfig {
        program: PathBuf::from("python3"),
        args: vec![format!(
            "{}/tests/fixtures/fake_repl.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        env: vec![("FAKE_REPL_MODE".into(), mode.into())],
        timeout: Duration::from_millis(150),
        max_stdout_bytes: 1024,
        max_stderr_bytes: 64,
    }
}

#[test]
fn candidate_requires_explicit_acceptance_and_failures_do_not_advance() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let base = worker.accepted_environment();
    let result = worker
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo",
            source: "theorem mls_demo : True := by sorry",
        })
        .unwrap();
    assert_eq!(worker.accepted_environment(), base);
    let accepted = worker.accept(result.candidate).unwrap();
    assert_ne!(accepted, base);
    let error = worker
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo",
            source: "theorem mls_demo : badType := by sorry",
        })
        .unwrap_err();
    assert!(matches!(error, WorkerError::Diagnostics(_)));
    assert_eq!(worker.accepted_environment(), accepted);
}

#[test]
fn reset_invalidates_candidate() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let result = worker
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo",
            source: "theorem mls_demo : True := by sorry",
        })
        .unwrap();
    worker.reset_to_base();
    assert!(matches!(
        worker.accept(result.candidate),
        Err(WorkerError::StaleCandidate)
    ));
}

#[test]
fn timeout_eof_and_stdout_bounds_are_reported() {
    assert!(matches!(
        LeanWorker::spawn(config("timeout")),
        Err(WorkerError::Repl(ReplError::Timeout(_)))
    ));
    assert!(matches!(
        LeanWorker::spawn(config("eof")),
        Err(WorkerError::Repl(ReplError::Eof { .. }))
    ));
    assert!(matches!(
        LeanWorker::spawn(config("oversize")),
        Err(WorkerError::Repl(ReplError::StdoutLimit { .. }))
    ));
    assert!(matches!(
        LeanWorker::spawn(config("unterminated")),
        Err(WorkerError::Repl(ReplError::StdoutLimit { .. }))
    ));
    assert!(matches!(
        LeanWorker::spawn(config("malformed")),
        Err(WorkerError::Repl(ReplError::Json(_)))
    ));
}

#[test]
fn duplicate_trusted_payload_is_rejected_without_advancing() {
    let mut worker = LeanWorker::spawn(config("duplicate")).unwrap();
    let accepted = worker.accepted_environment();
    let error = worker
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo",
            source: "theorem mls_demo : True := by sorry",
        })
        .unwrap_err();
    assert!(matches!(error, WorkerError::TrustedPayload { .. }));
    assert_eq!(worker.accepted_environment(), accepted);
}

#[test]
fn candidate_is_bound_to_its_originating_worker() {
    let mut first = LeanWorker::spawn(config("worker")).unwrap();
    let mut second = LeanWorker::spawn(config("worker")).unwrap();
    let result = first
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo",
            source: "theorem mls_demo : True := by sorry",
        })
        .unwrap();
    assert!(matches!(
        second.accept(result.candidate),
        Err(WorkerError::StaleCandidate)
    ));
}

#[test]
fn untrusted_command_fragments_cannot_enter_generated_commands() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let error = worker
        .check_statement(StatementRequest {
            block_id: block_id(),
            declaration_name: "mls_demo\n#eval 1",
            source: "theorem mls_demo : True := by sorry",
        })
        .unwrap_err();
    assert!(matches!(error, WorkerError::InvalidDeclarationName));
}
