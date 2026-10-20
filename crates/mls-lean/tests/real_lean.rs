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

#[test]
fn notation_admission_and_group_confinement_through_worker() {
    if !enabled() {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let mut worker = worker();
    let admitted = [
        (
            "mls_abs_le",
            "theorem mls_abs_le (x : ℝ) : |x| ≤ 1 := by sorry",
            "app",
        ),
        (
            "mls_bounded",
            "theorem mls_bounded (u : ℕ → ℝ) : ∃ M : ℝ, ∀ n : ℕ, |u n| ≤ M := by sorry",
            "exists",
        ),
        (
            "mls_forall_mem",
            "theorem mls_forall_mem (S : Set ℝ) (P : ℝ → Prop) : ∀ x ∈ S, P x := by sorry",
            "forall",
        ),
        (
            "mls_setb_typed",
            "theorem mls_setb_typed (P : ℝ → Prop) (S : Set ℝ) : {x : ℝ | P x} = S := by sorry",
            "eq",
        ),
    ];
    for (index, (declaration_name, source, head)) in admitted.into_iter().enumerate() {
        let result = worker
            .check_statement(StatementRequest {
                block_id: block_id(10 + index as u8),
                declaration_name,
                source,
            })
            .expect("admitted notation should elaborate");
        assert_eq!(result.extracted.goal_outer_head, head);
    }

    // `⋃ x : ℕ, S x` carries a `group` outside abs notation and typed set-builders.
    let rejected = worker.check_statement(StatementRequest {
        block_id: block_id(20),
        declaration_name: "mls_group_union",
        source: "theorem mls_group_union (S : ℕ → Set ℝ) (T : Set ℝ) : (⋃ x : ℕ, S x) = T := by sorry",
    });
    match rejected {
        Err(WorkerError::Diagnostics(diagnostics)) => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.data.contains("unsupported syntax 'group'")),
            "unexpected diagnostics: {diagnostics:?}"
        ),
        Ok(_) => panic!("a group outside abs notation must not be admitted"),
        Err(other) => panic!("expected Lean diagnostics, got {other}"),
    }
}

#[test]
fn deriving_and_unsafe_identifiers_are_rejected_before_elaboration() {
    if !enabled() {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let mut worker = worker();
    let rejected = [
        (
            "mls_e3",
            "def mls_e3 : Nat := 0 deriving Repr, BEq",
            "unsupported syntax 'deriving'",
        ),
        (
            "mls_m9",
            "def mls_m9 : Nat := sorryAx Nat true",
            "unsupported syntax 'sorryAx'",
        ),
    ];
    for (index, (declaration_name, source, fragment)) in rejected.into_iter().enumerate() {
        let result = worker.check_statement(StatementRequest {
            block_id: block_id(30 + index as u8),
            declaration_name,
            source,
        });
        match result {
            Err(WorkerError::Diagnostics(diagnostics)) => assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.data.contains(fragment)),
                "{declaration_name}: unexpected diagnostics: {diagnostics:?}"
            ),
            Ok(_) => panic!("{declaration_name} must not be admitted"),
            Err(other) => panic!("{declaration_name}: expected Lean diagnostics, got {other}"),
        }
    }
}

fn probe(
    worker: &mut LeanWorker,
    block: u8,
    name: &str,
    reference: &str,
) -> Result<(), WorkerError> {
    worker
        .check_statement(StatementRequest {
            block_id: block_id(block),
            declaration_name: name,
            source: &format!("def {name} : True := {reference}"),
        })
        .map(|_| ())
}

fn assert_known(worker: &mut LeanWorker, block: u8, reference: &str) {
    let name = format!("mls_probe_{block}");
    probe(worker, block, &name, reference)
        .unwrap_or_else(|error| panic!("{reference} should be known: {error}"));
}

fn assert_unknown(worker: &mut LeanWorker, block: u8, reference: &str) {
    let name = format!("mls_probe_{block}");
    match probe(worker, block, &name, reference) {
        Err(WorkerError::Diagnostics(diagnostics)) => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.data.contains(reference)),
            "{reference}: unexpected diagnostics: {diagnostics:?}"
        ),
        Ok(()) => panic!("{reference} must be unknown in this environment"),
        Err(other) => panic!("{reference}: expected Lean diagnostics, got {other}"),
    }
}

#[test]
fn restore_discards_later_branches_and_reaches_base() {
    if !enabled() {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let mut worker = worker();
    let base = worker.snapshot();

    let a = worker
        .check_statement(StatementRequest {
            block_id: block_id(40),
            declaration_name: "mls_a",
            source: "theorem mls_a : True := by sorry",
        })
        .expect("mls_a should elaborate");
    worker.accept(a.candidate).unwrap();
    let s1 = worker.snapshot();

    let b = worker
        .check_statement(StatementRequest {
            block_id: block_id(41),
            declaration_name: "mls_b",
            source: "def mls_b : True := mls_a",
        })
        .expect("mls_b should build on mls_a");
    worker.accept(b.candidate).unwrap();
    assert_known(&mut worker, 42, "mls_b");

    worker.restore(s1).unwrap();
    assert_unknown(&mut worker, 43, "mls_b");
    assert_known(&mut worker, 44, "mls_a");

    // Accept a different branch from s1; the abandoned mls_b stays unknown.
    let b_prime = worker
        .check_statement(StatementRequest {
            block_id: block_id(45),
            declaration_name: "mls_b_prime",
            source: "def mls_b_prime : True := mls_a",
        })
        .expect("mls_b_prime should build on mls_a");
    worker.accept(b_prime.candidate).unwrap();
    assert_known(&mut worker, 46, "mls_b_prime");
    assert_unknown(&mut worker, 47, "mls_b");

    worker.restore(base).unwrap();
    assert_unknown(&mut worker, 48, "mls_a");
}
