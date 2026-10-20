//! Real TeXmacs checks that `write_ids_atomic` does not change what a document means. Gated: set
//! `MLS_TEXMACS_TESTS=1` (skipped otherwise).
//!
//! For each document, the writer's output is loaded by TeXmacs next to the original, and the two
//! body trees must be equal once `mls-id` carriers are stripped. Block identity, kind and
//! canonical text must also survive the write. A document whose span holds a bare `|` or `>`
//! must be refused (`BareSeparator`), and so must one whose span begins or ends at white space,
//! a line join or an escaping backslash (`UnsafeBoundary`); both are left untouched. TeXmacs
//! runs with a scratch `TEXMACS_HOME_PATH` and `HOME`, under `perl -e 'alarm 240; exec @ARGV'`.
//! `MLS_TEXMACS_BIN` overrides the binary path.

use mls_tm::{Analysis, WriteError, analyze, parse, write_ids_atomic};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod texmacs_common;

use texmacs_common::gated_bin;

const INSTALLED_DOCS: &str = "/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/doc";
const SAMPLE_SIZE: usize = 40;

/// Loads each file named in `DUMP_LIST` (one path per line), and writes its body tree as an
/// S-expression to `DUMP_OUT/<index>.stree`. A file TeXmacs cannot load gets `DUMP_ERROR`.
const DUMP_SCM: &str = r#"(define (dump-one path out)
  (catch #t
    (lambda ()
      (let ((url (string->url path)))
        (load-buffer url)
        (string-save (object->string (tree->stree (buffer-get-body url))) out)))
    (lambda args (string-save (string-append "DUMP_ERROR " (object->string args)) out))))
