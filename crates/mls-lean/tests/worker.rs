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

fn check(worker: &mut LeanWorker, source: &str) -> Result<mls_lean::CheckResult, WorkerError> {
    worker.check_statement(StatementRequest {
        block_id: block_id(),
        declaration_name: "mls_demo",
        source,
    })
}

const DEMO: &str = "theorem mls_demo : True := by sorry";

#[test]
fn foreign_snapshot_is_rejected_even_with_matching_env_id() {
    let first = LeanWorker::spawn(config("worker")).unwrap();
    let mut second = LeanWorker::spawn(config("worker")).unwrap();
    let foreign = first.snapshot();
    // Both fixture processes number their environments identically, so only the worker
    // identity distinguishes these snapshots.
    let result = check(&mut second, DEMO).unwrap();
    let accepted = second.accept(result.candidate).unwrap();
    assert!(matches!(
        second.restore(foreign),
        Err(WorkerError::ForeignSnapshot)
    ));
    assert_eq!(second.accepted_environment(), accepted);
}

#[test]
fn candidates_created_before_a_restore_are_refused() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let base = worker.snapshot();
    let base_env = worker.accepted_environment();
    let first = check(&mut worker, DEMO).unwrap();
    worker.accept(first.candidate).unwrap();

    // Derived from a non-base accepted environment, then the worker is restored to base.
    let branch = check(&mut worker, DEMO).unwrap();
    worker.restore(base).unwrap();
    assert_eq!(worker.accepted_environment(), base_env);
    assert!(matches!(
        worker.accept(branch.candidate),
        Err(WorkerError::StaleCandidate)
    ));
    assert_eq!(worker.accepted_environment(), base_env);

    // A candidate derived from the restored environment is refused by a same-env restore.
    let same = check(&mut worker, DEMO).unwrap();
    let current = worker.snapshot();
    worker.restore(current).unwrap();
    assert!(matches!(
        worker.accept(same.candidate),
        Err(WorkerError::StaleCandidate)
    ));
    assert_eq!(worker.accepted_environment(), base_env);
}

#[test]
fn restore_then_check_builds_on_restored_environment() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let base = worker.snapshot();
    let base_env = worker.accepted_environment();
    let first = check(&mut worker, DEMO).unwrap();
    let first_env = worker.accept(first.candidate).unwrap();
    let saved = worker.snapshot();
    let second = check(&mut worker, DEMO).unwrap();
    worker.accept(second.candidate).unwrap();

    worker.restore(saved).unwrap();
    assert_eq!(worker.accepted_environment(), first_env);
    let resumed = check(&mut worker, DEMO).unwrap();
    let resumed_env = worker.accept(resumed.candidate).unwrap();
    assert_ne!(resumed_env, first_env);
    assert_eq!(worker.accepted_environment(), resumed_env);

    // The base snapshot still restores the import-only environment.
    worker.restore(base).unwrap();
    assert_eq!(worker.accepted_environment(), base_env);
}

#[test]
fn restart_invalidates_snapshots_and_candidates_from_the_old_process() {
    let mut worker = LeanWorker::spawn(config("worker")).unwrap();
    let before_restart = worker.snapshot();
    let pending = check(&mut worker, DEMO).unwrap();

    worker.restart().unwrap();
    assert!(matches!(
        worker.restore(before_restart),
        Err(WorkerError::StaleSnapshot)
    ));
    assert!(matches!(
        worker.accept(pending.candidate),
        Err(WorkerError::StaleCandidate)
    ));

    // Snapshots of the fresh process are valid again.
    let fresh = worker.snapshot();
    worker.restore(fresh).unwrap();
    let next = check(&mut worker, DEMO).unwrap();
    worker.accept(next.candidate).unwrap();
}

#[test]
fn timeout_requires_restart_before_check_accept_or_restore() {
    for mode in ["worker-timeout-validation", "worker-timeout-extraction"] {
        let mut worker = LeanWorker::spawn(config(mode)).unwrap();
        let accepted = check(&mut worker, DEMO).unwrap();
        let accepted_env = worker.accept(accepted.candidate).unwrap();
        let snapshot = worker.snapshot();
        let pending = check(&mut worker, DEMO).unwrap();
        let before_restart = check(&mut worker, DEMO).unwrap();

        assert!(matches!(
            check(
                &mut worker,
                "theorem mls_demo : True := by sorry -- trigger_transport_failure"
            ),
            Err(WorkerError::Repl(ReplError::Timeout(_)))
        ));
        assert_eq!(worker.accepted_environment(), accepted_env);
        assert!(matches!(
            check(&mut worker, DEMO),
            Err(WorkerError::NeedsRestart)
        ));
        assert!(matches!(
            worker.accept(pending.candidate),
            Err(WorkerError::NeedsRestart)
        ));
        assert!(matches!(
            worker.restore(snapshot),
            Err(WorkerError::NeedsRestart)
        ));
        worker.reset_to_base();
        assert!(matches!(
            check(&mut worker, DEMO),
            Err(WorkerError::NeedsRestart)
        ));

        worker.restart().unwrap();
        assert!(matches!(
            worker.restore(snapshot),
            Err(WorkerError::StaleSnapshot)
        ));
        assert!(matches!(
            worker.accept(before_restart.candidate),
            Err(WorkerError::StaleCandidate)
        ));
        let recovered = check(&mut worker, DEMO).unwrap();
        worker.accept(recovered.candidate).unwrap();
    }
}
