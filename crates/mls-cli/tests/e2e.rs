//! End-to-end command-line behavior and source-mutation checks.

use std::{fs, process::Command};

#[test]
fn default_is_read_only_and_write_ids_is_explicit() {
    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A<label|thm:a></theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["blocks"][0]["kind"], "theorem");
    assert_eq!(fs::read(&path).unwrap(), source);
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["--write-ids", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read(&path)
            .unwrap()
            .windows(13)
            .any(|window| window == b"<\\with|mls-id")
    );
}

#[test]
fn malformed_input_fails_without_modification() {
    let source = b"<\\theorem>broken</lemma>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.tm");
    fs::write(&path, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["--write-ids", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(path).unwrap(), source);
}

#[test]
fn invalid_identity_metadata_blocks_writes() {
    let source = b"<\\body><with|mls-id|broken|<\\theorem>A</theorem>></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid-id.tm");
    fs::write(&path, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["--write-ids", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(path).unwrap(), source);
}
