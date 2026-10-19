//! Explicitly gated tests against the repository-local pinned Lean/Mathlib/REPL runtime.

use mls_lean::{LeanWorker, ReplConfig, StatementRequest, WorkerError};
use std::{path::PathBuf, time::Duration};

fn block_id(value: u8) -> mls_core::BlockId {
    format!("00000000-0000-4000-8000-{value:012}")
        .parse()
        .unwrap()
}

fn enabled() -> bool {
    std::env::var("MLS_LEAN_TESTS").as_deref() == Ok("1")
}

fn worker() -> LeanWorker {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate lives under repository/crates")
        .to_path_buf();
    LeanWorker::spawn(ReplConfig {
        program: repository.join("scripts/run-lean-repl.sh"),
        args: vec![],
        cwd: repository.join(".mls"),
        env: vec![],
        timeout: Duration::from_secs(180),
        max_stdout_bytes: 1024 * 1024,
        max_stderr_bytes: 64 * 1024,
    })
    .expect("pinned local Lean runtime must start")
}

#[test]
fn persistent_dependencies_and_failed_branch_repair() {
    if !enabled() {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let mut worker = worker();
    let first = worker
        .check_statement(StatementRequest {
            block_id: block_id(1),
            declaration_name: "mls_first",
            source: "theorem mls_first : True := by sorry",
        })
        .expect("first declaration should elaborate");
    assert!(first.extracted.has_sorry);
    worker.accept(first.candidate).unwrap();

    let accepted = worker.accepted_environment();
    let failed = worker.check_statement(StatementRequest {
        block_id: block_id(2),
        declaration_name: "mls_bad",
        source: "theorem mls_bad : MissingName := by sorry",
    });
    assert!(matches!(failed, Err(WorkerError::Diagnostics(_))));
    assert_eq!(worker.accepted_environment(), accepted);

    let second = worker
        .check_statement(StatementRequest {
            block_id: block_id(3),
            declaration_name: "mls_second",
            source: "def mls_second : True := mls_first",
        })
        .expect("repair from accepted pre-error environment should elaborate");
    worker.accept(second.candidate).unwrap();
}
