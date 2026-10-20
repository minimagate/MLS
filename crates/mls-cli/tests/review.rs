//! Command-line tests for the review commands. Ungated tests use only state that they write
//! themselves (an owned project and a crafted report), so they never start Lean or call a model.
//! Gated tests (`MLS_LEAN_TESTS=1`) run the offline replay fixtures through the pinned runtime.

use mls_core::{Block, BlockKind};
use mls_engine::ReviewStore;
use mls_tm::{analyze, parse};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const DOCUMENT: &str = r#"<TeXmacs|2.1.4>
<\body>
  <section|First>
  <\with|mls-id|00000000-0000-4000-8000-000000000001><\theorem>Truth holds.<label|thm:first></theorem></with>
  <section|Second>
  <\with|mls-id|00000000-0000-4000-8000-000000000002><\theorem>Truth still holds by <reference|thm:first>.<label|thm:second></theorem></with>
</body>
"#;

const UNIDENTIFIED: &str = r#"<TeXmacs|2.1.4>
<\body>
  <\theorem>Truth holds.<label|thm:first></theorem>
</body>
"#;

fn block_id(number: u8) -> String {
    format!("00000000-0000-4000-8000-{number:012}")
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(path)
}

fn run_mls(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mls"))
        .current_dir(directory)
        .args(arguments)
        .env_remove("MLS_LIVE")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Asserts the exit status and the error code of a refused command.
fn assert_refused(output: &Output, path: &Path, status: i32, code: &str) {
    assert_eq!(
        output.status.code(),
        Some(status),
        "stderr={}",
        stderr(output)
    );
    assert_eq!(read_json(path)["error"], code);
}

/// Creates a document in `directory` and returns its path.
fn write_document(directory: &Path, source: &str) -> PathBuf {
    let path = directory.join("notes.tm");
    fs::write(&path, source).unwrap();
    path
}

/// An MLS-owned project directory, as batch and review-run would create it.
fn owned_project(directory: &Path) {
    let project = directory.join(".mls");
    fs::create_dir_all(project.join("review")).unwrap();
    fs::write(project.join(".mls-owner.json"), b"{\"format\":1}\n").unwrap();
}

fn sir_for(label: &str, uses: &[&str]) -> Value {
    json!({
        "kind": "theorem",
        "label": label,
        "variables": [],
        "hypotheses": [],
        "conclusion": "truth",
        "uses": uses,
        "assumptions_made": []
    })
}

/// The semantic hash of a SIR, obtained through the engine's store so the test needs no SIR type.
fn semantic_hash(block: &Block, uses: &[&str], sir: &Value) -> String {
    let expected: Vec<String> = uses.iter().map(|name| (*name).to_owned()).collect();
    let mut store = ReviewStore::new();
    store.edit(block, &expected, &sir.to_string()).unwrap();
    let stored: Value = serde_json::from_str(&store.to_json()).unwrap();
    stored["entries"][block.id.to_string()]["sir_hash"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The section 4 cache key, computed as the engine does: the block's SIR hash, then one
/// `"<block id> <SIR hash>"` line for each formal dependency, joined by newlines and hashed with
/// SHA-256. The view recomputes this key, so a wrong key shows up as a `needs_rerun` drift.
fn section4_key(sir_hash: &str, dependencies: &[(String, String)]) -> String {
    let mut lines = vec![sir_hash.to_owned()];
    lines.extend(dependencies.iter().map(|(id, hash)| format!("{id} {hash}")));
    mls_core::source_hash(lines.join("\n").as_bytes())
}

/// The crafted SIR, SIR hash and section 4 key of every theorem of `source`, by label.
fn crafted_blocks(source: &str) -> BTreeMap<String, (Value, String, String)> {
    let analysis = analyze(&parse(source.as_bytes()).unwrap());
    let theorems: Vec<&Block> = analysis
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Theorem)
        .collect();
    let uses_of = |label: &str| -> Vec<&'static str> {
        if label == "thm:second" {
            vec!["thm:first"]
        } else {
            Vec::new()
        }
    };
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for block in &theorems {
        let label = block.label.clone().unwrap();
        let uses = uses_of(&label);
        let sir = sir_for(&label, &uses);
        hashes.insert(label.clone(), semantic_hash(block, &uses, &sir));
        ids.insert(label, block.id.to_string());
    }
    let mut crafted = BTreeMap::new();
    for block in &theorems {
        let label = block.label.clone().unwrap();
        let uses = uses_of(&label);
        let sir = sir_for(&label, &uses);
        let dependencies: Vec<(String, String)> = uses
            .iter()
            .map(|dependency| (ids[*dependency].clone(), hashes[*dependency].clone()))
            .collect();
        let key = section4_key(&hashes[&label], &dependencies);
        crafted.insert(label.clone(), (sir, hashes[&label].clone(), key));
    }
    crafted
}

/// The section 4 key the last report of `DOCUMENT` carries for the block labelled `label`.
fn crafted_key(label: &str) -> String {
    crafted_blocks(DOCUMENT)[label].2.clone()
}

/// Writes the last report of `notes.tm` for every theorem of `source`, as if a run had produced
/// it: each block is drafted and its Lean statement accepted under its section 4 key.
fn write_crafted_report(directory: &Path, source: &str) {
    let analysis = analyze(&parse(source.as_bytes()).unwrap());
    let crafted = crafted_blocks(source);
    let mut blocks = Vec::new();
    for block in analysis
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Theorem)
    {
        let label = block.label.clone().unwrap();
        let (sir, sir_hash, key) = crafted[&label].clone();
        blocks.push(json!({
            "block_id": block.id.to_string(),
            "label": label,
            "source_start": block.source_start,
            "source_end": block.source_end,
            "text_hash": block.text_hash,
            "status": "sir_drafted",
            "sir": sir,
            "sir_hash": sir_hash,
            "cache_key": key,
            "sir_origin": "generated",
            "lean_origin": "generated",
            "cosmetic_reconfirmation": false,
            "superseded_confirmation": null,
            "previously_rejected": false,
            "lean_statement": format!("theorem {label} : True := by sorry"),
            "elaborated": true,
            "complete_structure": false,
            "accepted_speculatively": true,
            "checks": [],
            "attempts": 1,
            "errors": [],
            "lean_diagnostics": []
        }));
    }
    let report = json!({
        "diagnostics": [],
        "blocks": blocks,
        "usage": {
            "calls": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "unknown_input_token_calls": 0,
            "unknown_output_token_calls": 0,
            "known_cost": 0.0,
            "unknown_cost_calls": 0
        }
    });
    fs::write(
        directory.join(".mls/review/notes.tm.report.json"),
        report.to_string(),
    )
    .unwrap();
}

