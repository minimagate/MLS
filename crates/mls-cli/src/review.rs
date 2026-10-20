//! The review commands: `review-run`, `review-action` and `review-status`.
//!
//! Each command writes one JSON document to `--output`: the review view on success, or the error
//! object of `docs/protocol.md` on failure. Only `review-run` may call a model or start the Lean
//! worker. Review state lives beside the document in `<document dir>/.mls/review/`.

use crate::{
    PROJECT_OWNER, Transport, absolute_path, atomic_create, atomic_replace_owned, build_transport,
    ensure_output_parent, ensure_owned_dir, ensure_plain_dir_if_present, ensure_project_marker,
    ensure_regular_if_present, ensure_regular_owned, initialize_project, is_occupied,
    output_exists, preflight_batch, project_directory, reject_output_alias,
    reject_output_in_mls_directories, spawn_worker, store_report, workspace_root, write_shadow,
};
use mls_core::{Block, BlockId};
use mls_engine::{
    BatchReport, BlockReport, LeanStatementChecker, PipelineConfig, ReviewEntry, ReviewError,
    ReviewStore, UsageSummary, formal_block, lacks_persisted_ids, review_view, run_batch_reviewed,
};
use mls_models::{ModelConfig, RecordingClient};
use mls_tm::{Analysis, analyze, parse};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
};

const REVIEW_DIRECTORY: &str = "review";
const SIR_LIMIT_BYTES: u64 = 256 * 1024;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const RUN_VALUED: &[&str] = &["--replay", "--model-config", "--output", "--max-attempts"];
const ACTION_VALUED: &[&str] = &["--block", "--action", "--sir", "--output"];
const STATUS_VALUED: &[&str] = &["--output"];

/// A failed review command: a stable code for the error object and the exit status.
#[derive(Debug)]
struct Refusal {
    code: &'static str,
    exit: u8,
    message: String,
}

impl Refusal {
    fn new(code: &'static str, exit: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            exit,
            message: message.into(),
        }
    }

    /// A failure of the environment or of MLS itself.
    fn failed(error: impl std::fmt::Display) -> Self {
        Self::new("failed", 1, error.to_string())
    }

    /// A request that the arguments or the current state refuse.
    fn refused(error: impl std::fmt::Display) -> Self {
        Self::new("refused", 2, error.to_string())
    }

    /// Reports the failure on stderr, and as the error object when `output` is known to be writable.
    fn exit(self, output: Option<&Path>) -> ExitCode {
        eprintln!("mls: {}", self.message);
        if let Some(output) = output {
            let body = json!({"protocol": 1, "error": self.code, "message": self.message});
            let bytes = serde_json::to_vec_pretty(&body).expect("a JSON value always serializes");
            if let Err(error) = atomic_create(output, &bytes) {
                eprintln!(
                    "mls: could not write the error object to {}: {error}",
                    output.display()
                );
            }
        }
        ExitCode::from(self.exit)
    }
}

/// Whether `name` is a command handled by this module.
pub(crate) fn is_review_command(name: &str) -> bool {
    matches!(name, "review-run" | "review-action" | "review-status")
}

/// Runs one review command and returns its exit status.
pub(crate) fn dispatch(command: &str, arguments: &[OsString]) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print_help();
        return ExitCode::SUCCESS;
    }
    let mut output = None;
    let result = match command {
        "review-run" => review_run(arguments, &mut output),
        "review-action" => review_action(arguments, &mut output),
        _ => review_status(arguments, &mut output),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => refusal.exit(output.as_deref()),
    }
}

/// Prints the usage of the review commands.
pub(crate) fn print_help() {
    println!(
        "Review commands (each writes one JSON document to --output):\n  mls review-run FILE.tm (--replay DIR | --live) --output OUT.json [--model-config FILE] [--max-attempts N]\n  mls review-action FILE.tm --block UUID --action confirm|reject|edit [--sir FILE.json] --output OUT.json\n  mls review-status FILE.tm --output OUT.json\n\nLive review runs also require MLS_LIVE=1. A failure writes {{\"protocol\":1,\"error\":CODE,\"message\":...}} when the output is writable."
    );
}

