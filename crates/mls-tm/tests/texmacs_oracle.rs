//! Oracle check of the canonical text against TeXmacs' own reader. Gated: set `MLS_TEXMACS_TESTS=1`
//! (skipped otherwise).
//!
//! TeXmacs parses each sample with `parse-texmacs-snippet`, headless, through the scheme file below.
//! Two samples that TeXmacs reads as different trees must never get the same canonical text. Trees
//! are compared after `mls-id` carriers are removed and `document` and `concat` are flattened, as
//! the canonical text does. A sample whose canonical text is refused, or that TeXmacs rejects, is
//! not compared. TeXmacs runs with a scratch `TEXMACS_HOME_PATH` and `HOME`, under
//! `perl -e 'alarm 120; exec @ARGV'`. `MLS_TEXMACS_BIN` overrides the binary path.

use mls_tm::{canonical_block_text, parse};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

mod texmacs_common;

use texmacs_common::gated_bin;

const UUID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const RANDOM_SAMPLES: usize = 2000;
const SEPARATOR: u8 = 0x1e;

/// Reads each `0x1e`-separated sample, parses it with `parse-texmacs-snippet`, and writes the
/// trees as S-expressions, one record per sample. A rejected sample is `ORACLE_ERROR`.
const ORACLE_SCM: &str = r#"(define (oracle-run in out)
  (let* ((data (string-load in))
         (parts (string-split data (integer->char 30)))
         (results
          (map (lambda (s)
                 (catch #t
                   (lambda ()
                     (object->string (tree->stree (parse-texmacs-snippet s))))
                   (lambda args "ORACLE_ERROR")))
               parts)))
    (string-save (apply string-append
                        (cdr (apply append (map (lambda (r) (list (string (integer->char 30)) r)) results))))
                 out)))
(catch #t
  (lambda () (oracle-run (getenv "ORACLE_IN") (getenv "ORACLE_OUT")))
  (lambda args (display "ORACLE_FAILED") (newline)))
"#;

/// Tokens of the random sources: escapes, separators, bare angle brackets, odd tag names, raw data,
/// carriers, and line endings.
const TOKENS: &[&str] = &[
    "a",
    "b",
    "xy",
    " ",
    "  ",
    "\n",
    "\n\n",
    "\r",
    "\r\n",
    "\t",
    "|",
    ">",
    "<",
    "</",
    "<|",
    "\\",
    "\\\\",
    "\\ ",
    "\\\n",
    "\\<",
    "\\|",
    "\\>",
    "\\;",
    "\\n",
    "\\t",
    "\\0",
    "\\@",
    "\\M",
    "\\A",
    "\\x0a",
    "_",
    "#",
    "<#4a>",
    "<#4A>",
    "<#41|",
    "<#4",
    "<theorem|",
    "<\\theorem>",
    "</theorem>",
    "<em|",
    "<label|x>",
    "<reference|x>",
    "<with|mls-id|0f8fad5b-d9cb-469f-a165-70867728950e|",
    "<with|font-series|bold|",
    "<\\",
    "<a",
    "<a\\ b|",
    "<\\a>",
    "</a>",
    "\u{e9}",
    "\u{1}",
    "@",
    "<|theorem>",
    "</with>",
    "<theorem|>",
    "<1>",
    "<\\\n  b>",
    "\\\t",
];

/// Prefix and suffix that give a random body a plausible enclosing tag.
const WRAPPERS: &[(&str, &str)] = &[
    ("", ""),
    ("", ""),
    ("<\\theorem>", "</theorem>"),
    ("<theorem|", ">"),
    ("<\\theorem>x<|theorem>", "</theorem>"),
    ("<\\body>", "</body>"),
    ("<em|", ">"),
];

/// Pairs whose TeXmacs trees differ and whose canonical texts must differ. Each one is a case the
/// oracle found before the canonical text was fixed.
fn regression_pairs() -> Vec<(String, String)> {
    vec![
        (
            r"<\theorem>\@<label|thm:a>_</theorem>".into(),
            r"<\theorem>\@\<label|thm:a|_</theorem>".into(),
        ),
        (r"<a<b|c>".into(), r"<a\<b|c>".into()),
        (
            format!("<\\theorem>A\n\n<with|mls-id|{UUID}|></theorem>"),
            "<\\theorem>A</theorem>".into(),
        ),
        (
            format!("<\\theorem>x<with|mls-id|{UUID}|A\n\nB>y</theorem>"),
            "<\\theorem>xA\n\nBy</theorem>".into(),
        ),
        (
            format!("<\\theorem>x<with|mls-id|{UUID}|y > </theorem>"),
            "<\\theorem>xy </theorem>".into(),
        ),
        (
            "<\\theorem>\n    |x\n  </theorem>".into(),
            format!("<\\theorem>\n    <\\with|mls-id|{UUID}>|</with>x\n  </theorem>"),
        ),
        (
            "<\\theorem><#4a>x</theorem>".into(),
            "<\\theorem>x</theorem>".into(),
        ),
        (
            "<\\theorem>a\n\n\\\n\nb</theorem>".into(),
            "<\\theorem>a b</theorem>".into(),
        ),
        (
            "<\\theorem><w\\\n      ith|mode|math|x></theorem>".into(),
            "<\\theorem><w ith|mode|math|x></theorem>".into(),
        ),
    ]
}

