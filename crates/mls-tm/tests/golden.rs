//! Golden block-tree checks against synthetic and official TeXmacs documents.

use mls_core::BlockKind;
use mls_tm::{analyze, parse};
use serde_json::{Value, json};
use std::collections::HashMap;

#[test]
fn parses_checked_in_native_fixture() {
    let source = include_bytes!("../../../tests/fixtures/tm/basic.tm");
    let document = parse(source).unwrap();
    let analysis = analyze(&document);
    let kinds: Vec<_> = analysis.blocks.iter().map(|block| block.kind).collect();
    assert_eq!(
        kinds,
        [
            BlockKind::Section,
            BlockKind::Definition,
            BlockKind::Theorem,
            BlockKind::Proof
        ]
    );
    assert_eq!(analysis.blocks[2].deps, vec![analysis.blocks[1].id]);
    assert_eq!(analysis.blocks[3].deps, vec![analysis.blocks[1].id]);
}

#[test]
fn official_texmacs_corpus_matches_checked_in_block_trees() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tm");
    for stem in [
        "texmacs-man-environments.en",
        "texmacs-tm-tm.en",
        "texmacs-prim-table.en",
    ] {
        let source = std::fs::read(fixtures.join(format!("{stem}.tm"))).unwrap();
        let analysis = analyze(&parse(&source).unwrap());
        let indices: HashMap<_, _> = analysis
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.id, index))
            .collect();
        let actual = json!({
            "blocks": analysis.blocks.iter().map(|block| json!({
                "kind": block.kind,
                "label": block.label,
                "deps": block.deps.iter().map(|id| indices[id]).collect::<Vec<_>>(),
                "source_start": block.source_start,
                "source_end": block.source_end,
            })).collect::<Vec<_>>(),
            "diagnostics": analysis.diagnostics.iter().map(|diagnostic| json!({
                "code": diagnostic.code,
                "source_start": diagnostic.source_start,
                "source_end": diagnostic.source_end,
            })).collect::<Vec<_>>(),
        });
        let expected: Value = serde_json::from_slice(
            &std::fs::read(fixtures.join(format!("{stem}.expected.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(actual, expected, "golden mismatch for {stem}");
    }
}