fn review_run(arguments: &[OsString], output: &mut Option<PathBuf>) -> Result<(), Refusal> {
    let parsed = parse_arguments(arguments, RUN_VALUED, &["--live"])?;
    let live = parsed.flag("--live");
    let replay = parsed.path("--replay");
    if live == replay.is_some() {
        return Err(Refusal::refused(
            "choose exactly one of --replay DIR or --live; review-run never starts live implicitly",
        ));
    }
    if live && env::var("MLS_LIVE").as_deref() != Ok("1") {
        return Err(Refusal::refused("--live requires MLS_LIVE=1"));
    }
    let max_attempts = max_attempts(&parsed)?;
    let model_config = absolute_path(
        &parsed
            .path("--model-config")
            .unwrap_or_else(|| workspace_root().join("config/models.json")),
    )
    .map_err(Refusal::failed)?;
    let (document, written) = open_session(&parsed.document, &parsed.output()?)?;
    *output = Some(written.clone());

    let (source, analysis) = load_source(&document)?;
    if lacks_persisted_ids(&analysis) {
        return Err(Refusal::new(
            "missing_ids",
            3,
            "formal blocks lack persisted IDs; run mls --write-ids on the document, reload it and retry",
        ));
    }
    let models = ModelConfig::from_path(&model_config).map_err(|error| {
        Refusal::failed(format!(
            "cannot load model config {}: {error}",
            model_config.display()
        ))
    })?;
    let transport = build_transport(replay, &models).map_err(Refusal::failed)?;
    preflight_batch(&document, live).map_err(Refusal::failed)?;
    preflight_review(&document).map_err(Refusal::failed)?;
    let store = load_store(&document)?;
    let project = initialize_project(&document).map_err(Refusal::failed)?;
    let mut worker = spawn_worker(&project).map_err(Refusal::failed)?;
    let mut checker = LeanStatementChecker::new(&mut worker);
    let pipeline = PipelineConfig {
        max_attempts,
        formalization_max_tokens: models.default_max_tokens,
    };
    let (report, persistence) = match transport {
        Transport::Replay(mut client) => (
            run_batch_reviewed(
                &source,
                &analysis,
                &mut client,
                &mut checker,
                pipeline,
                &store,
            )
            .map_err(Refusal::failed)?,
            None,
        ),
        Transport::Live(client) => {
            let log_root = project.join("log");
            ensure_owned_dir(&log_root).map_err(Refusal::failed)?;
            let record_dir = log_root.join(format!("batch-{}", BlockId::new()));
            eprintln!("Model call records: {}", record_dir.display());
            let mut client = RecordingClient::new(
                client,
                record_dir,
                &models.generation_model,
                &models.decision_model,
                "live",
            );
            let report = run_batch_reviewed(
                &source,
                &analysis,
                &mut client,
                &mut checker,
                pipeline,
                &store,
            )
            .map_err(Refusal::failed)?;
            (report, client.persistence_failure())
        }
    };

    let mut store = store;
    reconcile(&analysis, &report, &mut store)?;
    let view = review_view(&document.to_string_lossy(), &analysis, &report, &store);
    let report_bytes = serde_json::to_vec_pretty(&report).map_err(Refusal::failed)?;
    let view_bytes = serde_json::to_vec_pretty(&view).map_err(Refusal::failed)?;
    // The report and the store are kept before the shadow project, and the view is written last.
    let state = persist_state(&document, &report_bytes, &store);
    let shadow = write_shadow(&document, &analysis, &report);
    if let Some(detail) = persistence {
        if let Err(error) = &state {
            eprintln!("mls: {error}");
        }
        if let Err(error) = &shadow {
            eprintln!("mls: shadow project was not written: {error}");
        }
        return Err(Refusal::new(
            "recording_incomplete",
            1,
            format!(
                "model call recording is incomplete and not replayable ({detail}); the review state was kept, read it with review-status"
            ),
        ));
    }
    state.map_err(Refusal::failed)?;
    shadow.map_err(Refusal::failed)?;
    store_report(Some(&written), &view_bytes, &mut std::io::stdout().lock())
        .map_err(Refusal::failed)
}

