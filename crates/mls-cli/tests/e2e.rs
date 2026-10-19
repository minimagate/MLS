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

#[test]
fn batch_never_chooses_live_transport_implicitly() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(
        &path,
        b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("choose exactly one of --replay DIR or --live")
    );
}

#[test]
fn live_flag_requires_explicit_environment_gate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(
        &path,
        b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap(), "--live"])
        .env_remove("MLS_LIVE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--live requires MLS_LIVE=1"));
}

#[test]
fn batch_rejects_source_as_output_before_side_effects() {
    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap(), "--replay"])
        .arg(directory.path().join("missing-recordings"))
        .args(["--output", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must not alias"));
    assert_eq!(fs::read(&path).unwrap(), source);
    assert!(!directory.path().join(".mls").exists());
}

#[cfg(unix)]
#[test]
fn batch_rejects_symlink_and_hardlink_output_aliases() {
    use std::os::unix::fs::symlink;

    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    for hard_link in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.tm");
        let alias = directory.path().join("alias.json");
        fs::write(&path, source).unwrap();
        if hard_link {
            fs::hard_link(&path, &alias).unwrap();
        } else {
            symlink(&path, &alias).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_mls"))
            .args(["batch", path.to_str().unwrap(), "--replay"])
            .arg(directory.path().join("missing-recordings"))
            .args(["--output", alias.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must not alias"));
        assert_eq!(fs::read(&path).unwrap(), source);
        assert!(!directory.path().join(".mls").exists());
    }
}

#[test]
fn batch_checks_output_parent_before_any_side_effect() {
    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    fs::write(directory.path().join("not-a-directory"), b"file").unwrap();
    for output in [
        directory.path().join("missing/report.json"),
        directory.path().join("not-a-directory/report.json"),
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_mls"))
            .args(["batch", path.to_str().unwrap(), "--replay"])
            .arg(directory.path().join("missing-recordings"))
            .arg("--output")
            .arg(&output)
            .env_remove("MLS_LIVE")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("output directory"));
        assert!(!output.exists());
        assert_eq!(fs::read(&path).unwrap(), source);
        assert!(!directory.path().join(".mls").exists());
    }
}

#[test]
fn batch_refuses_outputs_inside_mls_directories_before_any_side_effect() {
    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let runtime_template =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.mls/report.json");
    for relative in [
        ".mls/report.json",
        ".mls/foo.json",
        ".mls/Shadow/Section0.lean",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.tm");
        fs::write(&path, source).unwrap();
        let output = directory.path().join(relative);
        let before = tree(directory.path());

        let result = Command::new(env!("CARGO_BIN_EXE_mls"))
            .args(["batch", path.to_str().unwrap(), "--replay"])
            .arg(directory.path().join("missing-recordings"))
            .arg("--output")
            .arg(&output)
            .env_remove("MLS_LIVE")
            .env_remove("OPENROUTER_API_KEY")
            .output()
            .unwrap();
        assert!(!result.status.success(), "{relative} was accepted");
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("must not be inside the MLS directory")
        );
        assert!(!output.exists());
        assert!(!directory.path().join(".mls").exists());
        assert_eq!(tree(directory.path()), before);
        assert_eq!(fs::read(&path).unwrap(), source);
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap(), "--replay"])
        .arg(directory.path().join("missing-recordings"))
        .arg("--output")
        .arg(&runtime_template)
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("must not be inside the MLS directory")
    );
    assert!(!runtime_template.exists());
    assert_eq!(fs::read(&path).unwrap(), source);
}

#[cfg(unix)]
#[test]
fn batch_refuses_a_read_only_output_directory_before_creating_a_project() {
    use std::os::unix::fs::PermissionsExt;

    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    let readonly = directory.path().join("readonly");
    fs::create_dir(&readonly).unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).unwrap();
    // Root can write into a read-only directory, so the refusal cannot be observed there.
    if fs::File::create(readonly.join("root-check")).is_ok() {
        fs::remove_file(readonly.join("root-check")).unwrap();
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let before = tree(directory.path());

    let result = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap(), "--replay"])
        .arg(directory.path().join("missing-recordings"))
        .arg("--output")
        .arg(readonly.join("report.json"))
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("not writable"));
    assert!(!directory.path().join(".mls").exists());
    assert_eq!(tree(directory.path()), before);
}