/// Deterministic xorshift generator, so the sweep is the same on every run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, count: usize) -> usize {
        usize::try_from(self.next_u64() % count as u64).expect("index fits in usize")
    }
}

fn random_source(rng: &mut Rng) -> String {
    let (prefix, suffix) = WRAPPERS[rng.below(WRAPPERS.len())];
    let mut body = String::new();
    for _ in 0..=rng.below(12) {
        body.push_str(TOKENS[rng.below(TOKENS.len())]);
    }
    format!("{prefix}{body}{suffix}")
}

/// Every source of the sweep, each wrapped in `<\wrap>…</wrap>`.
fn sources() -> Vec<String> {
    let mut pairs: Vec<String> = regression_pairs()
        .into_iter()
        .flat_map(|(left, right)| [left, right])
        .collect();
    let mut seen: HashSet<String> = pairs.iter().cloned().collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    while pairs.len() < RANDOM_SAMPLES + 2 * regression_pairs().len() {
        let source = random_source(&mut rng);
        if !source.contains(char::from(SEPARATOR)) && seen.insert(source.clone()) {
            pairs.push(source);
        }
    }
    pairs
        .into_iter()
        .map(|source| format!("<\\wrap>{source}</wrap>"))
        .collect()
}

/// Run the scheme oracle over `input` and return its output bytes.
fn run_oracle(bin: &Path, scratch: &Path, input: &Path) -> Result<Vec<u8>, String> {
    let scm = scratch.join("oracle.scm");
    let output = scratch.join("oracle.out");
    let home = scratch.join("home");
    let user = scratch.join("user");
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&user).map_err(|error| error.to_string())?;
    fs::write(&scm, ORACLE_SCM).map_err(|error| error.to_string())?;
    let log = |name: &str| fs::File::create(scratch.join(name)).map_err(|error| error.to_string());
    let status = Command::new("perl")
        .arg("-e")
        .arg("alarm 120; exec @ARGV")
        .arg("env")
        .arg(format!("HOME={}", user.display()))
        .arg(format!("TEXMACS_HOME_PATH={}", home.display()))
        .arg(format!("ORACLE_IN={}", input.display()))
        .arg(format!("ORACLE_OUT={}", output.display()))
        .arg(bin)
        .args(["-H", "-s", "-x"])
        .arg(format!("(load \"{}\")", scm.display()))
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(log("oracle.stdout")?)
        .stderr(log("oracle.stderr")?)
        .status()
        .map_err(|error| format!("cannot start perl/TeXmacs: {error}"))?;
    if !status.success() {
        return Err(format!("TeXmacs exited with {status}"));
    }
    fs::read(&output).map_err(|error| format!("TeXmacs wrote no output: {error}"))
}

/// A parsed S-expression of TeXmacs' `tree->stree` output.
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

fn is_str(node: &Node, text: &str) -> bool {
    matches!(node, Node::Str(s) if s == text)
}

/// Children of a node with the given head, or the node itself.
fn flatten(label: &str, args: Vec<Node>) -> Vec<Node> {
    let mut out = Vec::new();
    for arg in args {
        if is_head(&arg, label) {
            if let Node::List(items) = arg {
                out.extend(items.into_iter().skip(1));
            }
        } else {
            out.push(arg);
        }
    }
    out
}

/// The tree with a transparent identity carrier removed, and with `document` and `concat`
/// flattened the way the canonical text joins them.
fn normalize(node: &Node) -> Node {
    let Node::List(items) = node else {
        return node.clone();
    };
    let Some(Node::Sym(label)) = items.first() else {
        return Node::List(items.iter().map(normalize).collect());
    };
    let args: Vec<Node> = items[1..].iter().map(normalize).collect();
    match label.as_str() {
        "with" => carrier(args),
        "concat" => {
            let mut merged: Vec<Node> = Vec::new();
            for node in flatten("concat", args) {
                if let Node::Str(text) = &node {
                    if text.is_empty() {
                        continue;
                    }
                    if let Some(Node::Str(previous)) = merged.last_mut() {
                        previous.push_str(text);
                        continue;
                    }
                }
                merged.push(node);
            }
            join("concat", merged)
        }
        "document" => join("document", flatten("document", args)),
        _ => {
            let mut out = vec![Node::Sym(label.clone())];
            out.extend(args);
            Node::List(out)
        }
    }
}