fn review_action(arguments: &[OsString], output: &mut Option<PathBuf>) -> Result<(), Refusal> {
    let parsed = parse_arguments(arguments, ACTION_VALUED, &[])?;
    let action = parsed.text("--action")?;
    if !matches!(action.as_str(), "confirm" | "reject" | "edit") {
        return Err(Refusal::refused("--action must be confirm, reject or edit"));
    }
    let sir_path = parsed.path("--sir");
    if (action == "edit") != sir_path.is_some() {
        return Err(Refusal::refused(
            "--sir FILE is required with edit and only with edit",
        ));
    }
    let block_text = parsed.text("--block")?;
    let (document, written) = open_session(&parsed.document, &parsed.output()?)?;
    *output = Some(written.clone());

    let (_, analysis) = load_source(&document)?;
    ensure_owned_project(&document)?;
    let report = load_report(&document)?.ok_or_else(|| {
        Refusal::new(
            "no_report",
            2,
            "this document has no review report; run review-run first",
        )
    })?;
    let mut store = load_store(&document)?;
    let block = block_text
        .parse::<BlockId>()
        .ok()
        .and_then(|id| formal_block(&analysis, id))
        .ok_or_else(|| {
            Refusal::new(
                "unknown_block",
                2,
                format!("{block_text} is not a formal block of this document"),
            )
        })?;
    match action.as_str() {
        "confirm" => {
            let block_report = find_report(&report, block.id)?;
            store.confirm(block, block_report).map_err(review_failure)?;
            cache_accepted_lean(&mut store, block, block_report)?;
        }
        "reject" => {
            let block_report = find_report(&report, block.id)?;
            store.reject(block, block_report).map_err(review_failure)?;
        }
        _ => {
            let path =
                sir_path.ok_or_else(|| Refusal::refused("--sir FILE is required with edit"))?;
            let sir_json = read_sir(&path)?;
            let uses = expected_uses(&analysis, block);
            store
                .edit(block, &uses, &sir_json)
                .map_err(review_failure)?;
        }
    }
    persist_store(&document, &store).map_err(Refusal::failed)?;
    let view = review_view(&document.to_string_lossy(), &analysis, &report, &store);
    let bytes = serde_json::to_vec_pretty(&view).map_err(Refusal::failed)?;
    store_report(Some(&written), &bytes, &mut std::io::stdout().lock()).map_err(Refusal::failed)
}

fn review_status(arguments: &[OsString], output: &mut Option<PathBuf>) -> Result<(), Refusal> {
    let parsed = parse_arguments(arguments, STATUS_VALUED, &[])?;
    let (document, written) = open_session(&parsed.document, &parsed.output()?)?;
    *output = Some(written.clone());

    let (_, analysis) = load_source(&document)?;
    ensure_owned_project(&document)?;
    let report = load_report(&document)?.unwrap_or_else(|| BatchReport {
        diagnostics: Vec::new(),
        blocks: Vec::new(),
        usage: UsageSummary::default(),
    });
    let store = load_store(&document)?;
    let view = review_view(&document.to_string_lossy(), &analysis, &report, &store);
    let bytes = serde_json::to_vec_pretty(&view).map_err(Refusal::failed)?;
    store_report(Some(&written), &bytes, &mut std::io::stdout().lock()).map_err(Refusal::failed)
}

/// Resolves the document and checks the output, before any other side effect.
fn open_session(document: &Path, output: &Path) -> Result<(PathBuf, PathBuf), Refusal> {
    if document
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("tm")
    {
        return Err(Refusal::refused(format!(
            "{} is not a .tm document",
            document.display()
        )));
    }
    let document = document.canonicalize().map_err(|error| {
        Refusal::refused(format!(
            "cannot resolve document {}: {error}",
            document.display()
        ))
    })?;
    let output = absolute_path(output).map_err(Refusal::failed)?;
    reject_output_alias(&document, &output).map_err(Refusal::refused)?;
    // An existing output is never replaced, so no error object is written over it either.
    if output_exists(&output) {
        return Err(Refusal::refused(format!(
            "output exists; no error object written: {} already exists",
            output.display()
        )));
    }
    reject_output_in_mls_directories(&document, &output).map_err(Refusal::refused)?;
    ensure_output_parent(&output).map_err(Refusal::refused)?;
    Ok((document, output))
}