/// A crafted-report fixture: an owned project, the two-theorem document and its report.
fn reviewed_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    owned_project(directory.path());
    write_crafted_report(directory.path(), DOCUMENT);
    directory
}

fn view_block<'a>(view: &'a Value, label: &str) -> &'a Value {
    view["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["label"] == label)
        .unwrap()
}

fn store_path(directory: &Path) -> PathBuf {
    directory.join(".mls/review/notes.tm.store.json")
}

#[test]
fn review_run_refuses_missing_ids_before_any_side_effect() {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), UNIDENTIFIED);
    let output = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            directory.path().to_str().unwrap(),
            "--output",
            "out.json",
        ],
    );
    assert_refused(
        &output,
        &directory.path().join("out.json"),
        3,
        "missing_ids",
    );
    assert!(!directory.path().join(".mls").exists());
}

#[test]
fn review_run_needs_exactly_one_transport_and_the_live_gate() {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    let neither = run_mls(
        directory.path(),
        &["review-run", "notes.tm", "--output", "out.json"],
    );
    assert_eq!(neither.status.code(), Some(2), "{}", stderr(&neither));
    assert!(!directory.path().join("out.json").exists());

    let live = run_mls(
        directory.path(),
        &["review-run", "notes.tm", "--live", "--output", "live.json"],
    );
    assert_eq!(live.status.code(), Some(2), "{}", stderr(&live));
    assert!(stderr(&live).contains("MLS_LIVE=1"));
    assert!(!directory.path().join("live.json").exists());
    assert!(!directory.path().join(".mls").exists());
}

