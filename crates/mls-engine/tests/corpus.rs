//! Offline integrity checks for the hand-authored evaluation reference corpus.

use mls_sir::Sir;
use mls_tm::{analyze, parse};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};

#[derive(Deserialize)]
struct Corpus {
    version: u32,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    block_id: String,
    expected_sir: Value,
}

#[test]
fn reference_sirs_match_fixed_source_identities() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let corpus: Corpus =
        serde_json::from_slice(&fs::read(root.join("evals/cases.json")).unwrap()).unwrap();
    assert_eq!(corpus.version, 1);
    assert_eq!(corpus.cases.len(), 10);
    let source = fs::read(root.join("evals/statements.tm")).unwrap();
    let analysis = analyze(&parse(&source).unwrap());
    let blocks: HashMap<_, _> = analysis
        .blocks
        .iter()
        .map(|block| (block.id.to_string(), block))
        .collect();
    for case in corpus.cases {
        let expected: Sir = serde_json::from_value(case.expected_sir).unwrap();
        let block = blocks
            .get(&case.block_id)
            .unwrap_or_else(|| panic!("missing fixed block {}", case.block_id));
        assert_eq!(Some(expected.kind()), source_kind(block.kind));
        assert_eq!(expected.label(), block.label.as_deref());
    }
}

fn source_kind(kind: mls_core::BlockKind) -> Option<mls_sir::StatementKind> {
    use mls_core::BlockKind;
    use mls_sir::StatementKind;
    match kind {
        BlockKind::Definition => Some(StatementKind::Definition),
        BlockKind::Theorem => Some(StatementKind::Theorem),
        BlockKind::Lemma => Some(StatementKind::Lemma),
        BlockKind::Proposition => Some(StatementKind::Proposition),
        BlockKind::Corollary => Some(StatementKind::Corollary),
        _ => None,
    }
}