/// A `with` node whose only binding is `mls-id` with the test UUID is its body; any other
/// bindings stay.
fn carrier(args: Vec<Node>) -> Node {
    let count = args.len();
    if count >= 3 && is_str(&args[0], "mls-id") && is_str(&args[1], UUID) {
        let mut removed = vec![false; count];
        for pair in 0..(count - 1) / 2 {
            if is_str(&args[2 * pair], "mls-id") {
                removed[2 * pair] = true;
                removed[2 * pair + 1] = true;
            }
        }
        let kept: Vec<Node> = args
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !removed[*index])
            .map(|(_, node)| node)
            .collect();
        if kept.len() == 1 {
            return kept.into_iter().next().expect("one kept node");
        }
        let mut out = vec![Node::Sym("with".into())];
        out.extend(kept);
        return Node::List(out);
    }
    let mut out = vec![Node::Sym("with".into())];
    out.extend(args);
    Node::List(out)
}

/// Rebuild a `concat` or `document` node from its children, collapsing the trivial cases.
fn join(label: &str, mut items: Vec<Node>) -> Node {
    match items.len() {
        0 => Node::Str(String::new()),
        1 => items.pop().expect("one item"),
        _ => {
            let mut out = vec![Node::Sym(label.into())];
            out.extend(items);
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

/// The normalized tree of the `wrap` a sample holds, or `None` when TeXmacs rejected it or the
/// wrapper is not the first top-level tree.
fn texmacs_key(record: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(record);
    let Node::List(top) = read_stree(&text)? else {
        return None;
    };
    let [Node::Sym(head), first, ..] = top.as_slice() else {
        return None;
    };
    if head != "document" {
        return None;
    }
    let first = match first {
        Node::List(inner) if is_head(first, "concat") => inner.get(1)?,
        other => other,
    };
    is_head(first, "wrap").then(|| key(&normalize(first)))
}

/// The canonical text of the sample's leading tag, as a block span would give it.
fn canonical_key(sample: &[u8]) -> Option<Vec<u8>> {
    let document = parse(sample).ok()?;
    match document.nodes().first()? {
        mls_tm::Node::Tag { span, .. } => canonical_block_text(&sample[span.start..span.end]).ok(),
        mls_tm::Node::Text(_) => None,
    }
}

#[test]
fn texmacs_trees_that_differ_have_different_canonical_text() {
    let Some(bin) = gated_bin() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let samples = sources();
    let input = scratch.path().join("samples.bin");
    let joined: Vec<u8> = samples
        .iter()
        .map(|sample| sample.as_bytes().to_vec())
        .collect::<Vec<_>>()
        .join(&SEPARATOR);
    fs::write(&input, joined).unwrap();
    let output = run_oracle(&bin, scratch.path(), &input).unwrap();
    let records: Vec<&[u8]> = output.split(|&byte| byte == SEPARATOR).collect();
    assert_eq!(
        records.len(),
        samples.len(),
        "one TeXmacs record per sample"
    );

    let mut by_canonical: HashMap<Vec<u8>, (String, &str)> = HashMap::new();
    let mut failures = Vec::new();
    let (mut compared, mut canonical_refused) = (0, 0);
    let mut tree_of = HashMap::new();
    for (sample, record) in samples.iter().zip(&records) {
        let Some(tree) = texmacs_key(record) else {
            continue;
        };
        tree_of.insert(sample.clone(), tree.clone());
        let Some(canonical) = canonical_key(sample.as_bytes()) else {
            canonical_refused += 1;
            continue;
        };
        compared += 1;
        match by_canonical.get(&canonical) {
            Some((other_tree, other_sample)) if *other_tree != tree => failures.push(format!(
                "same canonical text {:?} for\n  {other_sample}\n  {sample}\nTeXmacs trees differ",
                String::from_utf8_lossy(&canonical)
            )),
            Some(_) => {}
            None => {
                by_canonical.insert(canonical, (tree, sample.as_str()));
            }
        }
    }

    // The regression pairs must be real: TeXmacs reads them differently, and their canonical texts
    // differ (a refused text counts as different from any text).
    for (left, right) in regression_pairs() {
        let (left, right) = (
            format!("<\\wrap>{left}</wrap>"),
            format!("<\\wrap>{right}</wrap>"),
        );
        let (Some(left_tree), Some(right_tree)) = (tree_of.get(&left), tree_of.get(&right)) else {
            failures.push(format!("regression pair not compared: {left} / {right}"));
            continue;
        };
        if left_tree == right_tree {
            failures.push(format!(
                "regression pair reads the same in TeXmacs: {left} / {right}"
            ));
        }
        if canonical_key(left.as_bytes()) == canonical_key(right.as_bytes()) {
            failures.push(format!(
                "regression pair has equal canonical text: {left} / {right}"
            ));
        }
    }

    eprintln!(
        "texmacs oracle: {} samples, {compared} compared, {canonical_refused} refused by canonical, {} canonical texts",
        samples.len(),
        by_canonical.len()
    );
    assert!(
        failures.is_empty(),
        "oracle failures:\n{}",
        failures.join("\n")
    );
    assert!(compared > 0);
}
