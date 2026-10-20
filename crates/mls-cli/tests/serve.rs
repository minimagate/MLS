//! Process-level checks exercise real stdio and explicit transport selection without any calls.
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[path = "serve/acceptance.rs"]
mod acceptance;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mls"));
    command
        .arg("serve")
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY");
    command
}
fn replay() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/recordings/models/success")
}

#[test]
fn no_implicit_live_and_live_requires_both_opt_in_and_key() {
    for args in [
        vec![],
        vec!["--live"],
        vec!["--live", "--replay", "unused"],
        vec!["--replay"],
    ] {
        let output = command().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let output = command()
        .arg("--live")
        .env("MLS_LIVE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("OPENROUTER_API_KEY")
    );
}

#[test]
fn real_stdio_preserves_ids_recovers_from_parse_error_and_shuts_down_without_jobs() {
    let mut child = command()
        .arg("--replay")
        .arg(replay())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{{").unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":"start","method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_ascii());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["id"], "start");
    assert_eq!(replies[1]["result"]["protocol"], 1);
    assert_eq!(replies[2]["id"], 99);
    assert!(replies[2]["result"]["usage"]["formalization"].is_null());
    assert!(replies[2]["result"]["usage"]["decisions"].is_null());
}

#[test]
fn eof_is_disconnect_and_oversized_or_partial_lines_are_bounded_errors() {
    for line in [Vec::new(), b"{}".to_vec(), vec![b'x'; 4 * 1024 * 1024 + 1]] {
        let mut child = command()
            .arg("--replay")
            .arg(replay())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&line).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        if line.is_empty() {
            assert!(output.stdout.is_empty());
        } else {
            let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(reply["error"]["code"], -32700);
        }
    }
}
