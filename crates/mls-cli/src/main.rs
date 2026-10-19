//! Batch M1 command: parse a TeXmacs file and emit its block graph as JSON.

use mls_tm::{analyze, parse, write_ids_atomic};
use serde::Serialize;
use std::{env, error::Error, fmt, fs, path::PathBuf, process::ExitCode};

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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mls: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut write_ids = false;
    let mut path = None;
    for argument in env::args_os().skip(1) {
        if argument == "--write-ids" {
            write_ids = true;
        } else if argument == "--help" || argument == "-h" {
            println!(
                "Usage: mls [--write-ids] FILE.tm\n\nParses blocks as JSON. Files are read-only unless --write-ids is supplied."
            );
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
            .any(|diagnostic| matches!(diagnostic.code.as_str(), "duplicate_id" | "invalid_id"))
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