#[test]
fn outputs_inside_mls_and_unowned_projects_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    owned_project(directory.path());
    let inside = run_mls(
        directory.path(),
        &[
            "review-status",
            "notes.tm",
            "--output",
            ".mls/review/out.json",
        ],
    );
    assert_eq!(inside.status.code(), Some(2), "{}", stderr(&inside));
    assert!(!directory.path().join(".mls/review/out.json").exists());

    let unowned = tempfile::tempdir().unwrap();
    write_document(unowned.path(), DOCUMENT);
    fs::create_dir(unowned.path().join(".mls")).unwrap();
    let run = run_mls(
        unowned.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            unowned.path().to_str().unwrap(),
            "--output",
            "out.json",
        ],
    );
    assert!(!run.status.success(), "{}", stderr(&run));
    assert!(!unowned.path().join(".mls/review").exists());
    let status = run_mls(
        unowned.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(&status, &unowned.path().join("status.json"), 2, "refused");
}

#[test]
fn a_symlinked_review_directory_is_refused_and_its_target_stays_empty() {
    let directory = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    owned_project(directory.path());
    fs::remove_dir(directory.path().join(".mls/review")).unwrap();
    std::os::unix::fs::symlink(target.path(), directory.path().join(".mls/review")).unwrap();
    let empty = directory.path().join("empty-replay");
    fs::create_dir(&empty).unwrap();
    let output = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            empty.to_str().unwrap(),
            "--output",
            "out.json",
        ],
    );
    assert!(!output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
}

#[test]
fn review_status_without_a_report_lists_every_block_as_not_formalized() {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    let output = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let view = read_json(&directory.path().join("status.json"));
    assert_eq!(view["protocol"], 1);
    assert_eq!(view["needs_rerun"], true);
    assert_eq!(view["counts"]["not_formalized"], 2);
    assert_eq!(view["counts"]["statement_confirmed"], 0);
    assert_eq!(view["blocks"][0]["label"], "thm:first");
    assert_eq!(view["blocks"][1]["label"], "thm:second");
    assert!(
        view["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["needs_rerun"] == true && block["sir"].is_null())
    );
    assert!(!directory.path().join(".mls").exists());
}

#[test]
fn review_action_without_a_report_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    write_document(directory.path(), DOCUMENT);
    let output = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--output",
            "act.json",
        ],
    );
    assert_refused(&output, &directory.path().join("act.json"), 2, "no_report");
}

#[test]
fn confirm_records_the_accepted_lean_and_shows_statement_confirmed_without_a_run() {
    let directory = reviewed_directory();
    let output = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--output",
            "act.json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!stderr(&output).contains("Model call"));
    let view = read_json(&directory.path().join("act.json"));
    let first = view_block(&view, "thm:first");
    assert_eq!(first["status"], "statement_confirmed");
    assert_eq!(first["review"], "confirmed");
    assert_eq!(first["needs_rerun"], false);
    assert_eq!(view_block(&view, "thm:second")["status"], "sir_drafted");
    assert_eq!(view["counts"]["statement_confirmed"], 1);

    let store: Value = read_json(&store_path(directory.path()));
    assert_eq!(
        store["lean_cache"][block_id(1)]["cache_key"],
        crafted_key("thm:first")
    );
    assert!(!directory.path().join(".mls/Shadow").exists());
}

