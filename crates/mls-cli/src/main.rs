//! M1 inspection and offline-by-default M2 batch statement commands.

use mls_engine::{BatchReport, LeanStatementChecker, PipelineConfig, run_batch_analyzed};
use mls_lean::{LeanWorker, ReplConfig, ShadowDeclaration, ShadowSection, generate_shadow_project};
use mls_models::{ModelClient, ModelConfig, OpenRouterClient, RecordingClient, ReplayClient};
use mls_tm::{analyze, parse, write_ids_atomic};
use serde::Serialize;
use std::{
    env,
    error::Error,
    ffi::OsString,
    fmt, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

mod retrieval;
mod review;

#[derive(Serialize)]
struct Output<'a> {
    blocks: &'a [mls_core::Block],
    diagnostics: &'a [mls_core::Diagnostic],
}

#[derive(Debug)]
struct CliError(String);
impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for CliError {}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    if let Some(command) = arguments
        .first()
        .and_then(|argument| argument.to_str())
        .filter(|command| review::is_review_command(command))
    {
        return review::dispatch(command, &arguments[1..]);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mls: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "batch") {
        return batch(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "retrieval-index")
    {
        return retrieval::run(&arguments[1..]);
    }
    inspect(&arguments)
}

fn inspect(arguments: &[std::ffi::OsString]) -> Result<(), Box<dyn Error>> {
    let mut write_ids = false;
    let mut path = None;
    for argument in arguments {
        if argument == "--write-ids" {
            write_ids = true;
        } else if argument == "--help" || argument == "-h" {
            print_help();
            return Ok(());
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(Box::new(CliError(format!(
                "unknown option {}",
                argument.to_string_lossy()
            ))));
        } else if path.replace(PathBuf::from(argument)).is_some() {
            return Err(Box::new(CliError("expected exactly one .tm file".into())));
        }
    }
    let path = path.ok_or_else(|| CliError("Usage: mls [--write-ids] FILE.tm".into()))?;
    let source = fs::read(&path)?;
    let document = parse(&source)?;
    let analysis = analyze(&document);
    if write_ids {
        if analysis
            .diagnostics
            .iter()
            .any(|d| matches!(d.code.as_str(), "duplicate_id" | "invalid_id"))
        {
            return Err(Box::new(CliError(
                "refusing to write IDs while identity metadata is invalid or duplicated".into(),
            )));
        }
        if !analysis.insertions.is_empty() {
            write_ids_atomic(&path, &source, &analysis.insertions)?;
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&Output {
            blocks: &analysis.blocks,
            diagnostics: &analysis.diagnostics
        })?
    );
    Ok(())
}

fn batch(arguments: &[std::ffi::OsString]) -> Result<(), Box<dyn Error>> {
    let mut replay = None;
    let mut live = false;
    let mut model_config = workspace_root().join("config/models.json");
    let mut output = None;
    let mut max_attempts = 3_u32;
    let mut path = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy();
        match argument.as_ref() {
            "--live" => live = true,
            "--replay" | "--model-config" | "--output" | "--max-attempts" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError(format!("{argument} requires a value")))?;
                match argument.as_ref() {
                    "--replay" => replay = Some(PathBuf::from(value)),
                    "--model-config" => model_config = PathBuf::from(value),
                    "--output" => output = Some(PathBuf::from(value)),
                    _ => {
                        max_attempts = value.to_string_lossy().parse().map_err(|_| {
                            CliError("--max-attempts must be a positive integer".into())
                        })?;
                        if max_attempts == 0 {
                            return Err(Box::new(CliError(
                                "--max-attempts must be positive".into(),
                            )));
                        }
                    }
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ if argument.starts_with('-') => {
                return Err(Box::new(CliError(format!("unknown option {argument}"))));
            }
            _ => {
                if path.replace(PathBuf::from(&arguments[index])).is_some() {
                    return Err(Box::new(CliError("expected exactly one .tm file".into())));
                }
            }
        }
        index += 1;
    }
    let path = path.ok_or_else(|| CliError("Usage: mls batch FILE.tm --replay DIR".into()))?;
    if live == replay.is_some() {
        return Err(Box::new(CliError(
            "choose exactly one of --replay DIR or --live; batch never starts live implicitly"
                .into(),
        )));
    }
    if live && env::var("MLS_LIVE").as_deref() != Ok("1") {
        return Err(Box::new(CliError("--live requires MLS_LIVE=1".into())));
    }
    let path = path
        .canonicalize()
        .map_err(|error| CliError(format!("cannot resolve input {}: {error}", path.display())))?;
    // Every check that can fail without model or Lean work runs before any side effect.
    let output = match output {
        Some(candidate) => {
            reject_output_alias(&path, &candidate)?;
            let output = absolute_path(&candidate)?;
            if output_exists(&output) {
                return Err(Box::new(CliError(format!(
                    "refusing to overwrite existing output {}",
                    output.display()
                ))));
            }
            reject_output_in_mls_directories(&path, &candidate)?;
            ensure_output_parent(&output)?;
            Some(output)
        }
        None => None,
    };
    let source = fs::read(&path)?;
    let analysis = analyze(&parse(&source)?);
    if analysis
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.code.as_str(), "duplicate_id" | "invalid_id"))
    {
        return Err(Box::new(CliError(
            "batch requires valid, unique identity metadata".into(),
        )));
    }
    let model_config = absolute_path(&model_config)?;
    let models = ModelConfig::from_path(&model_config).map_err(|e| {
        CliError(format!(
            "cannot load model config {}: {e}",
            model_config.display()
        ))
    })?;
    // The model transport is built before any side effect, so a missing replay directory or
    // API key is refused here rather than after the project exists.
    let transport = build_transport(replay, &models)?;
    preflight_batch(&path, live)?;
    let project = initialize_project(&path)?;
    let mut worker = spawn_worker(&project)?;
    let mut checker = LeanStatementChecker::new(&mut worker);
    let pipeline = PipelineConfig {
        max_attempts,
        sir_max_tokens: models.default_max_tokens,
        lean_max_tokens: models.default_max_tokens,
    };
    let (report, persistence) = match transport {
        Transport::Replay(mut client) => (
            execute(&source, &analysis, &mut client, &mut checker, pipeline)?,
            None,
        ),
        Transport::Live(client) => {
            let log_root = project.join("log");
            ensure_owned_dir(&log_root)?;
            let record_dir = log_root.join(format!("batch-{}", mls_core::BlockId::new()));
            eprintln!("Model call records: {}", record_dir.display());
            let mut client = RecordingClient::new(
                client,
                record_dir,
                &models.generation_model,
                &models.decision_model,
                "live",
            );
            let report = execute(&source, &analysis, &mut client, &mut checker, pipeline)?;
            (report, client.persistence_failure())
        }
    };
    publish(output, &report, persistence, || {
        write_shadow(&path, &analysis, &report)
    })
}