/// Reads the source and analyzes it. Invalid or duplicate identities are refused.
fn load_source(document: &Path) -> Result<(Vec<u8>, Analysis), Refusal> {
    let source = fs::read(document).map_err(Refusal::failed)?;
    let analysis = analyze(
        &parse(&source).map_err(|error| Refusal::new("invalid_document", 2, error.to_string()))?,
    );
    if analysis
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.code.as_str(), "duplicate_id" | "invalid_id"))
    {
        return Err(Refusal::new(
            "invalid_ids",
            2,
            "identity metadata is invalid or duplicated; fix it before reviewing",
        ));
    }
    Ok((source, analysis))
}

fn parse_arguments(
    arguments: &[OsString],
    valued: &[&'static str],
    flags: &[&'static str],
) -> Result<Arguments, Refusal> {
    let mut document = None;
    let mut values = BTreeMap::new();
    let mut set = BTreeSet::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let text = argument.to_string_lossy();
        if let Some(name) = valued.iter().copied().find(|name| text.as_ref() == *name) {
            let value = rest
                .next()
                .ok_or_else(|| Refusal::refused(format!("{name} requires a value")))?;
            if values.insert(name, value.clone()).is_some() {
                return Err(Refusal::refused(format!("{name} is given twice")));
            }
        } else if let Some(name) = flags.iter().copied().find(|name| text.as_ref() == *name) {
            set.insert(name);
        } else if text.starts_with('-') {
            return Err(Refusal::refused(format!("unknown option {text}")));
        } else if document.replace(PathBuf::from(argument)).is_some() {
            return Err(Refusal::refused("expected exactly one .tm file"));
        }
    }
    let document = document.ok_or_else(|| Refusal::refused("missing FILE.tm"))?;
    Ok(Arguments {
        document,
        values,
        flags: set,
    })
}

/// Parsed command-line arguments of one review command.
struct Arguments {
    document: PathBuf,
    values: BTreeMap<&'static str, OsString>,
    flags: BTreeSet<&'static str>,
}

impl Arguments {
    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.values.get(name).map(PathBuf::from)
    }

    fn text(&self, name: &str) -> Result<String, Refusal> {
        self.values
            .get(name)
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| Refusal::refused(format!("{name} is required")))
    }

    fn output(&self) -> Result<PathBuf, Refusal> {
        self.path("--output")
            .ok_or_else(|| Refusal::refused("--output is required"))
    }
}

fn max_attempts(parsed: &Arguments) -> Result<u32, Refusal> {
    match parsed.values.get("--max-attempts") {
        None => Ok(DEFAULT_MAX_ATTEMPTS),
        Some(value) => value
            .to_string_lossy()
            .parse::<u32>()
            .ok()
            .filter(|attempts| *attempts > 0)
            .ok_or_else(|| Refusal::refused("--max-attempts must be a positive integer")),
    }
}

/// Every command that reads review state goes through this check first. A project that exists must
/// carry MLS's ownership marker, and its review directory must be a real directory, not a symlink
/// that would send reads and writes elsewhere. The files inside are checked where they are read.
fn ensure_owned_project(document: &Path) -> Result<(), Refusal> {
    let project = project_directory(document);
    if is_occupied(&project).map_err(Refusal::failed)? {
        ensure_plain_dir_if_present(&project).map_err(Refusal::refused)?;
        ensure_project_marker(&project.join(PROJECT_OWNER)).map_err(|_| {
            Refusal::refused(format!(
                "refusing unowned existing project {}",
                project.display()
            ))
        })?;
    }
    ensure_plain_dir_if_present(&review_directory(document)).map_err(Refusal::refused)?;
    Ok(())
}

fn review_directory(document: &Path) -> PathBuf {
    project_directory(document).join(REVIEW_DIRECTORY)
}

/// The last report and the decision store of a document, by file name.
fn review_paths(document: &Path) -> (PathBuf, PathBuf) {
    let name = document
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let directory = review_directory(document);
    (
        directory.join(format!("{name}.report.json")),
        directory.join(format!("{name}.store.json")),
    )
}