#[test]
fn reject_then_edit_follow_the_review_rules() {
    let directory = reviewed_directory();
    let reject = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "reject",
            "--output",
            "reject.json",
        ],
    );
    assert_eq!(reject.status.code(), Some(0), "{}", stderr(&reject));
    let rejected = read_json(&directory.path().join("reject.json"));
    let first = view_block(&rejected, "thm:first");
    assert_eq!(first["status"], "not_formalized");
    assert_eq!(first["review"], "rejected");
    assert_eq!(first["needs_rerun"], true);
    assert_eq!(first["previously_rejected"], true);
    assert_eq!(rejected["needs_rerun"], true);

    fs::write(
        directory.path().join("good.json"),
        sir_for("thm:first", &[])
            .to_string()
            .replace("truth", "edited truth"),
    )
    .unwrap();
    let edit = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "edit",
            "--sir",
            "good.json",
            "--output",
            "edit.json",
        ],
    );
    assert_eq!(edit.status.code(), Some(0), "{}", stderr(&edit));
    let edited = read_json(&directory.path().join("edit.json"));
    let first = view_block(&edited, "thm:first");
    assert_eq!(first["status"], "sir_drafted");
    assert_eq!(first["review"], "edited");
    assert_eq!(first["needs_rerun"], true);
    assert!(first["lean_statement"].is_null());
    assert_eq!(first["sir"]["conclusion"], "edited truth");
}

#[test]
fn invalid_edits_are_refused_and_leave_the_store_unchanged() {
    let directory = reviewed_directory();
    let seed = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "reject",
            "--output",
            "seed.json",
        ],
    );
    assert!(seed.status.success(), "{}", stderr(&seed));
    let before = fs::read(store_path(directory.path())).unwrap();

    let wrong_kind = sir_for("thm:first", &[])
        .to_string()
        .replace("\"theorem\"", "\"lemma\"");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("malformed.json", b"{\"kind\":".to_vec()),
        ("kind.json", wrong_kind.into_bytes()),
        ("oversized.json", vec![b' '; 300 * 1024]),
    ];
    for (index, (name, bytes)) in cases.into_iter().enumerate() {
        fs::write(directory.path().join(name), bytes).unwrap();
        let output_name = format!("bad{index}.json");
        let output = run_mls(
            directory.path(),
            &[
                "review-action",
                "notes.tm",
                "--block",
                &block_id(1),
                "--action",
                "edit",
                "--sir",
                name,
                "--output",
                &output_name,
            ],
        );
        assert_refused(
            &output,
            &directory.path().join(&output_name),
            2,
            "invalid_sir",
        );
        assert_eq!(
            fs::read(store_path(directory.path())).unwrap(),
            before,
            "{name}"
        );
    }

    let missing = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "edit",
            "--sir",
            "absent.json",
            "--output",
            "missing.json",
        ],
    );
    assert_refused(
        &missing,
        &directory.path().join("missing.json"),
        2,
        "invalid_sir",
    );
    assert_eq!(fs::read(store_path(directory.path())).unwrap(), before);

    let sir_without_edit = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--sir",
            "malformed.json",
            "--output",
            "misuse.json",
        ],
    );
    assert_eq!(sir_without_edit.status.code(), Some(2));
    assert!(!directory.path().join("misuse.json").exists());
    assert_eq!(fs::read(store_path(directory.path())).unwrap(), before);
}

#[test]
fn unknown_blocks_and_stale_reports_are_refused() {
    let directory = reviewed_directory();
    let unknown = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(9),
            "--action",
            "reject",
            "--output",
            "unknown.json",
        ],
    );
    assert_refused(
        &unknown,
        &directory.path().join("unknown.json"),
        2,
        "unknown_block",
    );

    fs::write(
        directory.path().join("notes.tm"),
        DOCUMENT.replace("Truth holds.", "Truth holds, always."),
    )
    .unwrap();
    let stale = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--output",
            "stale.json",
        ],
    );
    assert_refused(
        &stale,
        &directory.path().join("stale.json"),
        2,
        "stale_report",
    );
    assert!(!store_path(directory.path()).exists());
}

