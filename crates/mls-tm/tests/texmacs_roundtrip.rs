//! Real TeXmacs load-and-save checks. Gated: set `MLS_TEXMACS_TESTS=1` (skipped otherwise).
//!
//! For every document, each block must keep its ID, kind, `text_hash` and canonical text across
//! a headless `TeXmacs -H -C in out` save. A real wording change must change only its block.
//! TeXmacs runs with a scratch `TEXMACS_HOME_PATH` and `HOME`, so `~/.TeXmacs` is never touched,
//! and each run is wrapped in `perl -e 'alarm 120; exec @ARGV'`. `MLS_TEXMACS_BIN` overrides the
//! binary path.

use mls_tm::{Analysis, analyze, parse, write_ids_atomic};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod texmacs_common;

use texmacs_common::gated_bin;

const INSTALLED_DOCS: &str = "/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/doc";
const SAMPLE_SIZE: usize = 40;

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(relative)
}

/// Add the blank lines TeXmacs 2.1.x needs after the header and after the style declaration.
/// Without them the body (after the style) or the style itself is dropped on load and save.
fn canonical_layout(bytes: &[u8]) -> Vec<u8> {
    let lines: Vec<&[u8]> = bytes.split_inclusive(|&byte| byte == b'\n').collect();
    let mut out = Vec::with_capacity(bytes.len() + 2);
    for (index, line) in lines.iter().enumerate() {
        out.extend_from_slice(line);
        let declaration = line.starts_with(b"<TeXmacs|") || line.starts_with(b"<style|");
        let next_has_content = lines
            .get(index + 1)
            .is_some_and(|next| !next.iter().all(u8::is_ascii_whitespace));
        if declaration && next_has_content {
            out.push(b'\n');
        }
    }
    out
}

fn analyze_bytes(bytes: &[u8]) -> Result<Analysis, String> {
    let document = parse(bytes).map_err(|error| format!("parse failed: {error}"))?;
    Ok(analyze(&document))
}

/// Run one headless TeXmacs conversion with a scratch home.
fn texmacs_save(bin: &Path, home: &Path, input: &Path, output: &Path) -> Result<(), String> {
    let log = |name: &str| File::create(home.join(name)).map_err(|error| error.to_string());
    let status = Command::new("perl")
        .arg("-e")
        .arg("alarm 120; exec @ARGV")
        .arg("env")
        .arg(format!("TEXMACS_HOME_PATH={}", home.display()))
        .arg(format!("HOME={}", home.display()))
        .arg(bin)
        .args(["-H", "-C"])
        .arg(input)
        .arg(output)
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(log("texmacs.out")?)
        .stderr(log("texmacs.err")?)
        .status()
        .map_err(|error| format!("cannot start perl/TeXmacs: {error}"))?;
    if !status.success() {
        return Err(format!("TeXmacs exited with {status}"));
    }
    match fs::metadata(output) {
        Ok(metadata) if metadata.len() > 0 => Ok(()),
        _ => Err("TeXmacs wrote no output".into()),
    }
}

/// Compare block identity and canonical text between two analyses of the same document.
fn compare(before: &Analysis, after: &Analysis) -> Result<usize, String> {
    if before.blocks.len() != after.blocks.len() {
        return Err(format!(
            "block count changed: {} -> {}",
            before.blocks.len(),
            after.blocks.len()
        ));
    }
    for (index, (old, new)) in before.blocks.iter().zip(&after.blocks).enumerate() {
        if old.id != new.id || old.kind != new.kind {
            return Err(format!("block {index}: identity or kind changed"));
        }
        let old_text = before.canonical_text(old.id).unwrap_or_default();
        let new_text = after.canonical_text(new.id).unwrap_or_default();
        if old_text != new_text || old.text_hash != new.text_hash {
            return Err(format!(
                "block {index} ({:?}) canonical text changed:\n  before: {}\n  after:  {}",
                old.kind,
                String::from_utf8_lossy(old_text),
                String::from_utf8_lossy(new_text),
            ));
        }
    }
    Ok(before.blocks.len())
}

/// Result of one document's round trip.
struct Outcome {
    blocks: usize,
    /// The first save changed the trees of a legacy-format document (see `roundtrip`).
    upgraded: bool,
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Whether a document was written by a pre-2.0 TeXmacs. TeXmacs upgrades such trees on load.
fn legacy_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"<TeXmacs|") && !bytes.starts_with(b"<TeXmacs|2.")
}

/// Load-and-save one document through TeXmacs and check every block.
///
/// Documents from TeXmacs 2.x must keep every block through one save. A pre-2.0 document is
/// upgraded by TeXmacs when it is loaded (for example `with mode math` becomes `math`), so its
/// first save may legitimately change trees. The check then starts from that saved file: a second
/// save must keep every block, and the upgrade is reported.
fn roundtrip(
    bin: &Path,
    home: &Path,
    work: &Path,
    name: &str,
    source: &[u8],
) -> Result<Outcome, String> {
    let layout = canonical_layout(source);
    let input = work.join(format!("{name}.tm"));
    let output = work.join(format!("{name}.saved.tm"));
    fs::write(&input, &layout).map_err(|error| error.to_string())?;
    let first = analyze_bytes(&layout)?;
    if !first.insertions.is_empty() {
        // Give every block a persisted ID with the production writer, so IDs can be compared.
        write_ids_atomic(&input, &layout, &first.insertions).map_err(|error| error.to_string())?;
    }
    let bytes = fs::read(&input).map_err(|error| error.to_string())?;
    let before = analyze_bytes(&bytes)?;
    texmacs_save(bin, home, &input, &output)?;
    let saved = fs::read(&output).map_err(|error| error.to_string())?;
    let after = analyze_bytes(&saved)?;
    match compare(&before, &after) {
        Ok(blocks) => Ok(Outcome {
            blocks,
            upgraded: false,
        }),
        Err(error) if legacy_header(&bytes) => {
            let first_line = error.lines().take(4).collect::<Vec<_>>().join(" | ");
            eprintln!("legacy upgrade {name}: {}", truncate(&first_line, 400));
            let again = work.join(format!("{name}.again.tm"));
            texmacs_save(bin, home, &output, &again)?;
            let resaved = fs::read(&again).map_err(|error| error.to_string())?;
            compare(&after, &analyze_bytes(&resaved)?)
                .map(|blocks| Outcome {
                    blocks,
                    upgraded: true,
                })
                .map_err(|second| format!("{error}; a second save also changed: {second}"))
        }
        Err(error) => Err(error),
    }
}

