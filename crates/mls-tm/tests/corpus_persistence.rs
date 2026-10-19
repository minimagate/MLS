//! Persistence checks over the checked-in official TeXmacs corpus.

use mls_tm::{analyze, parse, write_ids_atomic};
use std::fs;

#[test]
fn official_corpus_ids_are_stable_and_second_write_is_a_noop() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tm");
    for stem in [
        "texmacs-man-environments.en",
        "texmacs-tm-tm.en",
        "texmacs-prim-table.en",
    ] {
        let source = fs::read(fixtures.join(format!("{stem}.tm"))).unwrap();
        let initial = analyze(&parse(&source).unwrap());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{stem}.tm"));
        fs::write(&path, &source).unwrap();
        write_ids_atomic(&path, &source, &initial.insertions).unwrap();

        let once = fs::read(&path).unwrap();
        let reopened = analyze(&parse(&once).unwrap());
        let initial_identity: Vec<_> = initial
            .blocks
            .iter()
            .map(|block| (block.kind, block.id))
            .collect();
        let reopened_identity: Vec<_> = reopened
            .blocks
            .iter()
            .map(|block| (block.kind, block.id))
            .collect();
        assert_eq!(
            reopened_identity, initial_identity,
            "identity mismatch for {stem}"
        );
        assert!(reopened.insertions.is_empty(), "ID churn for {stem}");

        write_ids_atomic(&path, &once, &reopened.insertions).unwrap();
        assert_eq!(fs::read(path).unwrap(), once, "second write changed {stem}");
    }
}

#[test]
fn split_persisted_paragraph_and_formatted_formal_content_remain_stable() {
    let original_id = mls_core::BlockId::new();
    let source = format!(
        "<\\body><\\with|mls-id|{original_id}>First.\n\nSecond.</with><\\with|color|red>Intro.\n\n<theorem|A>\n\nAfter.</with></body>"
    );
    let initial = analyze(&parse(source.as_bytes()).unwrap());
    assert_eq!(initial.blocks.len(), 5);
    assert_eq!(initial.blocks[0].id, original_id);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edited.tm");
    fs::write(&path, source.as_bytes()).unwrap();
    write_ids_atomic(&path, source.as_bytes(), &initial.insertions).unwrap();
    let once = fs::read(&path).unwrap();
    let reopened = analyze(&parse(&once).unwrap());
    assert_eq!(
        reopened
            .blocks
            .iter()
            .map(|block| (block.kind, block.id))
            .collect::<Vec<_>>(),
        initial
            .blocks
            .iter()
            .map(|block| (block.kind, block.id))
            .collect::<Vec<_>>()
    );
    assert!(reopened.insertions.is_empty());
}