/// Builds the model transport for a batch or review run. Replay is chosen only by `--replay`; a
/// missing replay directory or API key is refused here, before any side effect.
fn build_transport(
    replay: Option<PathBuf>,
    models: &ModelConfig,
) -> Result<Transport, Box<dyn Error>> {
    Ok(match replay {
        Some(directory) => Transport::Replay(ReplayClient::from_directory(
            directory,
            &models.generation_model,
            &models.decision_model,
        )?),
        None => Transport::Live(OpenRouterClient::from_env(models.clone())?),
    })
}

/// Stores the report first, then writes the shadow project, so a shadow failure never loses the
/// report. A report that cannot be stored is printed to stdout and returned as an error, and the
/// shadow project is still attempted. An incomplete live recording is reported only afterwards.
fn publish(
    output: Option<PathBuf>,
    report: &BatchReport,
    persistence: Option<String>,
    write_shadow: impl FnOnce() -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let bytes = serde_json::to_vec_pretty(report)?;
    let report_error = store_report(output.as_deref(), &bytes, &mut std::io::stdout().lock()).err();
    let shadow = write_shadow();
    if let Some(detail) = persistence {
        if let Err(error) = &shadow {
            eprintln!("mls: shadow project was not written: {error}");
        }
        if let Some(error) = &report_error {
            eprintln!("mls: {error}");
        }
        let kept = if output.is_some() && report_error.is_none() {
            "the report was kept"
        } else {
            "the report was printed to stdout"
        };
        return Err(Box::new(CliError(format!(
            "model call recording is incomplete and not replayable ({detail}); {kept}"
        ))));
    }
    if let Some(error) = report_error {
        if let Err(error) = &shadow {
            eprintln!("mls: shadow project was not written: {error}");
        }
        return Err(error);
    }
    shadow
}

/// Stores the report at `output`, or prints it when no output file was requested. If storing
/// fails (for example disk full, or a noclobber race with a file created after the up-front
/// check), the report is printed to `stdout` instead and the error names the output path.
fn store_report(
    output: Option<&Path>,
    bytes: &[u8],
    stdout: &mut impl Write,
) -> Result<(), Box<dyn Error>> {
    let Some(output) = output else {
        print_report(stdout, bytes)?;
        return Ok(());
    };
    match atomic_create(output, bytes) {
        Ok(()) => Ok(()),
        Err(error) => {
            let note = match print_report(stdout, bytes) {
                Ok(()) => "the report was printed to stdout instead".to_string(),
                Err(stdout_error) => {
                    format!("printing it to stdout also failed: {stdout_error}")
                }
            };
            Err(Box::new(CliError(format!(
                "could not write report to {}: {error}; {note}",
                output.display()
            ))))
        }
    }
}

fn print_report(stdout: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    stdout.write_all(bytes)?;
    stdout.write_all(b"\n")
}

fn execute(
    source: &[u8],
    analysis: &mls_tm::Analysis,
    models: &mut impl ModelClient,
    checker: &mut impl mls_engine::StatementChecker,
    config: PipelineConfig,
) -> Result<BatchReport, Box<dyn Error>> {
    Ok(run_batch_analyzed(
        source, analysis, models, checker, config,
    )?)
}

/// The model transport for one batch, built before any side effect.
enum Transport {
    Replay(ReplayClient),
    Live(OpenRouterClient),
}

/// Checks that the prepared Lean runtime exists and returns its REPL launcher. MLS never downloads
/// tools, so a missing runtime is refused here, before the project is touched.
fn lean_runtime_script() -> Result<PathBuf, Box<dyn Error>> {
    let root = workspace_root();
    let script = root.join("scripts/run-lean-repl.sh");
    if !script.is_file() || !root.join(".mls/lakefile.lean").is_file() {
        return Err(Box::new(CliError(format!(
            "Lean runtime is not prepared; expected {} and {}/.mls (run repository setup; MLS does not download tools)",
            script.display(),
            root.display()
        ))));
    }
    Ok(script)
}

fn spawn_worker(project: &Path) -> Result<LeanWorker, Box<dyn Error>> {
    let root = workspace_root();
    let script = lean_runtime_script()?;
    Ok(LeanWorker::spawn(ReplConfig {
        program: script,
        args: vec![project.to_string_lossy().into_owned()],
        cwd: root.clone(),
        env: Vec::new(),
        timeout: Duration::from_secs(60),
        max_stdout_bytes: 4 * 1024 * 1024,
        max_stderr_bytes: 256 * 1024,
    })?)
}

fn write_shadow(
    document: &Path,
    analysis: &mls_tm::Analysis,
    report: &BatchReport,
) -> Result<(), Box<dyn Error>> {
    let root = shadow_root(document);
    migrate_flat_shadow(document, &root)?;
    ensure_owned_dir(&root)?;
    let directory = shadow_directory(document);
    prepare_shadow_directory(document, &directory)?;
    let sections = shadow_sections(analysis, report);
    write_shadow_sections(&directory, &document_module(document), &sections)
}