#[test]
fn review_action_refuses_a_block_that_the_last_run_does_not_list() {
    let directory = reviewed_directory();
    let added = DOCUMENT.replace(
        "</body>",
        "  <\\with|mls-id|00000000-0000-4000-8000-000000000003><\\theorem>New claim.<label|thm:new></theorem></with>\n</body>",
    );
    fs::write(directory.path().join("notes.tm"), added).unwrap();
    let output = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(3),
            "--action",
            "confirm",
            "--output",
            "new.json",
        ],
    );
    assert_refused(
        &output,
        &directory.path().join("new.json"),
        2,
        "stale_report",
    );
}

#[test]
fn a_confirmed_block_waits_for_its_unconfirmed_dependency_and_then_settles() {
    let directory = reviewed_directory();
    let seed = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(2),
            "--action",
            "confirm",
            "--output",
            "seed.json",
        ],
    );
    assert!(seed.status.success(), "{}", stderr(&seed));
    let waiting = read_json(&directory.path().join("seed.json"));
    let second = view_block(&waiting, "thm:second");
    assert_eq!(second["status"], "sir_drafted");
    assert_eq!(second["review"], "confirmed");
    assert_eq!(second["needs_rerun"], false);
    assert!(
        second["message"]
            .as_str()
            .unwrap()
            .contains("waiting for dependency confirmation")
    );
    assert_eq!(view_block(&waiting, "thm:first")["status"], "sir_drafted");
    // Its own SIR is confirmed and accepted, so its Lean is cached although it still waits.
    let store: Value = read_json(&store_path(directory.path()));
    assert_eq!(
        store["lean_cache"][block_id(2)]["cache_key"],
        crafted_key("thm:second")
    );

    let confirm = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--output",
            "both.json",
        ],
    );
    assert!(confirm.status.success(), "{}", stderr(&confirm));
    assert!(!stderr(&confirm).contains("Model call"));
    let settled = read_json(&directory.path().join("both.json"));
    assert_eq!(
        view_block(&settled, "thm:first")["status"],
        "statement_confirmed"
    );
    assert_eq!(
        view_block(&settled, "thm:second")["status"],
        "statement_confirmed"
    );
    assert_eq!(settled["needs_rerun"], false);
    assert_eq!(settled["counts"]["statement_confirmed"], 2);
}

#[test]
fn review_run_replays_into_a_view_report_store_and_shadow() {
    if env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        directory.path().join("notes.tm"),
    )
    .unwrap();
    let output = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            fixture("recordings/pipeline/success").to_str().unwrap(),
            "--output",
            "view.json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let view = read_json(&directory.path().join("view.json"));
    assert_eq!(view["protocol"], 1);
    assert_eq!(view["counts"]["sir_drafted"], 2);
    assert_eq!(view["needs_rerun"], false);
    assert!(
        directory
            .path()
            .join(".mls/review/notes.tm.report.json")
            .is_file()
    );
    assert!(store_path(directory.path()).is_file());
    let shadow = module_directory(directory.path());
    let module = shadow.file_name().unwrap().to_string_lossy().into_owned();
    for name in ["Section1.lean", "Section2.lean"] {
        let actual = fs::read_to_string(shadow.join(name)).unwrap();
        assert_eq!(
            actual.replace(&format!("Shadow.{module}."), "Shadow."),
            fs::read_to_string(fixture(&format!("lean/pipeline/{name}"))).unwrap(),
            "{name}"
        );
    }
}