#[cfg(unix)]
#[test]
fn batch_resolves_symlinks_before_dot_dot_when_refusing_mls_outputs() {
    use std::os::unix::fs::symlink;

    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.tm");
    fs::write(&path, source).unwrap();
    fs::create_dir(directory.path().join("sub")).unwrap();
    fs::create_dir_all(directory.path().join(".mls/inner")).unwrap();
    // `link` resolves to the document's subdirectory, so `link/..` is the document directory.
    symlink(directory.path().join("sub"), directory.path().join("link")).unwrap();
    // `inner-link` resolves inside the project, so `inner-link/..` is the project itself.
    symlink(
        directory.path().join(".mls/inner"),
        directory.path().join("inner-link"),
    )
    .unwrap();
    let before = tree(directory.path());
    let run = |output: std::path::PathBuf| {
        Command::new(env!("CARGO_BIN_EXE_mls"))
            .args(["batch", path.to_str().unwrap(), "--replay"])
            .arg(directory.path().join("missing-recordings"))
            .arg("--output")
            .arg(output)
            .env_remove("MLS_LIVE")
            .env_remove("OPENROUTER_API_KEY")
            .output()
            .unwrap()
    };

    for refused in [
        directory.path().join("link/../.mls/report.json"),
        directory.path().join("sub/../.mls/report.json"),
        directory.path().join("inner-link/../report.json"),
    ] {
        let result = run(refused.clone());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success(),
            "{} was accepted",
            refused.display()
        );
        assert!(
            stderr.contains("must not be inside the MLS directory"),
            "{}: {stderr}",
            refused.display()
        );
    }
    assert_eq!(tree(directory.path()), before);
    assert_eq!(fs::read(&path).unwrap(), source);

    // An ordinary output next to the document is not refused by the MLS-directory check.
    let result = run(directory.path().join("report.json"));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !stderr.contains("must not be inside the MLS directory"),
        "{stderr}"
    );
    assert!(!stderr.contains("does not exist; spell"), "{stderr}");

    // A `..` after a directory that does not exist yet is refused, because its meaning is unknown.
    let result = run(directory.path().join("missing/../report.json"));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(stderr.contains("does not exist; spell"), "{stderr}");
    assert!(!directory.path().join("report.json").exists());
    assert_eq!(fs::read(&path).unwrap(), source);
}

/// Every path under `root` with its file bytes (`None` for directories), sorted.
fn tree(root: &std::path::Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
                entries.push((path, None));
            } else {
                let bytes = fs::read(&path).unwrap();
                entries.push((path, Some(bytes)));
            }
        }
    }
    entries.sort();
    entries
}

#[test]
fn batch_refuses_a_tampered_shadow_owner_before_any_model_call() {
    let source = b"<TeXmacs|2.1.4><\\body><\\theorem>A</theorem></body>";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.tm");
    fs::write(&path, source).unwrap();
    let project = directory.path().join(".mls");
    let shadow = project.join("Shadow");
    fs::create_dir_all(&shadow).unwrap();
    fs::write(project.join(".mls-owner.json"), b"{\"format\":1}\n").unwrap();
    fs::write(shadow.join(".mls-owner"), b"/elsewhere/other.tm").unwrap();
    // An empty recording directory makes any model call fail per block, so a model call would
    // leave a report behind and succeed. The refusal must come first.
    let recordings = directory.path().join("empty-recording");
    fs::create_dir(&recordings).unwrap();
    let before = tree(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_mls"))
        .args(["batch", path.to_str().unwrap(), "--replay"])
        .arg(&recordings)
        .args([
            "--output",
            directory.path().join("report.json").to_str().unwrap(),
        ])
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("another document or user owns this Shadow directory")
    );
    assert!(output.stdout.is_empty());
    assert_eq!(tree(directory.path()), before);
}