/// Writes generated sections so that every file MLS creates is listed in the ownership manifest
/// at the moment it exists. All collisions and unsafe paths are refused before the first removal
/// or write, so a refused run leaves the directory exactly as it was.
fn write_shadow_sections(
    directory: &Path,
    module: &str,
    sections: &[ShadowSection],
) -> Result<(), Box<dyn Error>> {
    let generated = generate_shadow_project(module, sections)?;
    let manifest_path = directory.join(SHADOW_MANIFEST);
    let old = read_shadow_manifest(&manifest_path)?;
    let names: Vec<String> = generated
        .iter()
        .map(|(name, _)| format!("{name}.lean"))
        .collect();

    for name in &names {
        let path = directory.join(name);
        if old.contains(name) {
            ensure_regular_if_present(&path)?;
        } else if is_occupied(&path)? {
            return Err(Box::new(CliError(format!(
                "refusing to overwrite unowned shadow file {}",
                path.display()
            ))));
        }
    }
    for name in old.iter().filter(|name| !names.contains(name)) {
        ensure_regular_if_present(&directory.join(name))?;
    }

    // Claim old and new names first: any file that exists afterwards is already listed.
    let mut claimed = old.clone();
    for name in &names {
        if !claimed.contains(name) {
            claimed.push(name.clone());
        }
    }
    atomic_replace_owned(&manifest_path, &serde_json::to_vec_pretty(&claimed)?)?;
    for (name, file) in generated {
        let path = directory.join(format!("{name}.lean"));
        atomic_replace_owned(&path, file.source.as_bytes())?;
    }
    for name in old.iter().filter(|name| !names.contains(name)) {
        remove_owned_if_present(&directory.join(name))?;
    }
    atomic_replace_owned(&manifest_path, &serde_json::to_vec_pretty(&names)?)?;
    Ok(())
}

fn read_shadow_manifest(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    if !is_occupied(path)? {
        return Ok(Vec::new());
    }
    ensure_regular_owned(path)?;
    let names: Vec<String> = serde_json::from_slice(&fs::read(path)?)?;
    if names.iter().any(|name| !valid_shadow_file_name(name)) {
        return Err(Box::new(CliError("invalid owned shadow manifest".into())));
    }
    Ok(names)
}