/// The only document module directory under `.mls/Shadow`.
fn module_directory(project: &Path) -> PathBuf {
    let mut directories: Vec<PathBuf> = fs::read_dir(project.join(".mls/Shadow"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(directories.len(), 1, "{directories:?}");
    directories.pop().unwrap()
}

#[test]
fn two_documents_in_one_directory_both_review_run() {
    if env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        directory.path().join("notes.tm"),
    )
    .unwrap();
    fs::copy(
        fixture("lean/pipeline/dependency.tm"),
        directory.path().join("other.tm"),
    )
    .unwrap();
    for (document, recording, output) in [
        ("notes.tm", "recordings/pipeline/success", "notes-view.json"),
        (
            "other.tm",
            "recordings/pipeline/dependency",
            "other-view.json",
        ),
    ] {
        let run = run_mls(
            directory.path(),
            &[
                "review-run",
                document,
                "--replay",
                fixture(recording).to_str().unwrap(),
                "--output",
                output,
            ],
        );
        assert_eq!(run.status.code(), Some(0), "{document}: {}", stderr(&run));
        let view = read_json(&directory.path().join(output));
        assert_eq!(view["needs_rerun"], false, "{document}");
        assert!(
            view["counts"]["sir_drafted"].as_u64().unwrap() >= 1,
            "{document}"
        );
    }
    assert_eq!(
        fs::read_dir(directory.path().join(".mls/Shadow"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().is_dir())
            .count(),
        2
    );
    assert!(store_path(directory.path()).is_file());
    assert!(
        directory
            .path()
            .join(".mls/review/other.tm.store.json")
            .is_file()
    );
}

#[test]
fn confirmed_blocks_rerun_without_model_calls_and_reuse_the_lean_cache() {
    if env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        directory.path().join("notes.tm"),
    )
    .unwrap();
    let first = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            fixture("recordings/pipeline/success").to_str().unwrap(),
            "--output",
            "first.json",
        ],
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    for (number, name) in [(1, "confirm-1.json"), (2, "confirm-2.json")] {
        let confirm = run_mls(
            directory.path(),
            &[
                "review-action",
                "notes.tm",
                "--block",
                &block_id(number),
                "--action",
                "confirm",
                "--output",
                name,
            ],
        );
        assert_eq!(confirm.status.code(), Some(0), "{}", stderr(&confirm));
    }
    empty_replay(directory.path());
    let rerun = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            directory.path().join("empty-replay").to_str().unwrap(),
            "--output",
            "rerun.json",
        ],
    );
    assert_eq!(rerun.status.code(), Some(0), "{}", stderr(&rerun));
    let view = read_json(&directory.path().join("rerun.json"));
    assert_eq!(view["counts"]["statement_confirmed"], 2);
    assert_eq!(view["needs_rerun"], false);
    let report = read_json(&directory.path().join(".mls/review/notes.tm.report.json"));
    assert_eq!(report["usage"]["calls"], 0);
    assert!(
        report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["lean_origin"] == "cache")
    );
}

#[test]
fn a_rejected_block_is_regenerated_with_the_rejection_as_a_note() {
    if env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the pinned local runtime");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        fixture("lean/pipeline/success.tm"),
        directory.path().join("notes.tm"),
    )
    .unwrap();
    let first = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            fixture("recordings/pipeline/success").to_str().unwrap(),
            "--output",
            "first.json",
        ],
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let reject = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "reject",
            "--output",
            "reject.json",
        ],
    );
    assert_eq!(reject.status.code(), Some(0), "{}", stderr(&reject));
    empty_replay(directory.path());
    let rerun = run_mls(
        directory.path(),
        &[
            "review-run",
            "notes.tm",
            "--replay",
            directory.path().join("empty-replay").to_str().unwrap(),
            "--output",
            "rerun.json",
        ],
    );
    assert_eq!(rerun.status.code(), Some(0), "{}", stderr(&rerun));
    let view = read_json(&directory.path().join("rerun.json"));
    let first_block = view_block(&view, "thm:first");
    assert_eq!(first_block["status"], "not_formalized");
    assert_eq!(first_block["review"], "rejected");
    assert_eq!(first_block["previously_rejected"], true);
    assert!(
        first_block["errors"][0]
            .as_str()
            .unwrap()
            .contains("no exact replay match")
    );
}