/// Change one letter of one block's text and check that only that block's hash moves.
fn wording_change_is_local(work: &Path, name: &str, saved: &[u8]) -> Result<(), String> {
    let before = analyze_bytes(saved)?;
    for block in &before.blocks {
        let start = block.source_start;
        for position in start..block.source_end {
            let byte = saved[position];
            if !byte.is_ascii_alphabetic() {
                continue;
            }
            let mut edited = saved.to_vec();
            edited[position] = if byte.is_ascii_lowercase() {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            };
            let Ok(after) = analyze_bytes(&edited) else {
                continue;
            };
            let same_shape = after.blocks.len() == before.blocks.len()
                && after
                    .blocks
                    .iter()
                    .zip(&before.blocks)
                    .all(|(new, old)| new.id == old.id && new.kind == old.kind);
            if !same_shape {
                continue;
            }
            let index = before
                .blocks
                .iter()
                .position(|candidate| candidate.id == block.id)
                .expect("block comes from the same analysis");
            let changed = after.blocks[index].text_hash != before.blocks[index].text_hash;
            if !changed {
                continue;
            }
            for (other, (new, old)) in after.blocks.iter().zip(&before.blocks).enumerate() {
                if other != index && new.text_hash != old.text_hash {
                    return Err(format!("{name}: unrelated block {other} changed its hash"));
                }
            }
            fs::write(work.join(format!("{name}.edited.tm")), &edited)
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
    }
    Err(format!(
        "{name}: no wording change found that keeps the block shape"
    ))
}

fn fixture_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ["tests/fixtures/tm", "tests/fixtures/lean/pipeline"]
        .iter()
        .flat_map(|dir| fs::read_dir(fixture(dir)).expect("fixture directory"))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "tm"))
        .collect();
    files.push(fixture("evals/statements.tm"));
    files.sort();
    files
}

fn installed_sample() -> Vec<PathBuf> {
    let mut docs = Vec::new();
    let mut pending = vec![PathBuf::from(INSTALLED_DOCS)];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(".en.tm") {
                docs.push(path);
            }
        }
    }
    docs.sort();
    if docs.len() <= SAMPLE_SIZE {
        return docs;
    }
    (0..SAMPLE_SIZE)
        .map(|index| docs[index * docs.len() / SAMPLE_SIZE].clone())
        .collect()
}

#[test]
fn fixtures_keep_block_text_through_a_texmacs_save() {
    let Some(bin) = gated_bin() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in fixture_files() {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let source = fs::read(&path).unwrap();
        match roundtrip(&bin, &home, &work, &name, &source) {
            Ok(outcome) => {
                checked += 1;
                let note = if outcome.upgraded {
                    " (legacy upgrade on first save; second save stable)"
                } else {
                    ""
                };
                eprintln!("ok {name}: {} blocks{note}", outcome.blocks);
            }
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "fixture failures:\n{}",
        failures.join("\n")
    );
    assert!(checked > 0);
}

#[test]
fn a_wording_change_moves_only_its_block_hash() {
    let Some(bin) = gated_bin() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    let path = fixture("tests/fixtures/lean/pipeline/success.tm");
    let source = fs::read(&path).unwrap();
    let layout = canonical_layout(&source);
    let input = work.join("success.tm");
    let output = work.join("success.saved.tm");
    fs::write(&input, &layout).unwrap();
    let first = analyze_bytes(&layout).unwrap();
    write_ids_atomic(&input, &layout, &first.insertions).unwrap();
    texmacs_save(&bin, &home, &input, &output).unwrap();
    let saved = fs::read(&output).unwrap();
    wording_change_is_local(&work, "success", &saved).unwrap();
}

#[test]
fn installed_documentation_sample_keeps_block_text_through_a_texmacs_save() {
    let Some(bin) = gated_bin() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    let docs = installed_sample();
    let (mut ok, mut upgraded, mut blocks, mut skipped, mut failures) = (0, 0, 0, 0, Vec::new());
    for (index, path) in docs.iter().enumerate() {
        let name = format!("doc{index:02}");
        let source = fs::read(path).unwrap();
        if analyze_bytes(&canonical_layout(&source)).is_err() {
            skipped += 1;
            eprintln!("skip {}: parse failed", path.display());
            continue;
        }
        match roundtrip(&bin, &home, &work, &name, &source) {
            Ok(outcome) => {
                ok += 1;
                blocks += outcome.blocks;
                upgraded += usize::from(outcome.upgraded);
            }
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    eprintln!(
        "installed sample: {} documents, {ok} round-tripped ({upgraded} legacy upgrades), {blocks} blocks compared, {skipped} skipped, {} failed",
        docs.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "round-trip failures:\n{}",
        failures.join("\n")
    );
}