/// Removes a manifest-listed file. A file already gone (for example deleted by hand) counts as
/// removed; symlinks and non-regular files are still refused.
fn remove_owned_if_present(path: &Path) -> Result<(), Box<dyn Error>> {
    if !is_occupied(path)? {
        return Ok(());
    }
    ensure_regular_owned(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}

fn ensure_regular_if_present(path: &Path) -> Result<(), Box<dyn Error>> {
    if is_occupied(path)? {
        ensure_regular_owned(path)?;
    }
    Ok(())
}

/// True when a directory entry exists at `path`, including a dangling symlink.
fn is_occupied(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn shadow_sections(analysis: &mls_tm::Analysis, report: &BatchReport) -> Vec<ShadowSection> {
    let accepted: std::collections::HashMap<_, _> = report
        .blocks
        .iter()
        .filter(|report| report.accepted_speculatively)
        .filter_map(|report| {
            report
                .lean_statement
                .as_ref()
                .map(|source| (report.block_id, source.clone()))
        })
        .collect();
    let mut sections = vec![ShadowSection {
        name: "Section0".into(),
        declarations: Vec::new(),
    }];
    for block in &analysis.blocks {
        if block.kind == mls_core::BlockKind::Section {
            sections.push(ShadowSection {
                name: format!("Section{}", sections.len()),
                declarations: Vec::new(),
            });
        } else if let Some(source) = accepted.get(&block.id) {
            sections
                .last_mut()
                .expect("initial section exists")
                .declarations
                .push(ShadowDeclaration {
                    block_id: block.id,
                    source: source.clone(),
                    dependencies: block
                        .deps
                        .iter()
                        .copied()
                        .filter(|id| accepted.contains_key(id))
                        .collect(),
                });
        }
    }
    sections.retain(|section| !section.declarations.is_empty());
    sections
}

/// Names shared by the project initialization and its read-only preflight, so they cannot drift.
const PROJECT_OWNER: &str = ".mls-owner.json";
const PROJECT_MARKER: &[u8] = b"{\"format\":1}\n";
const PROJECT_FILES: [&str; 4] = [
    "lean-toolchain",
    "lake-manifest.json",
    "lakefile.lean",
    "MlsExtract.lean",
];
const PROJECT_BUILD_DIR: &str = ".lake/build/lib/lean";
const PROJECT_BUILD_FILES: [&str; 5] = [
    "MlsExtract.olean",
    "MlsExtract.ilean",
    "MlsExtract.trace",
    "MlsExtract.olean.hash",
    "MlsExtract.ilean.hash",
];
const SHADOW_ROOT: &str = "Shadow";
const SHADOW_OWNER: &str = ".mls-owner";
const SHADOW_MANIFEST: &str = ".mls-files.json";

fn project_directory(document: &Path) -> PathBuf {
    document
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".mls")
}

/// The Shadow library shared by the documents of one directory. Each document owns one module
/// directory below it, so documents in the same directory never write into each other's files.
fn shadow_root(document: &Path) -> PathBuf {
    project_directory(document).join(SHADOW_ROOT)
}

fn shadow_directory(document: &Path) -> PathBuf {
    shadow_root(document).join(document_module(document))
}

/// The Lean module component of a document's Shadow directory: `Doc_`, the ASCII alphanumerics of
/// the file stem (other characters become `_`, at most 40 of them), `_`, and the first 8 hex digits
/// of the SHA-256 of the whole file name. The hash keeps names apart and makes them valid
/// identifiers whatever the file name is.
fn document_module(document: &Path) -> String {
    let name = document
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let stem = name.strip_suffix(".tm").unwrap_or(&name);
    let readable: String = stem
        .chars()
        .take(40)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let digest = mls_core::source_hash(name.as_bytes());
    format!("Doc_{readable}_{}", &digest[..8])
}

/// The M2 flat layout kept its files directly in `.mls/Shadow`, owned by the one document that
/// `.mls/Shadow/.mls-owner` names. Returns the files that document owns there, or `None` when the
/// flat layout is absent or belongs to another document. Such files are never touched by this
/// document. Read-only: every refusal happens here, before any removal.
fn flat_shadow_owned_by_document(
    document: &Path,
    root: &Path,
) -> Result<Option<Vec<String>>, Box<dyn Error>> {
    if !is_occupied(root)? {
        return Ok(None);
    }
    ensure_plain_dir_if_present(root)?;
    let marker = root.join(SHADOW_OWNER);
    if !is_occupied(&marker)? || ensure_regular_owned(&marker).is_err() {
        return Ok(None);
    }
    if fs::read_to_string(&marker)? != document.to_string_lossy() {
        return Ok(None);
    }
    let names = read_shadow_manifest(&root.join(SHADOW_MANIFEST))
        .and_then(|names| {
            for name in &names {
                ensure_regular_if_present(&root.join(name))?;
            }
            Ok(names)
        })
        .map_err(|error| {
            CliError(format!(
                "cannot migrate the M2 Shadow layout of this document ({error}); delete {} and run again",
                root.display()
            ))
        })?;
    Ok(Some(names))
}

/// Removes the M2 flat layout that this document owns, once the new per-document layout is
/// ready to be written. Only manifest-listed regular files are removed; the manifest and the owner
/// marker go last, so an interrupted migration is retried by the next run.
fn migrate_flat_shadow(document: &Path, root: &Path) -> Result<(), Box<dyn Error>> {
    let Some(names) = flat_shadow_owned_by_document(document, root)? else {
        return Ok(());
    };
    for name in &names {
        remove_owned_if_present(&root.join(name))?;
    }
    remove_owned_if_present(&root.join(SHADOW_MANIFEST))?;
    remove_owned_if_present(&root.join(SHADOW_OWNER))
}

/// Read-only preflight: runs every refusal that the later writes would make on the project, the
/// Shadow directory and the Lean runtime, before any model call, worker spawn or write. It changes
/// nothing; the output directory is probed earlier, in `ensure_output_parent`. The writing code keeps
/// its own checks because the filesystem can change between this preflight and the writes.
fn preflight_batch(document: &Path, live: bool) -> Result<(), Box<dyn Error>> {
    preflight_project(document, &workspace_root().join(".mls"), live)?;
    lean_runtime_script()?;
    Ok(())
}

fn preflight_project(document: &Path, template: &Path, live: bool) -> Result<(), Box<dyn Error>> {
    let project = project_directory(document);
    ensure_project_is_not_template(&project, template)?;
    ensure_plain_dir_if_present(&project)?;
    if is_occupied(&project)? && ensure_project_marker(&project.join(PROJECT_OWNER)).is_err() {
        return Err(Box::new(CliError(format!(
            "refusing unowned existing project {}",
            project.display()
        ))));
    }
    preflight_shadow(document)?;
    preflight_project_files(&project, template)?;
    if live {
        ensure_plain_dir_if_present(&project.join("log"))?;
    }
    Ok(())
}

/// Mirrors `write_shadow` for this document's module directory. The M2 flat layout is checked
/// too, so a migration that would be refused is refused before any model call. An empty module
/// directory is claimed by the writer; a non-empty one must carry this document's marker.
fn preflight_shadow(document: &Path) -> Result<(), Box<dyn Error>> {
    let root = shadow_root(document);
    ensure_plain_dir_if_present(&root)?;
    flat_shadow_owned_by_document(document, &root)?;
    let directory = shadow_directory(document);
    ensure_plain_dir_if_present(&directory)?;
    if !is_occupied(&directory)? || fs::read_dir(&directory)?.next().is_none() {
        return Ok(());
    }
    let marker = directory.join(SHADOW_OWNER);
    if ensure_regular_owned(&marker).is_err()
        || fs::read_to_string(&marker)? != document.to_string_lossy()
    {
        return Err(Box::new(CliError(
            "another document or user owns this Shadow directory".into(),
        )));
    }
    let listed = read_shadow_manifest(&directory.join(SHADOW_MANIFEST))?;
    for name in &listed {
        ensure_regular_if_present(&directory.join(name))?;
    }
    for entry in fs::read_dir(&directory)? {
        let name = entry?.file_name();
        let unlisted_section = name.to_str().is_some_and(|name| {
            valid_shadow_file_name(name) && !listed.iter().any(|listed| listed == name)
        });
        if unlisted_section {
            return Err(Box::new(CliError(format!(
                "refusing to overwrite unowned shadow file {}",
                directory.join(&name).display()
            ))));
        }
    }
    Ok(())
}

/// Mirrors the template-file and directory checks of `initialize_project_from`.
fn preflight_project_files(project: &Path, template: &Path) -> Result<(), Box<dyn Error>> {
    for relative in [".lake", ".lake/build", ".lake/build/lib", PROJECT_BUILD_DIR] {
        ensure_plain_dir_if_present(&project.join(relative))?;
    }
    for relative in PROJECT_FILES {
        preflight_copy(&template.join(relative), &project.join(relative))?;
    }
    for relative in PROJECT_BUILD_FILES {
        let source = template.join(PROJECT_BUILD_DIR).join(relative);
        if source.exists() {
            preflight_copy(&source, &project.join(PROJECT_BUILD_DIR).join(relative))?;
        }
    }
    let packages = project.join(".lake/packages");
    if is_occupied(&packages)?
        && (!fs::symlink_metadata(&packages)?.file_type().is_symlink()
            || packages.canonicalize()? != template.join(".lake/packages").canonicalize()?)
    {
        return Err(Box::new(CliError(format!(
            "unexpected packages path {}",
            packages.display()
        ))));
    }
    Ok(())
}

/// Mirrors `copy_owned`: the template must be readable, and an existing destination must be a
/// regular file. A destination that differs from the template is refreshed, not refused.
fn preflight_copy(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::read(source)?;
    if is_occupied(destination)? {
        ensure_regular_owned(destination)?;
    }
    Ok(())
}

/// Refuses a symlink or non-directory where a project directory belongs. A missing path is fine.
fn ensure_plain_dir_if_present(path: &Path) -> Result<(), Box<dyn Error>> {
    if is_occupied(path)? {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Box::new(CliError(format!(
                "unsafe project directory {}",
                path.display()
            ))));
        }
    }
    Ok(())
}