(define (dump-run)
  (let* ((paths (string-split (string-load (getenv "DUMP_LIST")) #\newline))
         (dir (getenv "DUMP_OUT")))
    (let loop ((ps paths) (n 0))
      (cond ((null? ps) #t)
            ((string-null? (car ps)) (loop (cdr ps) n))
            (else
             (dump-one (car ps) (string-append dir "/" (number->string n) ".stree"))
             (loop (cdr ps) (+ n 1)))))
    (display "DUMP_DONE") (newline)))
(catch #t (lambda () (dump-run)) (lambda args (display "DUMP_FAILED") (newline)))
"#;

/// Hand-written documents that cover prose, theorem bodies, multi-paragraph proofs, math, and
/// nested tags, with escaped and nested separators. Every one of them is expected to be written.
const SYNTHETIC_OK: &[(&str, &str)] = &[
    (
        "escaped-separators",
        r"<\body>
<\theorem>
Let a\|b and c\>d be given. Then <math|x\|y> holds.
</theorem>

First paragraph with a pipe \| and a gt \> here.

Second paragraph with <em|nested\|pipe> and <with|font-series|bold|x\>y>.

<\proof>
Proof text: a\|b, then <math|\|x\|> follows.

Another paragraph of the proof, with a trailing escaped \> sign.
</proof>
</body>",
    ),
    (
        "nested-separators",
        r"<\body>
Prose with <em|a|b> and <with|font-series|bold|c> in it.

<\theorem>
A <math|u|v> and <math|p\|q> inside a theorem.
</theorem>

<\proof>
Short <with|color|red|r> and <em|s> pieces, then <math|1>.
</proof>
</body>",
    ),
    (
        // A bare separator inside a formal block is the block's own argument structure, which
        // the carrier wraps unchanged, so the writer must not refuse it.
        "bare-separator-inside-block",
        r"<\body>
<\proof>
Proof line a>b here.
</proof>
</body>",
    ),
    (
        // Joins and escaped white space inside prose, away from the span edges, are written.
        "joins-and-escaped-edges",
        r"<\body>
<\theorem>
T.
</theorem>

A line that \
  continues and ends with an escaped space\

A double backslash \\
  then a word.
</body>",
    ),
];

/// Documents whose prose holds a bare separator, or whose span starts or ends at white space, a
/// line join or an escaping backslash. TeXmacs reads those bytes as argument separators (or drops
/// the whole body), or the carrier changes the spacing at the edge, so the writer must refuse them.
const SYNTHETIC_REFUSED: &[(&str, &str)] = &[
    (
        "bare-pipe-prose",
        r"<\body>
Some prose a|b here.
</body>",
    ),
    (
        "bare-gt-after-block",
        r"<\body>
<\theorem>
T.
</theorem>

Some prose a>b here.
</body>",
    ),
    (
        // The span starts with a line join. Wrapping it read as "  xHello" where the original
        // read " xHello".
        "reproducer-join-at-span-start",
        "<\\body>\n<section|Hi<em|x>  >\n\\\n  \r\n  xHello<em|y>\n</body>",
    ),
    (
        // The span ends with a line join: its last byte is the line feed of `\`+LF.
        "join-at-span-end",
        r"<\body>
<\theorem>
T.
</theorem>

Join ends here\
</body>",
    ),
];

const HEADER: &str = "<TeXmacs|2.1.5>\n\n<style|generic>\n\n";
const FOOTER: &str = "\n";

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(relative)
}

/// Add the blank lines TeXmacs 2.1.x needs after the header and after the style declaration, as
/// the roundtrip tests do.
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

/// Sources of the fixtures the roundtrip tests use, plus the statement corpus.
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

/// Every installed `*.en.tm` document, in a stable order.
fn installed_docs() -> Vec<PathBuf> {
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
    docs
}

/// A fixed-seed random sample of the installed documents (Fisher-Yates over an xorshift
/// generator), so every run checks the same documents.
fn installed_sample() -> Vec<PathBuf> {
    let mut docs = installed_docs();
    if docs.len() <= SAMPLE_SIZE {
        return docs;
    }
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for index in (1..docs.len()).rev() {
        let other = usize::try_from(next() % (index as u64 + 1)).expect("index fits in usize");
        docs.swap(index, other);
    }
    docs.truncate(SAMPLE_SIZE);
    docs.sort();
    docs
}

/// Check the writer's output for one source: block identity, kind, canonical text and text hash
/// must survive, every block must hold a persisted ID, and a second write must change nothing.
/// Returns `Ok(None)` when the writer refused the source (a bare separator or an unsafe boundary).
fn write_and_check(work: &Path, name: &str, layout: &[u8]) -> Result<Option<PathBuf>, String> {
    let before = analyze_bytes(layout)?;
    let path = work.join(format!("{name}.wrapped.tm"));
    fs::write(&path, layout).map_err(|error| error.to_string())?;
    match write_ids_atomic(&path, layout, &before.insertions) {
        Ok(()) => {}
        Err(WriteError::BareSeparator | WriteError::UnsafeBoundary) => return Ok(None),
        Err(error) => return Err(format!("write failed: {error}")),
    }
    let written = fs::read(&path).map_err(|error| error.to_string())?;
    let after = analyze_bytes(&written)?;
    if !after.insertions.is_empty() {
        return Err("a block still lacks a persisted ID after the write".into());
    }
    if before.blocks.len() != after.blocks.len() {
        return Err(format!(
            "block count changed: {} -> {}",
            before.blocks.len(),
            after.blocks.len()
        ));
    }
    for (index, (old, new)) in before.blocks.iter().zip(&after.blocks).enumerate() {
        if old.kind != new.kind {
            return Err(format!("block {index}: kind changed"));
        }
        if old.text_hash != new.text_hash
            || before.canonical_text(old.id) != after.canonical_text(new.id)
        {
            return Err(format!(
                "block {index}: canonical text or text_hash changed by the write"
            ));
        }
    }
    let rewrite = analyze_bytes(&written)?;
    write_ids_atomic(&path, &written, &rewrite.insertions).map_err(|error| error.to_string())?;
    if fs::read(&path).map_err(|error| error.to_string())? != written {
        return Err("a second --write-ids changed the bytes".into());
    }
    Ok(Some(path))
}

/// Run TeXmacs over `paths` and return one S-expression per path (or `DUMP_ERROR`).
fn texmacs_dump(bin: &Path, scratch: &Path, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let out = scratch.join("dump");
    let home = scratch.join("dump-home");
    let user = scratch.join("dump-user");
    for dir in [&out, &home, &user] {
        fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let list = scratch.join("dump.list");
    let listing: String = paths
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect();
    fs::write(&list, listing).map_err(|error| error.to_string())?;
    let scm = scratch.join("dump.scm");
    fs::write(&scm, DUMP_SCM).map_err(|error| error.to_string())?;
    let log = |name: &str| fs::File::create(scratch.join(name)).map_err(|error| error.to_string());
    let status = Command::new("perl")
        .arg("-e")
        .arg("alarm 240; exec @ARGV")
        .arg("env")
        .arg(format!("HOME={}", user.display()))
        .arg(format!("TEXMACS_HOME_PATH={}", home.display()))
        .arg(format!("DUMP_LIST={}", list.display()))
        .arg(format!("DUMP_OUT={}", out.display()))
        .arg(bin)
        .args(["-H", "-s", "-x"])
        .arg(format!("(load \"{}\")", scm.display()))
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(log("dump.stdout")?)
        .stderr(log("dump.stderr")?)
        .status()
        .map_err(|error| format!("cannot start perl/TeXmacs: {error}"))?;
    if !status.success() {
        return Err(format!("TeXmacs exited with {status}"));
    }
    (0..paths.len())
        .map(|index| {
            fs::read(out.join(format!("{index}.stree")))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| format!("TeXmacs wrote no tree {index}: {error}"))
        })
        .collect()
}

#[derive(Clone, Debug)]
enum Node {
    Sym(String),
    Str(String),
    List(Vec<Node>),
}

fn read_stree(text: &str) -> Option<Node> {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = 0;
    read(&chars, &mut pos)
}

fn read(chars: &[char], pos: &mut usize) -> Option<Node> {
    while chars.get(*pos)?.is_whitespace() {
        *pos += 1;
    }
    match *chars.get(*pos)? {
        '(' => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                while chars.get(*pos)?.is_whitespace() {
                    *pos += 1;
                }
                if *chars.get(*pos)? == ')' {
                    *pos += 1;
                    return Some(Node::List(items));
                }
                items.push(read(chars, pos)?);
            }
        }
        '"' => {
            *pos += 1;
            let mut text = String::new();
            while *chars.get(*pos)? != '"' {
                if chars[*pos] == '\\' {
                    text.push('\\');
                    *pos += 1;
                }
                text.push(*chars.get(*pos)?);
                *pos += 1;
            }
            *pos += 1;
            Some(Node::Str(text))
        }
        '#' if chars.get(*pos + 1) == Some(&'{') => {
            let start = *pos + 2;
            let mut end = start;
            while !(chars.get(end)? == &'}' && chars.get(end + 1)? == &'#') {
                end += 1;
            }
            *pos = end + 2;
            Some(Node::Sym(chars[start..end].iter().collect()))
        }
        _ => {
            let start = *pos;
            while *pos < chars.len()
                && !matches!(chars[*pos], ' ' | '\n' | '\t' | '\r' | '(' | ')' | '"')
            {
                *pos += 1;
            }
            Some(Node::Sym(chars[start..*pos].iter().collect()))
        }
    }
}

fn is_head(node: &Node, label: &str) -> bool {
    matches!(node, Node::List(items) if matches!(items.first(), Some(Node::Sym(s)) if s == label))
}

/// Remove every `mls-id` carrier, which binds nothing else, and flatten `concat` and `document`
/// the way the canonical text does. Two trees with the same text and no other bindings then
/// compare equal, and a carrier that split its body into extra arguments does not.
fn strip(node: &Node) -> Node {
    let Node::List(items) = node else {
        return node.clone();
    };
    let Some(Node::Sym(label)) = items.first() else {
        return Node::List(items.iter().map(strip).collect());
    };
    let args: Vec<Node> = items[1..].iter().map(strip).collect();
    match label.as_str() {
        "with" => {
            if let [Node::Str(binding), _, body] = args.as_slice()
                && binding == "mls-id"
            {
                return body.clone();
            }
            let mut out = vec![Node::Sym("with".into())];
            out.extend(args);
            Node::List(out)
        }
        "concat" | "document" => {
            let mut merged: Vec<Node> = Vec::new();
            for arg in args {
                let parts = if is_head(&arg, label) {
                    match arg {
                        Node::List(inner) => inner.into_iter().skip(1).collect(),
                        other => vec![other],
                    }
                } else {
                    vec![arg]
                };
                for part in parts {
                    match part {
                        Node::Str(text) if text.is_empty() => {}
                        Node::Str(text) => {
                            if let Some(Node::Str(previous)) = merged.last_mut() {
                                previous.push_str(&text);
                            } else {
                                merged.push(Node::Str(text));
                            }
                        }
                        other => merged.push(other),
                    }
                }
            }
            match merged.len() {
                0 => Node::Str(String::new()),
                1 => merged.pop().expect("one item"),
                _ => {
                    let mut out = vec![Node::Sym(label.clone())];
                    out.extend(merged);
                    Node::List(out)
                }
            }
        }
        _ => {
            let mut out = vec![Node::Sym(label.clone())];
            out.extend(args);
            Node::List(out)
        }
    }
}

fn key(node: &Node) -> String {
    match node {
        Node::Sym(text) => format!("y{text:?}"),
        Node::Str(text) => format!("s{text:?}"),
        Node::List(items) => format!("({})", items.iter().map(key).collect::<Vec<_>>().join(" ")),
    }
}

/// The carrier-free tree TeXmacs loaded from one dump, or an error if TeXmacs could not load it.
fn tree_key(dump: &str) -> Result<String, String> {
    if dump.starts_with("DUMP_ERROR") {
        return Err(dump.lines().next().unwrap_or_default().to_owned());
    }
    let node = read_stree(dump).ok_or_else(|| "unreadable tree".to_owned())?;
    Ok(key(&strip(&node)))
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Write each case through the production writer, then compare the TeXmacs trees of the original
/// and the written file. Returns `(compared, refused)` names, or the first failure.
fn compare_trees(
    bin: &Path,
    scratch: &Path,
    cases: &[(String, Vec<u8>)],
) -> Result<(Vec<String>, Vec<String>), Vec<String>> {
    let work = scratch.join("work");
    fs::create_dir_all(&work).expect("work directory");
    let mut failures = Vec::new();
    let mut pairs: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    let mut refused = Vec::new();
    for (index, (name, source)) in cases.iter().enumerate() {
        let layout = canonical_layout(source);
        let original = work.join(format!("case{index:03}.orig.tm"));
        if fs::write(&original, &layout).is_err() {
            failures.push(format!("{name}: cannot write the original"));
            continue;
        }
        match write_and_check(&work, &format!("case{index:03}"), &layout) {
            Ok(Some(written)) => pairs.push((name.clone(), original, written)),
            Ok(None) => refused.push(name.clone()),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    let paths: Vec<PathBuf> = pairs
        .iter()
        .flat_map(|(_, original, written)| [original.clone(), written.clone()])
        .collect();
    let dumps = match texmacs_dump(bin, scratch, &paths) {
        Ok(dumps) => dumps,
        Err(error) => {
            failures.push(error);
            return Err(failures);
        }
    };
    let mut compared = Vec::new();
    for (position, (name, _, _)) in pairs.iter().enumerate() {
        let original = tree_key(&dumps[2 * position]);
        let written = tree_key(&dumps[2 * position + 1]);
        match (original, written) {
            (Ok(before), Ok(after)) if before == after => compared.push(name.clone()),
            (Ok(before), Ok(after)) => failures.push(format!(
                "{name}: TeXmacs tree changed by the write:\n  before: {}\n  after:  {}",
                truncate(&before, 600),
                truncate(&after, 600)
            )),
            (before, after) => failures.push(format!(
                "{name}: TeXmacs could not load a copy: original {:?}, written {:?}",
                before.err(),
                after.err()
            )),
        }
    }
    if failures.is_empty() {
        Ok((compared, refused))
    } else {
        Err(failures)
    }
}

fn source_cases() -> Vec<(String, Vec<u8>)> {
    let mut cases: Vec<(String, Vec<u8>)> = fixture_files()
        .into_iter()
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&path).unwrap())
        })
        .collect();
    for (name, body) in SYNTHETIC_OK.iter().chain(SYNTHETIC_REFUSED) {
        cases.push((
            (*name).to_owned(),
            format!("{HEADER}{body}{FOOTER}").into_bytes(),
        ));
    }
    cases
}