fn empty_replay(directory: &Path) {
    fs::create_dir_all(directory.join("empty-replay")).unwrap();
}

/// Rewrites the stored report of `notes.tm` after applying `edit` to its JSON.
fn edit_report(directory: &Path, edit: impl FnOnce(&mut Value)) {
    let path = directory.join(".mls/review/notes.tm.report.json");
    let mut report: Value = read_json(&path);
    edit(&mut report);
    fs::write(path, report.to_string()).unwrap();
}

#[test]
fn a_hand_edited_report_is_refused_as_invalid_report() {
    let directory = reviewed_directory();
    edit_report(directory.path(), |report| {
        report["blocks"][0]["sir_hash"] = json!("0".repeat(64));
    });
    let status = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(
        &status,
        &directory.path().join("status.json"),
        2,
        "invalid_report",
    );
    assert!(stderr(&status).contains("sir_hash"), "{}", stderr(&status));

    let directory = reviewed_directory();
    edit_report(directory.path(), |report| {
        report["blocks"][1]["cache_key"] = json!("NOT-A-SHA256-KEY");
    });
    let status = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(
        &status,
        &directory.path().join("status.json"),
        2,
        "invalid_report",
    );

    let directory = reviewed_directory();
    edit_report(directory.path(), |report| {
        report["blocks"][0]["sir"] = Value::Null;
    });
    let action = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "confirm",
            "--output",
            "act.json",
        ],
    );
    assert_refused(
        &action,
        &directory.path().join("act.json"),
        2,
        "invalid_report",
    );
    assert!(!store_path(directory.path()).exists());
}

#[test]
fn review_status_refuses_a_symlinked_review_directory() {
    let directory = reviewed_directory();
    let target = tempfile::tempdir().unwrap();
    fs::copy(
        directory.path().join(".mls/review/notes.tm.report.json"),
        target.path().join("notes.tm.report.json"),
    )
    .unwrap();
    fs::remove_dir_all(directory.path().join(".mls/review")).unwrap();
    std::os::unix::fs::symlink(target.path(), directory.path().join(".mls/review")).unwrap();

    let status = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(&status, &directory.path().join("status.json"), 2, "refused");
    assert_eq!(
        fs::read_dir(target.path()).unwrap().count(),
        1,
        "the symlink target was touched"
    );
}

#[test]
fn review_status_refuses_symlinked_store_and_report_files() {
    let outside = tempfile::tempdir().unwrap();

    let directory = reviewed_directory();
    let seed = run_mls(
        directory.path(),
        &[
            "review-action",
            "notes.tm",
            "--block",
            &block_id(1),
            "--action",
            "reject",
            "--output",
            "seed.json",
        ],
    );
    assert!(seed.status.success(), "{}", stderr(&seed));
    let copy = outside.path().join("store.json");
    fs::copy(store_path(directory.path()), &copy).unwrap();
    fs::remove_file(store_path(directory.path())).unwrap();
    std::os::unix::fs::symlink(&copy, store_path(directory.path())).unwrap();
    let status = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(&status, &directory.path().join("status.json"), 2, "refused");

    let directory = reviewed_directory();
    let report = directory.path().join(".mls/review/notes.tm.report.json");
    let copy = outside.path().join("report.json");
    fs::copy(&report, &copy).unwrap();
    fs::remove_file(&report).unwrap();
    std::os::unix::fs::symlink(&copy, &report).unwrap();
    let status = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_refused(&status, &directory.path().join("status.json"), 2, "refused");
}

#[test]
fn an_existing_output_is_kept_and_no_error_object_is_written_over_it() {
    let directory = reviewed_directory();
    fs::write(directory.path().join("status.json"), b"previous result").unwrap();
    let output = run_mls(
        directory.path(),
        &["review-status", "notes.tm", "--output", "status.json"],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("output exists; no error object written"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read(directory.path().join("status.json")).unwrap(),
        b"previous result"
    );
}