fn initialize_project(document: &Path) -> Result<PathBuf, Box<dyn Error>> {
    initialize_project_from(document, &workspace_root().join(".mls"))
}

/// Refuses a project directory that is the runtime template itself, or shares its parent and name.
fn ensure_project_is_not_template(project: &Path, template: &Path) -> Result<(), Box<dyn Error>> {
    let same_parent = match (
        project.parent().map(fs::canonicalize),
        template.parent().map(fs::canonicalize),
    ) {
        (Some(Ok(left)), Some(Ok(right))) => left == right,
        _ => false,
    };
    if project == template || (same_parent && project.file_name() == template.file_name()) {
        return Err(Box::new(CliError(format!(
            "documents in the MLS repository root would share the runtime template {}; move the document into a subdirectory",
            template.display()
        ))));
    }
    Ok(())
}

fn initialize_project_from(document: &Path, template: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let project = project_directory(document);
    ensure_project_is_not_template(&project, template)?;
    let owner = project.join(PROJECT_OWNER);
    if project.exists() {
        if fs::symlink_metadata(&project)?.file_type().is_symlink() || !project.is_dir() {
            return Err(Box::new(CliError(format!(
                "unsafe MLS project path {}",
                project.display()
            ))));
        }
        if ensure_project_marker(&owner).is_err() {
            return Err(Box::new(CliError(format!(
                "refusing unowned existing project {}",
                project.display()
            ))));
        }
    } else {
        fs::create_dir(&project)?;
        atomic_create(&owner, PROJECT_MARKER)?;
    }
    ensure_owned_dir(&project.join(".lake"))?;
    ensure_owned_dir(&project.join(".lake/build"))?;
    ensure_owned_dir(&project.join(".lake/build/lib"))?;
    ensure_owned_dir(&project.join(".lake/build/lib/lean"))?;
    for relative in PROJECT_FILES {
        copy_owned(&template.join(relative), &project.join(relative))?;
    }
    for relative in PROJECT_BUILD_FILES {
        let source = template.join(PROJECT_BUILD_DIR).join(relative);
        if source.exists() {
            copy_owned(&source, &project.join(PROJECT_BUILD_DIR).join(relative))?;
        }
    }
    let packages = project.join(".lake/packages");
    if let Ok(metadata) = fs::symlink_metadata(&packages) {
        if !metadata.file_type().is_symlink()
            || packages.canonicalize()? != template.join(".lake/packages").canonicalize()?
        {
            return Err(Box::new(CliError(format!(
                "unexpected packages path {}",
                packages.display()
            ))));
        }
    } else {
        #[cfg(unix)]
        std::os::unix::fs::symlink(template.join(".lake/packages"), &packages)?;
    }
    Ok(project)
}

/// Checks the output directory before any side effect. The one write it makes is an exclusive
/// temporary file, created and removed at once, so an unwritable directory fails before any model
/// call instead of after paid work.
fn ensure_output_parent(output: &Path) -> Result<(), Box<dyn Error>> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    if !fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Box::new(CliError(format!(
            "output directory {} must exist and be a directory; create it before running batch",
            parent.display()
        ))));
    }
    let probe = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        CliError(format!(
            "output directory {} is not writable: {error}",
            parent.display()
        ))
    })?;
    drop(probe);
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    })
}

fn reject_output_alias(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let output = absolute_path(output)?;
    let aliases = output == input
        || output
            .canonicalize()
            .is_ok_and(|resolved| resolved == input)
        || (fs::metadata(input)
            .ok()
            .zip(fs::metadata(&output).ok())
            .is_some_and(|(a, b)| same_file(&a, &b)));
    if aliases {
        return Err(Box::new(CliError(
            "--output must not alias the input document".into(),
        )));
    }
    Ok(())
}

/// Whether anything, including a dangling symlink, already exists at an absolute output path.
fn output_exists(output: &Path) -> bool {
    fs::symlink_metadata(output).is_ok()
}

/// Refuses an output inside an MLS project directory before any side effect: the document-adjacent
/// `<document dir>/.mls` that batch creates, and the repository runtime template `<workspace>/.mls`.
/// Both sides are resolved the way the filesystem will see them (see `resolve_for_comparison`), so
/// a symlink followed by `..` cannot hide the alias.
fn reject_output_in_mls_directories(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let output = resolve_for_comparison(&absolute_path(output)?)?;
    let document_project = input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".mls");
    for directory in [document_project, workspace_root().join(".mls")] {
        let directory = resolve_for_comparison(&directory)?;
        if output.starts_with(&directory) {
            return Err(Box::new(CliError(format!(
                "--output must not be inside the MLS directory {}; choose a path outside it",
                directory.display()
            ))));
        }
    }
    Ok(())
}

/// Resolves an absolute `path` the way the operating system does, component by component: every
/// existing component is canonicalised (following symlinks) before a following `..` is applied, so
/// `link/../x` means the parent of the link's target. The longest existing prefix is therefore
/// canonical, and the names that do not exist yet are appended to it. A `..` after a name that does
/// not exist has no meaning yet, so such a path is refused. A dangling symlink is refused as well,
/// because writing through it would create its target.
fn resolve_for_comparison(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut resolved = PathBuf::new();
    let mut missing = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir => {}
            Component::ParentDir if missing => {
                return Err(Box::new(CliError(format!(
                    "--output {} uses '..' after a directory that does not exist; spell out the path without '..'",
                    path.display()
                ))));
            }
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                if missing {
                    resolved = candidate;
                    continue;
                }
                match candidate.canonicalize() {
                    Ok(canonical) => resolved = canonical,
                    // Nothing is linked at this name, so there is no target to follow. The
                    // later parent check reports a missing or non-directory parent.
                    Err(_) if fs::symlink_metadata(&candidate).is_err() => {
                        missing = true;
                        resolved = candidate;
                    }
                    Err(error) => {
                        return Err(Box::new(CliError(format!(
                            "cannot resolve {}: {error}",
                            candidate.display()
                        ))));
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}
#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    false
}

/// Creates `path` without replacing anything. The file gets ordinary 0644 permissions instead of
/// the 0600 that the temporary file starts with, so published reports are readable.
fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    set_ordinary_permissions(temporary.as_file())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

/// Gives a created or refreshed file the ordinary 0644 permissions instead of the 0600 that a
/// temporary file starts with.
fn set_ordinary_permissions(file: &fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn ensure_regular_owned(path: &Path) -> Result<(), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Box::new(CliError(format!(
            "unsafe owned artifact {}",
            path.display()
        ))));
    }
    Ok(())
}