/// Read-only preflight for `review-run`, so that every refusal happens before any side effect.
fn preflight_review(document: &Path) -> Result<(), Box<dyn std::error::Error>> {
    ensure_plain_dir_if_present(&review_directory(document))?;
    let (report, store) = review_paths(document);
    ensure_regular_if_present(&report)?;
    ensure_regular_if_present(&store)
}

fn load_store(document: &Path) -> Result<ReviewStore, Refusal> {
    let (_, path) = review_paths(document);
    if !is_occupied(&path).map_err(Refusal::failed)? {
        return Ok(ReviewStore::new());
    }
    ensure_regular_owned(&path).map_err(Refusal::refused)?;
    let text = fs::read_to_string(&path).map_err(Refusal::failed)?;
    ReviewStore::from_json(&text).map_err(review_failure)
}

/// Reads the last report. Its SIRs must match their stored hashes and its cache keys must be
/// SHA-256 hex, because the view trusts both; anything else is an `invalid_report` refusal.
fn load_report(document: &Path) -> Result<Option<BatchReport>, Refusal> {
    let (path, _) = review_paths(document);
    if !is_occupied(&path).map_err(Refusal::failed)? {
        return Ok(None);
    }
    ensure_regular_owned(&path).map_err(Refusal::refused)?;
    let bytes = fs::read(&path).map_err(Refusal::failed)?;
    let report: BatchReport = serde_json::from_slice(&bytes).map_err(|error| {
        Refusal::new(
            "invalid_report",
            2,
            format!("the stored report is unreadable: {error}"),
        )
    })?;
    validate_report(&report)?;
    Ok(Some(report))
}

