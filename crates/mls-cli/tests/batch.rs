//! Gated acceptance tests for CLI batch replay through the pinned local Lean runtime.

use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn fixture(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(path)
}

#[test]
fn recorded_tm_to_real_lean_report_and_shadow_project() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        temporary.path().join("notes.tm"),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .current_dir(temporary.path())
        .args([
            "batch",
            "notes.tm",
            "--replay",
            fixture("recordings/pipeline/success").to_str().unwrap(),
            "--output",
            "report.json",
        ])
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: Value =
        serde_json::from_slice(&fs::read(temporary.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report["blocks"].as_array().unwrap().len(), 2);
    for block in report["blocks"].as_array().unwrap() {
        assert_eq!(block["status"], "sir_drafted");
        assert_eq!(block["elaborated"], true);
        assert_eq!(block["accepted_speculatively"], true);
        assert_eq!(block["attempts"], 1);
        assert!(block.get("confirmed").is_none());
        assert!(block.get("verified").is_none());
    }

    let shadow = temporary.path().join(".mls/Shadow");
    for name in ["Section1.lean", "Section2.lean"] {
        let actual = fs::read_to_string(shadow.join(name)).unwrap();
        let expected = fs::read_to_string(fixture(&format!("lean/pipeline/{name}"))).unwrap();
        assert_eq!(actual, expected, "{name}");
    }
    assert!(temporary.path().join(".mls/lakefile.lean").is_file());
    assert!(temporary.path().join(".mls/lean-toolchain").is_file());
}

#[test]
fn rerun_recreates_a_deleted_generated_section_listed_in_the_manifest() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        temporary.path().join("notes.tm"),
    )
    .unwrap();
    let run = |output: &str| {
        Command::new(env!("CARGO_BIN_EXE_mls"))
            .current_dir(temporary.path())
            .args([
                "batch",
                "notes.tm",
                "--replay",
                fixture("recordings/pipeline/success").to_str().unwrap(),
                "--output",
                output,
            ])
            .env_remove("MLS_LIVE")
            .env_remove("OPENROUTER_API_KEY")
            .output()
            .unwrap()
    };
    let first = run("first.json");
    assert!(
        first.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let shadow = temporary.path().join(".mls/Shadow");
    fs::remove_file(shadow.join("Section2.lean")).unwrap();

    let second = run("second.json");
    assert!(
        second.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(temporary.path().join("second.json").is_file());
    assert!(shadow.join("Section2.lean").is_file());
    let manifest: Value =
        serde_json::from_slice(&fs::read(shadow.join(".mls-files.json")).unwrap()).unwrap();
    assert_eq!(
        manifest,
        serde_json::json!(["Section1.lean", "Section2.lean"])
    );
}

#[test]
fn replay_failure_never_falls_back_to_live_transport() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        temporary.path().join("notes.tm"),
    )
    .unwrap();
    let empty = temporary.path().join("empty-recording");
    fs::create_dir(&empty).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .current_dir(temporary.path())
        .args([
            "batch",
            "notes.tm",
            "--replay",
            empty.to_str().unwrap(),
            "--output",
            "report.json",
        ])
        .env("OPENROUTER_API_KEY", "must-not-be-used")
        .env_remove("MLS_LIVE")
        .output()
        .unwrap();
    // The batch currently reports model failures per block, so CLI completion is valid. The
    // deterministic replay error proves it did not switch transports despite a credential.
    assert!(output.status.success());
    let report: Value =
        serde_json::from_slice(&fs::read(temporary.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report["blocks"][0]["status"], "formalization_error");
    assert_eq!(
        report["blocks"][0]["errors"][0],
        "no exact replay match for request"
    );
}

#[test]
fn generated_cross_section_dependency_compiles_with_pinned_project() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/dependency.tm"),
        temporary.path().join("dependency.tm"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .current_dir(temporary.path())
        .args([
            "batch",
            "dependency.tm",
            "--replay",
            fixture("recordings/pipeline/dependency").to_str().unwrap(),
            "--output",
            "report.json",
        ])
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value =
        serde_json::from_slice(&fs::read(temporary.path().join("report.json")).unwrap()).unwrap();
    assert!(
        report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["accepted_speculatively"] == true)
    );
    assert_eq!(
        report["blocks"][0]["checks"][2]["detail"],
        "expected 1, got 1"
    );

    let section1 = fs::read_to_string(temporary.path().join(".mls/Shadow/Section1.lean")).unwrap();
    let section2 = fs::read_to_string(temporary.path().join(".mls/Shadow/Section2.lean")).unwrap();
    let dependency_name = "mls_00000000_0000_4000_8000_000000000007";
    assert!(section1.contains(&format!("def {dependency_name} (x : Nat) : Nat := x")));
    assert!(section2.contains("import Shadow.Section1"));
    assert!(section2.contains(&format!("{dependency_name} 0 = 0")));

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let lean_bin = repository.join(".toolchains/lean-4.19.0-darwin_aarch64/bin");
    let path = format!(
        "{}:{}",
        lean_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let build = Command::new(lean_bin.join("lake"))
        .current_dir(temporary.path().join(".mls"))
        .args(["build", "Shadow"])
        .env("PATH", path)
        .env(
            "MATHLIB_CACHE_DIR",
            repository.join(".toolchains/mathlib-cache"),
        )
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        temporary
            .path()
            .join(".mls/.lake/build/lib/lean/Shadow/Section2.olean")
            .is_file()
    );
}