/// A project is MLS-owned only when its marker is a regular file holding exactly MLS's marker.
fn ensure_project_marker(path: &Path) -> Result<(), Box<dyn Error>> {
    ensure_regular_owned(path)?;
    if fs::read(path)? != PROJECT_MARKER {
        return Err(Box::new(CliError(format!(
            "unexpected project marker {}",
            path.display()
        ))));
    }
    Ok(())
}

fn ensure_owned_dir(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(Box::new(
            CliError(format!("unsafe project directory {}", path.display())),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            Ok(())
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn atomic_replace_owned(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if path.exists() || fs::symlink_metadata(path).is_ok() {
        ensure_regular_owned(path)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    set_ordinary_permissions(temporary.as_file())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Copies a template file into an MLS-owned project. A missing destination is created. An existing
/// one must be a regular file; when it differs from the template it is refreshed atomically. The
/// caller has already verified the project marker, so only MLS-owned files are refreshed.
fn copy_owned(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let bytes = fs::read(source)?;
    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        ensure_regular_owned(destination)?;
        if fs::read(destination)? != bytes {
            atomic_replace_owned(destination, &bytes)?;
        }
        return Ok(());
    }
    atomic_create(destination, &bytes)
}

fn prepare_shadow_directory(document: &Path, directory: &Path) -> Result<(), Box<dyn Error>> {
    let marker = directory.join(SHADOW_OWNER);
    let identity = document.to_string_lossy();
    if directory.exists() {
        if fs::symlink_metadata(directory)?.file_type().is_symlink() || !directory.is_dir() {
            return Err(Box::new(CliError(format!(
                "unsafe shadow directory {}",
                directory.display()
            ))));
        }
        if fs::read_dir(directory)?.next().is_none() {
            atomic_create(&marker, identity.as_bytes())?;
            return Ok(());
        }
        if ensure_regular_owned(&marker).is_err() || fs::read_to_string(&marker)? != identity {
            return Err(Box::new(CliError(
                "another document or user owns this Shadow directory".into(),
            )));
        }
    } else {
        // The Shadow root was created by the caller, so only the module directory is new.
        fs::create_dir(directory)?;
        atomic_create(&marker, identity.as_bytes())?;
    }
    Ok(())
}

fn valid_shadow_file_name(name: &str) -> bool {
    name.strip_prefix("Section")
        .and_then(|rest| rest.strip_suffix(".lean"))
        .is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CLI crate lives under workspace/crates")
        .to_path_buf()
}
fn print_help() {
    println!(
        "Usage:\n  mls [--write-ids] FILE.tm\n  mls batch FILE.tm (--replay DIR | --live) [--model-config FILE] [--output FILE] [--max-attempts N]\n  mls retrieval-index INDEX_DIR (--live | --replay RECORDINGS_DIR) [--batch-size N]\n\nLive mode requires --live and MLS_LIVE=1."
    );
    review::print_help();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_claims_an_empty_directory_but_preserves_unowned_contents() {
        let temporary = tempfile::tempdir().unwrap();
        let document = temporary.path().join("notes.tm");
        let shadow = temporary.path().join("Shadow");
        fs::create_dir(&shadow).unwrap();
        prepare_shadow_directory(&document, &shadow).unwrap();
        prepare_shadow_directory(&document, &shadow).unwrap();
        assert!(prepare_shadow_directory(&temporary.path().join("other.tm"), &shadow).is_err());

        let unowned = temporary.path().join("Unowned");
        fs::create_dir(&unowned).unwrap();
        fs::write(unowned.join("user.lean"), b"preserve this").unwrap();
        assert!(prepare_shadow_directory(&document, &unowned).is_err());
        assert_eq!(
            fs::read(unowned.join("user.lean")).unwrap(),
            b"preserve this"
        );
    }

    #[test]
    fn project_initialization_is_idempotent_and_refreshes_owned_template_files() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        assert!(project.is_absolute());
        initialize_project_from(&document, &template).unwrap();

        // A project-side edit is restored from the template, as is a changed template file.
        fs::write(project.join("lakefile.lean"), b"edited in project").unwrap();
        initialize_project_from(&document, &template).unwrap();
        assert_eq!(
            fs::read(project.join("lakefile.lean")).unwrap(),
            b"fixture for lakefile.lean"
        );
        fs::write(template.join("lean-toolchain"), b"leanprover/lean4:v-new").unwrap();
        preflight_project(&document, &template, false).unwrap();
        initialize_project_from(&document, &template).unwrap();
        assert_eq!(
            fs::read(project.join("lean-toolchain")).unwrap(),
            b"leanprover/lean4:v-new"
        );
        assert_eq!(
            fs::read(project.join(PROJECT_OWNER)).unwrap(),
            PROJECT_MARKER
        );
    }

    #[test]
    fn changed_compiled_template_artifacts_are_refreshed_for_owned_projects() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let olean = Path::new(PROJECT_BUILD_DIR).join("MlsExtract.olean");
        fs::write(template.join(&olean), b"old olean").unwrap();
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();

        fs::write(template.join(&olean), b"new olean").unwrap();
        preflight_project(&document, &template, false).unwrap();
        initialize_project_from(&document, &template).unwrap();
        assert_eq!(fs::read(project.join(&olean)).unwrap(), b"new olean");
    }

    #[cfg(unix)]
    #[test]
    fn refresh_refuses_symlinked_template_files_and_unowned_projects() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();

        // A template-derived file replaced by a symlink is never followed or refreshed.
        let elsewhere = temporary.path().join("elsewhere.lean");
        fs::write(&elsewhere, b"outside").unwrap();
        fs::remove_file(project.join("lakefile.lean")).unwrap();
        symlink(&elsewhere, project.join("lakefile.lean")).unwrap();
        fs::write(template.join("lakefile.lean"), b"changed template").unwrap();
        assert!(preflight_project(&document, &template, false).is_err());
        assert!(initialize_project_from(&document, &template).is_err());
        assert_eq!(fs::read(&elsewhere).unwrap(), b"outside");

        // A project whose marker is not MLS's exact marker is unowned, so refreshing is refused.
        fs::remove_file(project.join("lakefile.lean")).unwrap();
        fs::write(project.join("lakefile.lean"), b"fixture for lakefile.lean").unwrap();
        fs::write(project.join(PROJECT_OWNER), b"{\"format\":2}\n").unwrap();
        assert!(preflight_project(&document, &template, false).is_err());
        let error = initialize_project_from(&document, &template)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing unowned existing project"));
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_output_parent_is_refused_before_any_project_exists() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let readonly = temporary.path().join("readonly");
        fs::create_dir(&readonly).unwrap();
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).unwrap();
        // Root can write into a read-only directory, so the refusal cannot be observed there.
        if fs::File::create(readonly.join("root-check")).is_ok() {
            fs::remove_file(readonly.join("root-check")).unwrap();
            fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let result = ensure_output_parent(&readonly.join("report.json"));
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.contains("not writable"));
        assert_eq!(fs::read_dir(&readonly).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn project_initialization_rejects_symlinked_project_directory() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        symlink(elsewhere.path(), temporary.path().join(".mls")).unwrap();
        assert!(initialize_project(&document).is_err());
    }

    #[test]
    fn document_in_repository_root_is_refused_before_touching_the_template() {
        let temporary = tempfile::tempdir().unwrap();
        let template = temporary.path().join(".mls");
        fs::create_dir_all(template.join(".lake/packages")).unwrap();
        fs::write(template.join("lakefile.lean"), b"template").unwrap();
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();

        let error = initialize_project_from(&document, &template)
            .unwrap_err()
            .to_string();
        assert!(error.contains("share the runtime template"));
        assert!(!template.join("Shadow").exists());
        assert_eq!(
            fs::read(template.join("lakefile.lean")).unwrap(),
            b"template"
        );
    }

    const MODULE: &str = "Doc_notes_00000000";

    fn section(name: &str) -> ShadowSection {
        ShadowSection {
            name: name.into(),
            declarations: Vec::new(),
        }
    }

    #[test]
    fn stale_manifest_entry_whose_file_was_deleted_is_corrected() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("Shadow");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("Section1.lean"), b"old").unwrap();
        fs::write(
            directory.join(".mls-files.json"),
            br#"["Section1.lean","Section2.lean"]"#,
        )
        .unwrap();

        write_shadow_sections(&directory, MODULE, &[section("Section1")]).unwrap();

        let manifest: Vec<String> =
            serde_json::from_slice(&fs::read(directory.join(".mls-files.json")).unwrap()).unwrap();
        assert_eq!(manifest, ["Section1.lean"]);
        assert!(!directory.join("Section2.lean").exists());
        assert_ne!(fs::read(directory.join("Section1.lean")).unwrap(), b"old");
    }

    #[test]
    fn unowned_collision_is_refused_before_any_removal_or_write() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("Shadow");
        fs::create_dir(&directory).unwrap();
        let manifest = br#"["Section2.lean"]"#;
        fs::write(directory.join(".mls-files.json"), manifest).unwrap();
        fs::write(directory.join("Section2.lean"), b"owned stale").unwrap();
        fs::write(directory.join("Section1.lean"), b"user file").unwrap();

        let error = write_shadow_sections(&directory, MODULE, &[section("Section1")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unowned"));
        assert_eq!(
            fs::read(directory.join("Section2.lean")).unwrap(),
            b"owned stale"
        );
        assert_eq!(
            fs::read(directory.join("Section1.lean")).unwrap(),
            b"user file"
        );
        assert_eq!(
            fs::read(directory.join(".mls-files.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 3);
    }

    #[test]
    fn document_modules_are_valid_distinct_identifiers() {
        let names = [
            "notes.tm",
            "notes-2.tm",
            "Notes.tm",
            "a b.tm",
            "7 days.tm",
            "\u{e8}t\u{e9}.tm",
            ".tm",
        ];
        let modules: Vec<String> = names
            .iter()
            .map(|name| document_module(Path::new(&format!("/d/{name}"))))
            .collect();
        for module in &modules {
            assert!(module.starts_with("Doc_"), "{module}");
            assert!(
                module
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "{module}"
            );
        }
        let distinct: std::collections::BTreeSet<_> = modules.iter().collect();
        assert_eq!(distinct.len(), modules.len());
        assert_eq!(
            document_module(Path::new("/d/notes.tm")),
            document_module(Path::new("/other/notes.tm")),
            "the module depends on the file name only"
        );
    }

    #[test]
    fn two_documents_in_one_directory_claim_separate_shadow_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let first = temporary.path().join("notes.tm");
        let second = temporary.path().join("other.tm");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        initialize_project_from(&first, &template).unwrap();

        preflight_project(&first, &template, false).unwrap();
        preflight_project(&second, &template, false).unwrap();
        let sections = [section("Section1")];
        for document in [&first, &second] {
            let root = shadow_root(document);
            ensure_owned_dir(&root).unwrap();
            let directory = shadow_directory(document);
            prepare_shadow_directory(document, &directory).unwrap();
            write_shadow_sections(&directory, &document_module(document), &sections).unwrap();
        }
        assert!(shadow_directory(&first).join("Section1.lean").is_file());
        assert!(shadow_directory(&second).join("Section1.lean").is_file());
        assert_ne!(shadow_directory(&first), shadow_directory(&second));
        preflight_project(&first, &template, false).unwrap();
    }

    #[test]
    fn m2_flat_layout_of_this_document_is_migrated_and_user_files_are_kept() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        let root = project.join("Shadow");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join(SHADOW_OWNER),
            document.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(root.join(SHADOW_MANIFEST), br#"["Section1.lean"]"#).unwrap();
        fs::write(root.join("Section1.lean"), b"old flat").unwrap();
        fs::write(root.join("Mine.lean"), b"user file").unwrap();

        preflight_project(&document, &template, false).unwrap();
        migrate_flat_shadow(&document, &root).unwrap();
        assert!(!root.join("Section1.lean").exists());
        assert!(!root.join(SHADOW_MANIFEST).exists());
        assert!(!root.join(SHADOW_OWNER).exists());
        assert_eq!(fs::read(root.join("Mine.lean")).unwrap(), b"user file");
    }

    #[test]
    fn m2_flat_layout_of_another_document_is_left_alone() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        let root = project.join("Shadow");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(SHADOW_OWNER), b"/elsewhere/other.tm").unwrap();
        fs::write(root.join(SHADOW_MANIFEST), br#"["Section1.lean"]"#).unwrap();
        fs::write(root.join("Section1.lean"), b"other's flat file").unwrap();

        let before = tree(temporary.path());
        preflight_project(&document, &template, false).unwrap();
        migrate_flat_shadow(&document, &root).unwrap();
        assert_eq!(tree(temporary.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn m2_flat_layout_with_an_unsafe_entry_is_refused_with_a_clear_message() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        let root = project.join("Shadow");
        fs::create_dir(&root).unwrap();
        let elsewhere = temporary.path().join("elsewhere.lean");
        fs::write(&elsewhere, b"outside").unwrap();
        fs::write(
            root.join(SHADOW_OWNER),
            document.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(root.join(SHADOW_MANIFEST), br#"["Section1.lean"]"#).unwrap();
        symlink(&elsewhere, root.join("Section1.lean")).unwrap();

        let before = tree(temporary.path());
        let error = preflight_project(&document, &template, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("delete"), "{error}");
        assert_eq!(tree(temporary.path()), before);
        assert_eq!(fs::read(&elsewhere).unwrap(), b"outside");
    }

    fn empty_report() -> BatchReport {
        BatchReport {
            diagnostics: Vec::new(),
            blocks: Vec::new(),
            usage: mls_engine::UsageSummary::default(),
        }
    }

    #[test]
    fn report_is_kept_when_shadow_or_recording_fails() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("report.json");
        let error = publish(Some(output.clone()), &empty_report(), None, || {
            Err(Box::new(CliError("shadow failed".into())))
        })
        .unwrap_err()
        .to_string();
        assert_eq!(error, "shadow failed");
        assert!(fs::read_to_string(&output).unwrap().contains("\"blocks\""));

        let recorded = temporary.path().join("recorded.json");
        let error = publish(
            Some(recorded.clone()),
            &empty_report(),
            Some("2 model call record(s) not persisted".into()),
            || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not replayable"));
        assert!(recorded.is_file());
    }

    fn template_fixture(root: &Path) -> PathBuf {
        let template = root.join("runtime-template");
        fs::create_dir_all(template.join(".lake/packages")).unwrap();
        fs::create_dir_all(template.join(PROJECT_BUILD_DIR)).unwrap();
        for name in PROJECT_FILES {
            fs::write(template.join(name), format!("fixture for {name}")).unwrap();
        }
        template
    }

    /// Every path under `root` with its file bytes (`None` for directories), sorted.
    fn tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
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
    fn preflight_accepts_a_fresh_document_and_creates_nothing() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let before = tree(temporary.path());
        preflight_project(&document, &template, true).unwrap();
        assert_eq!(tree(temporary.path()), before);
    }

    #[test]
    fn preflight_refuses_a_foreign_shadow_owner_without_changing_anything() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        let shadow = project.join("Shadow").join(document_module(&document));
        fs::create_dir_all(&shadow).unwrap();
        fs::write(shadow.join(SHADOW_OWNER), b"/elsewhere/other.tm").unwrap();
        fs::write(shadow.join(SHADOW_MANIFEST), br#"["Section1.lean"]"#).unwrap();
        fs::write(shadow.join("Section1.lean"), b"owned").unwrap();

        let before = tree(temporary.path());
        let error = preflight_project(&document, &template, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("owns this Shadow directory"));
        assert_eq!(tree(temporary.path()), before);
    }

    #[test]
    fn preflight_refuses_an_unlisted_section_file_in_an_owned_shadow() {
        let temporary = tempfile::tempdir().unwrap();
        let template = template_fixture(temporary.path());
        let document = temporary.path().join("notes.tm");
        fs::write(&document, b"document").unwrap();
        let project = initialize_project_from(&document, &template).unwrap();
        let shadow = project.join("Shadow").join(document_module(&document));
        fs::create_dir_all(&shadow).unwrap();
        fs::write(
            shadow.join(SHADOW_OWNER),
            document.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(shadow.join(SHADOW_MANIFEST), b"[]").unwrap();
        fs::write(shadow.join("Section3.lean"), b"user file").unwrap();

        let error = preflight_project(&document, &template, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unowned shadow file"));
        assert_eq!(
            fs::read(shadow.join("Section3.lean")).unwrap(),
            b"user file"
        );
    }

    #[test]
    fn report_that_cannot_be_stored_is_printed_and_the_error_names_the_output() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("report.json");
        fs::write(&output, b"created by someone else").unwrap();
        let bytes = serde_json::to_vec_pretty(&empty_report()).unwrap();
        let mut stdout = Vec::new();

        let error = store_report(Some(&output), &bytes, &mut stdout)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&output.display().to_string()));
        assert!(error.contains("printed to stdout"));
        assert_eq!(stdout, [bytes.as_slice(), b"\n"].concat());
        assert_eq!(fs::read(&output).unwrap(), b"created by someone else");
    }

    #[cfg(unix)]
    #[test]
    fn created_report_has_ordinary_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("report.json");
        atomic_create(&output, b"{}").unwrap();
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