/// Checks the report fields that the view relies on. Each block's `sir_hash` must be the semantic
/// hash of its `sir` (both absent together), and every `cache_key` must be a SHA-256 hex digest.
fn validate_report(report: &BatchReport) -> Result<(), Refusal> {
    let refuse = |message: String| Refusal::new("invalid_report", 2, message);
    for block in &report.blocks {
        let hashes_agree = match (&block.sir, &block.sir_hash) {
            (Some(sir), Some(hash)) => *hash == sir.semantic_hash(),
            (None, None) => true,
            _ => false,
        };
        if !hashes_agree {
            return Err(refuse(format!(
                "the stored report is invalid: block {} has a sir_hash that does not match its SIR",
                block.block_id
            )));
        }
        if let Some(key) = &block.cache_key
            && !is_sha256_hex(key)
        {
            return Err(refuse(format!(
                "the stored report is invalid: block {} has a malformed cache_key",
                block.block_id
            )));
        }
    }
    Ok(())
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Writes the report and the store, each atomically and only inside the owned review directory.
fn persist_state(
    document: &Path,
    report: &[u8],
    store: &ReviewStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let (report_path, _) = review_paths(document);
    ensure_owned_dir(&review_directory(document))?;
    atomic_replace_owned(&report_path, report)?;
    persist_store(document, store)
}

fn persist_store(document: &Path, store: &ReviewStore) -> Result<(), Box<dyn std::error::Error>> {
    let (_, store_path) = review_paths(document);
    ensure_owned_dir(&review_directory(document))?;
    atomic_replace_owned(&store_path, store.to_json().as_bytes())
}

// The daemon uses exactly the same decision store as the review commands. Keep Refusal private
// to this command's exit-code protocol.
pub(super) fn serve_load_store(document: &Path) -> Result<ReviewStore, String> {
    preflight_review(document).map_err(|error| error.to_string())?;
    load_store(document).map_err(|error| error.message)
}

pub(super) fn serve_persist_store(document: &Path, store: &ReviewStore) -> Result<(), String> {
    persist_store(document, store).map_err(|error| error.to_string())
}

/// Caches the Lean declaration accepted for a block's SIR, under the report's section 4 key, when
/// that SIR is in effect in the store as a confirmation or edit. A draft, a rejected SIR, a
/// candidate that was not accepted, or a report without a real key is never cached. Dependency
/// confirmation is not required: the key already binds the SIRs of the dependencies.
fn cache_accepted_lean(
    store: &mut ReviewStore,
    block: &Block,
    report: &BlockReport,
) -> Result<(), Refusal> {
    let in_effect = matches!(
        store.entry(block.id),
        Some(ReviewEntry::Confirmed { sir_hash, .. }) if Some(sir_hash) == report.sir_hash.as_ref()
    );
    if report.block_id != block.id || !in_effect || !report.accepted_speculatively {
        return Ok(());
    }
    let (Some(cache_key), Some(declaration)) = (&report.cache_key, &report.lean_statement) else {
        return Ok(());
    };
    store
        .record_accepted_lean(block.id, cache_key.clone(), declaration.clone())
        .map_err(review_failure)
}

/// Applies the side effects of a finished run to the store: confirmations follow cosmetic edits,
/// accepted Lean declarations for confirmed SIRs are cached, and removed blocks are forgotten.
fn reconcile(
    analysis: &Analysis,
    report: &BatchReport,
    store: &mut ReviewStore,
) -> Result<(), Refusal> {
    let blocks: HashMap<BlockId, &Block> = analysis
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();
    for block_report in &report.blocks {
        let Some(block) = blocks.get(&block_report.block_id).copied() else {
            continue;
        };
        // Only a run whose own SIR is human-confirmed or edited may cache anything.
        if !matches!(
            block_report.sir_origin.as_deref(),
            Some("confirmed" | "edited")
        ) {
            continue;
        }
        store.anchor_text(block, block_report);
        cache_accepted_lean(store, block, block_report)?;
    }
    let present: BTreeSet<BlockId> = analysis.blocks.iter().map(|block| block.id).collect();
    for id in store.block_ids() {
        if !present.contains(&id) {
            store.forget(id);
        }
    }
    Ok(())
}

fn find_report(report: &BatchReport, id: BlockId) -> Result<&BlockReport, Refusal> {
    report
        .blocks
        .iter()
        .find(|block| block.block_id == id)
        .ok_or_else(|| {
            Refusal::new(
                "stale_report",
                2,
                "this block is not in the last run; run review-run first",
            )
        })
}

fn expected_uses(analysis: &Analysis, block: &Block) -> Vec<String> {
    let labels: HashMap<BlockId, &String> = analysis
        .blocks
        .iter()
        .filter_map(|candidate| candidate.label.as_ref().map(|label| (candidate.id, label)))
        .collect();
    block
        .deps
        .iter()
        .filter_map(|dependency| labels.get(dependency).map(|label| (*label).clone()))
        .collect()
}

/// Reads a SIR file of at most 256 KiB as UTF-8. The JSON itself is validated by the store.
fn read_sir(path: &Path) -> Result<String, Refusal> {
    let refuse = |reason: String| Refusal::new("invalid_sir", 2, reason);
    let metadata = fs::metadata(path)
        .map_err(|error| refuse(format!("cannot read --sir {}: {error}", path.display())))?;
    if !metadata.is_file() || metadata.len() > SIR_LIMIT_BYTES {
        return Err(refuse(format!(
            "--sir {} must be a regular file of at most 256 KiB",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(SIR_LIMIT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| refuse(format!("cannot read --sir {}: {error}", path.display())))?;
    if bytes.len() as u64 > SIR_LIMIT_BYTES {
        return Err(refuse(format!(
            "--sir {} grew beyond 256 KiB while it was read",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|_| refuse("--sir must be UTF-8 JSON".into()))
}

fn review_failure(error: ReviewError) -> Refusal {
    let (code, exit) = match &error {
        ReviewError::StaleReport { .. } | ReviewError::ReportForOtherBlock { .. } => {
            ("stale_report", 2)
        }
        ReviewError::NoUsableSir { .. } => ("no_usable_sir", 2),
        ReviewError::InvalidSirJson(_) | ReviewError::IdentityMismatch(_) => ("invalid_sir", 2),
        ReviewError::InvalidStore(_) => ("invalid_store", 1),
        ReviewError::NotConfirmed { .. } => ("failed", 1),
    };
    Refusal::new(code, exit, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mls_tm::{analyze, parse};
    use serde_json::{Value, json};

    const SOURCE: &str = r#"<TeXmacs|2.1.4>
<\body>
  <\with|mls-id|00000000-0000-4000-8000-000000000001><\theorem>Truth holds.<label|thm:first></theorem></with>
</body>
"#;

    fn analyzed() -> Analysis {
        analyze(&parse(SOURCE.as_bytes()).unwrap())
    }

    fn sir_json() -> Value {
        json!({
            "kind": "theorem",
            "label": "thm:first",
            "variables": [],
            "hypotheses": [],
            "conclusion": "truth",
            "uses": [],
            "assumptions_made": []
        })
    }

    fn sir_hash(block: &Block) -> String {
        let mut store = ReviewStore::new();
        store.edit(block, &[], &sir_json().to_string()).unwrap();
        let stored: Value = serde_json::from_str(&store.to_json()).unwrap();
        stored["entries"][block.id.to_string()]["sir_hash"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn report(block: &Block, origin: &str, accepted: bool, key: &str, hash: &str) -> BlockReport {
        serde_json::from_value(json!({
            "block_id": block.id.to_string(),
            "label": block.label,
            "source_start": block.source_start,
            "source_end": block.source_end,
            "text_hash": block.text_hash,
            "status": if accepted { "sir_drafted" } else { "formalization_error" },
            "sir": sir_json(),
            "sir_hash": hash,
            "cache_key": key,
            "sir_origin": origin,
            "lean_origin": "generated",
            "cosmetic_reconfirmation": false,
            "superseded_confirmation": null,
            "previously_rejected": false,
            "lean_statement": "theorem mls_first : True := by sorry",
            "elaborated": true,
            "complete_structure": false,
            "accepted_speculatively": accepted,
            "checks": [],
            "attempts": 1,
            "errors": [],
            "lean_diagnostics": []
        }))
        .unwrap()
    }

    #[test]
    fn a_draft_is_never_cached_and_a_confirmation_in_effect_is() {
        let analysis = analyzed();
        let block = analysis.blocks.iter().find(|b| b.label.is_some()).unwrap();
        let key = "a".repeat(64);
        let hash = sir_hash(block);
        let draft = report(block, "generated", true, &key, &hash);

        let mut store = ReviewStore::new();
        cache_accepted_lean(&mut store, block, &draft).unwrap();
        assert!(store.cached_lean(block.id).is_none());

        store.confirm(block, &draft).unwrap();
        cache_accepted_lean(&mut store, block, &draft).unwrap();
        assert_eq!(store.cached_lean(block.id).unwrap().cache_key, key);
    }

    #[test]
    fn a_rejected_candidate_or_a_mismatched_sir_is_not_cached() {
        let analysis = analyzed();
        let block = analysis.blocks.iter().find(|b| b.label.is_some()).unwrap();
        let key = "b".repeat(64);
        let hash = sir_hash(block);

        let mut store = ReviewStore::new();
        let unaccepted = report(block, "confirmed", false, &key, &hash);
        store.confirm(block, &unaccepted).unwrap();
        cache_accepted_lean(&mut store, block, &unaccepted).unwrap();
        assert!(store.cached_lean(block.id).is_none());

        let mut store = ReviewStore::new();
        let other = report(block, "confirmed", true, &key, &"c".repeat(64));
        store
            .confirm(block, &report(block, "confirmed", true, &key, &hash))
            .unwrap();
        cache_accepted_lean(&mut store, block, &other).unwrap();
        assert!(store.cached_lean(block.id).is_none());
    }

    #[test]
    fn reconcile_caches_only_runs_whose_own_sir_is_human_confirmed() {
        let analysis = analyzed();
        let block = analysis.blocks.iter().find(|b| b.label.is_some()).unwrap();
        let key = "d".repeat(64);
        let hash = sir_hash(block);
        let draft = report(block, "generated", true, &key, &hash);
        let mut store = ReviewStore::new();
        store.confirm(block, &draft).unwrap();
        let generated_run = BatchReport {
            diagnostics: Vec::new(),
            blocks: vec![draft.clone()],
            usage: UsageSummary::default(),
        };
        reconcile(&analysis, &generated_run, &mut store).unwrap();
        assert!(store.cached_lean(block.id).is_none());

        let confirmed_run = BatchReport {
            diagnostics: Vec::new(),
            blocks: vec![report(block, "confirmed", true, &key, &hash)],
            usage: UsageSummary::default(),
        };
        reconcile(&analysis, &confirmed_run, &mut store).unwrap();
        assert_eq!(store.cached_lean(block.id).unwrap().cache_key, key);
    }
}