#[test]
fn written_ids_keep_the_texmacs_tree_and_block_text() {
    let Some(bin) = gated_bin() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let mut cases = source_cases();
    for path in installed_sample() {
        let name = format!("installed-{}", path.file_stem().unwrap().to_string_lossy());
        cases.push((name, fs::read(&path).unwrap()));
    }
    let mut cases_to_run = Vec::new();
    for (name, source) in cases {
        match analyze_bytes(&canonical_layout(&source)) {
            Ok(_) => cases_to_run.push((name, source)),
            // Installed documents that do not parse are skipped, as in the roundtrip tests.
            Err(error) if name.starts_with("installed-") => {
                eprintln!("skip {name}: {error}");
            }
            Err(error) => panic!("{name} must parse: {error}"),
        }
    }
    let outcome = compare_trees(&bin, scratch.path(), &cases_to_run);
    let (compared, refused) = match outcome {
        Ok(result) => result,
        Err(failures) => panic!("tree check failures:\n{}", failures.join("\n")),
    };
    eprintln!(
        "writer tree check: {} documents compared, refused: {:?}",
        compared.len(),
        refused
    );
    for name in SYNTHETIC_REFUSED.iter().map(|(name, _)| *name) {
        assert!(
            refused.iter().any(|refused| refused == name),
            "{name} should have been refused"
        );
    }
    for name in SYNTHETIC_OK.iter().map(|(name, _)| *name) {
        assert!(
            compared.iter().any(|compared| compared == name),
            "{name} should have been written and compared"
        );
    }
}

#[test]
fn every_installed_document_is_written_stably() {
    if gated_bin().is_none() {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().join("installed");
    fs::create_dir_all(&work).unwrap();
    let docs = installed_docs();
    let mut written = 0;
    let mut refused = Vec::new();
    let mut failures = Vec::new();
    for (index, path) in docs.iter().enumerate() {
        let source = fs::read(path).unwrap();
        let layout = canonical_layout(&source);
        if analyze_bytes(&layout).is_err() {
            continue;
        }
        match write_and_check(&work, &format!("doc{index:03}"), &layout) {
            Ok(Some(_)) => written += 1,
            Ok(None) => refused.push(path.display().to_string()),
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    eprintln!(
        "installed documents: {} total, {written} written and stable, {} refused: {refused:?}",
        docs.len(),
        refused.len()
    );
    assert!(
        failures.is_empty(),
        "write failures:\n{}",
        failures.join("\n")
    );
}
