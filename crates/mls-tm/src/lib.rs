//! Lossless parsing, block extraction, and ID insertion for TeXmacs native syntax.

mod canonical;
mod parser;
mod writer;

pub use parser::{Document, Node, ParseError, Span, parse};
pub use writer::{WriteError, write_ids_atomic};

use mls_core::{Block, BlockId, BlockKind, Diagnostic, source_hash};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// A parsed document's blocks and deterministic linking diagnostics.
#[derive(Clone, Debug, Serialize)]
pub struct Analysis {
    /// Extracted blocks in source order.
    pub blocks: Vec<Block>,
    /// Non-fatal identity and dependency diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Planned source wrappers for blocks that do not yet have persisted IDs.
    #[serde(skip)]
    pub insertions: Vec<IdInsertion>,
    /// Canonical text of each block, keyed by block ID. Read it with [`Analysis::canonical_text`].
    #[serde(skip)]
    pub canonical: HashMap<BlockId, Vec<u8>>,
}

impl Analysis {
    /// Return the layout-independent canonical text of a block, or `None` for an unknown ID or
    /// for a block whose canonicalization failed (reported as `canonicalization_failed`).
    ///
    /// The text is what `Block::text_hash` hashes and what model prompts receive. It does not
    /// change when TeXmacs re-serializes the block (indentation, line breaks, short or long
    /// form), and it omits `mls-id` carriers. Source spans still locate the raw bytes.
    #[must_use]
    pub fn canonical_text(&self, id: BlockId) -> Option<&[u8]> {
        self.canonical.get(&id).map(Vec::as_slice)
    }
}

/// Canonical text of one block's source slice, as [`Analysis::canonical_text`] reports it.
///
/// The slice is parsed on its own. This is exposed for the TeXmacs oracle tests, which compare
/// it with TeXmacs' own reader; production code reads [`Analysis::canonical_text`].
///
/// # Errors
///
/// Returns [`ParseError`] when the slice has no canonical form.
#[doc(hidden)]
pub fn canonical_block_text(slice: &[u8]) -> Result<Vec<u8>, ParseError> {
    canonical::block_text(slice)
}

/// An ID wrapper to insert around an unannotated source span.
#[derive(Clone, Debug)]
pub struct IdInsertion {
    /// Fresh identity to persist.
    pub id: BlockId,
    /// Exact block span to wrap.
    pub span: Span,
}

#[derive(Clone)]
struct Candidate {
    id: Option<BlockId>,
    span: Span,
    kind: BlockKind,
    label: Option<String>,
    labels: Vec<String>,
    refs: Vec<(String, Span)>,
}

/// Extract blocks, assign missing IDs in memory, and resolve explicit references.
#[must_use]
pub fn analyze(document: &Document<'_>) -> Analysis {
    let mut candidates = Vec::new();
    let body = document
        .nodes()
        .iter()
        .find(|node| node.name() == Some(b"body"));
    if let Some(body) = body {
        collect_candidates(body.children(), document.source(), None, &mut candidates);
        collect_body_prose(body.children(), document.source(), &mut candidates);
        candidates.sort_by_key(|candidate| candidate.span.start);
    }
    let mut diagnostics = Vec::new();
    if body.is_none() {
        diagnostics.push(Diagnostic {
            code: "missing_body".into(),
            message: "document has no TeXmacs body".into(),
            source_start: 0,
            source_end: document.source().len(),
        });
    }
    collect_invalid_ids(document.nodes(), document.source(), &mut diagnostics);
    let mut used = HashSet::new();
    let mut insertions = Vec::new();
    for candidate in &mut candidates {
        if candidate.id.is_some_and(|id| !used.insert(id)) {
            let old = candidate.id.take().expect("checked above");
            diagnostics.push(Diagnostic {
                code: "duplicate_id".into(),
                message: format!("duplicate block ID {old}; assigned a fresh ID"),
                source_start: candidate.span.start,
                source_end: candidate.span.end,
            });
        }
        if candidate.id.is_none() {
            let id = BlockId::new();
            used.insert(id);
            candidate.id = Some(id);
            insertions.push(IdInsertion {
                id,
                span: candidate.span,
            });
        }
    }

    let mut labels = HashMap::new();
    let mut duplicate_labels = HashSet::new();
    for candidate in &candidates {
        for label in &candidate.labels {
            if labels
                .insert(label.clone(), candidate.id.unwrap())
                .is_some()
            {
                duplicate_labels.insert(label.clone());
                diagnostics.push(Diagnostic {
                    code: "duplicate_label".into(),
                    message: format!("duplicate label {label:?}; references to it are ambiguous"),
                    source_start: candidate.span.start,
                    source_end: candidate.span.end,
                });
            }
        }
    }
    let mut canonical_texts = HashMap::new();
    let blocks = candidates
        .iter()
        .map(|candidate| {
            let mut deps = Vec::new();
            for (reference, span) in &candidate.refs {
                if duplicate_labels.contains(reference) {
                    diagnostics.push(Diagnostic {
                        code: "ambiguous_reference".into(),
                        message: format!("reference {reference:?} has duplicate targets"),
                        source_start: span.start,
                        source_end: span.end,
                    });
                } else if let Some(id) = labels.get(reference) {
                    if !deps.contains(id) {
                        deps.push(*id);
                    }
                } else {
                    diagnostics.push(Diagnostic {
                        code: "unresolved_reference".into(),
                        message: format!("unresolved reference {reference:?}"),
                        source_start: span.start,
                        source_end: span.end,
                    });
                }
            }
            let span = candidate.span;
            let source = &document.source()[span.start..span.end];
            // A block span comes from a successful parse, so this normally succeeds. When it does
            // not, the block has no canonical text: it is reported, and it is never hashed from
            // raw bytes, which could then equal the hash of a different tree.
            let text_hash = match canonical::block_text(source) {
                Ok(canonical) => {
                    let hash = source_hash(&canonical);
                    canonical_texts.insert(candidate.id.unwrap(), canonical);
                    hash
                }
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        code: "canonicalization_failed".into(),
                        message: format!("block text has no canonical form: {error}"),
                        source_start: span.start,
                        source_end: span.end,
                    });
                    source_hash(&[b"canonicalization_failed\0".as_slice(), source].concat())
                }
            };
            Block {
                id: candidate.id.unwrap(),
                kind: candidate.kind,
                text_hash,
                label: candidate.label.clone(),
                deps,
                source_start: span.start,
                source_end: span.end,
            }
        })
        .collect();

    Analysis {
        blocks,
        diagnostics,
        insertions,
        canonical: canonical_texts,
    }
}

fn collect_candidates(
    nodes: &[Node],
    source: &[u8],
    inherited_id: Option<BlockId>,
    out: &mut Vec<Candidate>,
) {
    for node in nodes {
        if matches!(node, Node::Text(_)) {
            continue;
        }
        if matches!(
            node.name(),
            Some(b"inactive" | b"initial" | b"references" | b"collection" | b"doc-data")
        ) {
            continue;
        }
        let wrapper_id = with_id(node, source);
        let carried_id = wrapper_id.or(inherited_id);
        if wrapper_id.is_some() {
            collect_candidates(node.children(), source, carried_id, out);
            continue;
        }
        if let Some(kind) = node_kind(node.name()) {
            let mut label = None;
            let mut labels = Vec::new();
            let mut refs = Vec::new();
            for argument in node.args() {
                collect_links(argument, source, &mut label, &mut refs);
                collect_labels(argument, source, &mut labels);
            }
            out.push(Candidate {
                id: carried_id,
                span: node.span(),
                kind,
                label,
                labels,
                refs,
            });
            for argument in node.args() {
                collect_candidates(argument, source, None, out);
            }
            continue;
        }
        collect_candidates(node.children(), source, carried_id, out);
    }
}

fn is_useful_prose(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|byte| !byte.is_ascii_whitespace() && *byte != b';' && *byte != b'\\')
}

fn collect_body_prose(nodes: &[Node], source: &[u8], out: &mut Vec<Candidate>) {
    collect_prose_sequence(nodes, source, None, out);
}

fn collect_prose_sequence(
    nodes: &[Node],
    source: &[u8],
    inherited_id: Option<BlockId>,
    out: &mut Vec<Candidate>,
) {
    let mut run: Vec<&Node> = Vec::new();
    let mut available_id = inherited_id;
    let flush =
        |run: &mut Vec<&Node>, available_id: &mut Option<BlockId>, out: &mut Vec<Candidate>| {
            if let (Some(first), Some(last)) = (run.first(), run.last()) {
                let whole = Span {
                    start: first.span().start,
                    end: last.span().end,
                };
                for span in paragraph_spans(source, whole, run) {
                    let mut label = None;
                    let mut labels = Vec::new();
                    let mut refs = Vec::new();
                    for node in run.iter().filter(|node| spans_overlap(node.span(), span)) {
                        collect_links(std::slice::from_ref(node), source, &mut label, &mut refs);
                        collect_labels(std::slice::from_ref(node), source, &mut labels);
                    }
                    out.push(Candidate {
                        id: available_id.take(),
                        span,
                        kind: BlockKind::Commentary,
                        label,
                        labels,
                        refs,
                    });
                }
            }
            run.clear();
        };
    for node in nodes {
        if node_kind(node.name()).is_some() {
            flush(&mut run, &mut available_id, out);
        } else if contains_formal_block(node) {
            flush(&mut run, &mut available_id, out);
            collect_prose_sequence(node.children(), source, available_id.take(), out);
        } else if let Some(id) = with_id(node, source) {
            flush(&mut run, &mut available_id, out);
            collect_prose_sequence(node.children(), source, Some(id), out);
        } else {
            run.push(node);
        }
    }
    flush(&mut run, &mut available_id, out);
}

fn spans_overlap(left: Span, right: Span) -> bool {
    left.start < right.end && right.start < left.end
}

fn paragraph_spans(
    source: &[u8],
    span: Span,
    nodes: &[impl std::borrow::Borrow<Node>],
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut start = span.start;
    for node in nodes {
        if let Node::Text(text) = node.borrow() {
            let mut cursor = text.start;
            while cursor < text.end {
                let first_break = line_break_len(source, cursor, text.end);
                let mut second = cursor + first_break;
                while second < text.end && matches!(source[second], b' ' | b'\t') {
                    second += 1;
                }
                let second_break = line_break_len(source, second, text.end);
                if first_break > 0 && second_break > 0 && !is_backslash_escaped(source, cursor) {
                    push_trimmed_span(source, Span { start, end: cursor }, &mut spans);
                    cursor = second + second_break;
                    while cursor < text.end && source[cursor].is_ascii_whitespace() {
                        cursor += 1;
                    }
                    start = cursor;
                } else {
                    cursor += 1;
                }
            }
        }
    }
    push_trimmed_span(
        source,
        Span {
            start,
            end: span.end,
        },
        &mut spans,
    );
    spans
}

fn line_break_len(source: &[u8], offset: usize, end: usize) -> usize {
    match source.get(offset..end) {
        Some(bytes) if bytes.starts_with(b"\r\n") => 2,
        Some([b'\n' | b'\r', ..]) => 1,
        _ => 0,
    }
}

fn push_trimmed_span(source: &[u8], mut span: Span, spans: &mut Vec<Span>) {
    while span.start < span.end && source[span.start].is_ascii_whitespace() {
        span.start += 1;
    }
    while span.end > span.start
        && source[span.end - 1].is_ascii_whitespace()
        && !is_backslash_escaped(source, span.end - 1)
    {
        span.end -= 1;
    }
    if is_useful_prose(&source[span.start..span.end]) {
        spans.push(span);
    }
}

fn is_backslash_escaped(source: &[u8], offset: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = offset;
    while cursor > 0 && source[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn contains_formal_block(node: &Node) -> bool {
    node.args()
        .iter()
        .flatten()
        .any(|child| node_kind(child.name()).is_some() || contains_formal_block(child))
}

fn collect_invalid_ids(nodes: &[Node], source: &[u8], diagnostics: &mut Vec<Diagnostic>) {
    for node in nodes {
        if node.name() == Some(b"with")
            && has_id_key(node, source)
            && with_id(node, source).is_none()
        {
            let span = node.span();
            diagnostics.push(Diagnostic {
                code: "invalid_id".into(),
                message: "mls-id metadata is not a valid UUID".into(),
                source_start: span.start,
                source_end: span.end,
            });
        }
        for argument in node.args() {
            collect_invalid_ids(argument, source, diagnostics);
        }
    }
}

fn with_id(node: &Node, source: &[u8]) -> Option<BlockId> {
    if node.name() != Some(b"with") || node.args().len() < 3 {
        return None;
    }
    node.args()[..node.args().len() - 1]
        .as_chunks::<2>()
        .0
        .iter()
        .find_map(|pair| {
            (plain_bytes(&pair[0], source)? == b"mls-id")
                .then(|| {
                    std::str::from_utf8(plain_bytes(&pair[1], source)?)
                        .ok()?
                        .parse()
                        .ok()
                })
                .flatten()
        })
}

fn has_id_key(node: &Node, source: &[u8]) -> bool {
    node.args()
        .get(..node.args().len().saturating_sub(1))
        .is_some_and(|args| {
            args.as_chunks::<2>()
                .0
                .iter()
                .any(|pair| plain_bytes(&pair[0], source) == Some(b"mls-id"))
        })
}

fn collect_links(
    nodes: &[Node],
    source: &[u8],
    label: &mut Option<String>,
    refs: &mut Vec<(String, Span)>,
) {
    for node in nodes {
        if node_kind(node.name()).is_some() {
            continue;
        }
        match node.name() {
            Some(b"label") if label.is_none() => {
                *label = node
                    .args()
                    .first()
                    .and_then(|arg| plain_string(arg, source));
            }
            Some(b"reference" | b"pageref") => {
                if let Some(value) = node
                    .args()
                    .first()
                    .and_then(|arg| plain_string(arg, source))
                {
                    refs.push((value, node.span()));
                }
            }
            Some(b"hlink") => {
                if let Some(value) = node
                    .args()
                    .get(1)
                    .and_then(|arg| plain_string(arg, source))
                    .and_then(|value| value.strip_prefix('#').map(str::to_owned))
                {
                    refs.push((value, node.span()));
                }
            }
            _ => {}
        }
        for argument in node.args() {
            collect_links(argument, source, label, refs);
        }
    }
}

fn collect_labels(nodes: &[Node], source: &[u8], labels: &mut Vec<String>) {
    for node in nodes {
        if node_kind(node.name()).is_some() {
            continue;
        }
        if node.name() == Some(b"label")
            && let Some(value) = node
                .args()
                .first()
                .and_then(|arg| plain_string(arg, source))
        {
            labels.push(value);
        }
        for argument in node.args() {
            collect_labels(argument, source, labels);
        }
    }
}

fn plain_string(nodes: &[Node], source: &[u8]) -> Option<String> {
    String::from_utf8(plain_bytes(nodes, source)?.to_vec()).ok()
}

fn plain_bytes<'a>(nodes: &[Node], source: &'a [u8]) -> Option<&'a [u8]> {
    if let [Node::Text(span)] = nodes {
        Some(&source[span.start..span.end])
    } else {
        None
    }
}

fn node_kind(name: Option<&[u8]>) -> Option<BlockKind> {
    let name = name?;
    let name = name.strip_suffix(b"*").unwrap_or(name);
    Some(match name {
        b"definition" => BlockKind::Definition,
        b"theorem" => BlockKind::Theorem,
        b"lemma" => BlockKind::Lemma,
        b"proposition" => BlockKind::Proposition,
        b"corollary" => BlockKind::Corollary,
        b"proof" => BlockKind::Proof,
        b"equation" | b"eqnarray" => BlockKind::Equation,
        b"section" | b"subsection" | b"subsubsection" => BlockKind::Section,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_labels_and_explicit_dependencies() {
        let source = br#"<\body><\definition>Topological.<label|def:top></definition><\theorem>Use <reference|def:top>.<label|thm:use></theorem></body>"#;
        let document = parse(source).unwrap();
        let analysis = analyze(&document);
        assert_eq!(analysis.blocks.len(), 2);
        assert_eq!(analysis.blocks[0].kind, BlockKind::Definition);
        assert_eq!(analysis.blocks[1].deps, vec![analysis.blocks[0].id]);
        assert!(analysis.diagnostics.is_empty());
    }

    #[test]
    fn reads_persistent_id_wrapper() {
        let id = BlockId::new();
        let source = format!("<\\body><\\with|mls-id|{id}><\\theorem>T</theorem></with></body>");
        let document = parse(source.as_bytes()).unwrap();
        assert_eq!(analyze(&document).blocks[0].id, id);
    }

    #[test]
    fn groups_formatted_prose_and_links_inline_reference() {
        let source = br"<\body><\definition>D<label|def:x></definition>Let <with|font-series|bold|x> be real. By <reference|def:x>.</body>";
        let analysis = analyze(&parse(source).unwrap());
        assert_eq!(analysis.blocks.len(), 2);
        assert_eq!(analysis.blocks[1].kind, BlockKind::Commentary);
        assert_eq!(analysis.blocks[1].deps, vec![analysis.blocks[0].id]);
    }

    #[test]
    fn missing_body_does_not_extract_header_metadata() {
        let source = b"<TeXmacs|2.1.4><style|generic>";
        let analysis = analyze(&parse(source).unwrap());
        assert!(analysis.blocks.is_empty());
        assert_eq!(analysis.diagnostics[0].code, "missing_body");
    }

    #[test]
    fn recognizes_later_id_binding_and_reports_invalid_uuid() {
        let id = BlockId::new();
        let source = format!(
            "<\\body><with|font-series|bold|mls-id|{id}|<\\theorem>T</theorem>><with|mls-id|bad|text></body>"
        );
        let analysis = analyze(&parse(source.as_bytes()).unwrap());
        assert_eq!(analysis.blocks[0].id, id);
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "invalid_id")
        );
    }

    #[test]
    fn every_label_alias_resolves_to_the_same_block() {
        let source = br"<\body><\definition>D<label|one><label|two></definition><\theorem><reference|one><reference|two></theorem></body>";
        let analysis = analyze(&parse(source).unwrap());
        assert_eq!(analysis.blocks[1].deps, vec![analysis.blocks[0].id]);
    }

    #[test]
    fn prose_paragraphs_exclude_blank_line_separators_and_stay_in_source_order() {
        let source = b"<\\body>First.\n\nSecond.\n\n<\\definition>D</definition></body>";
        let analysis = analyze(&parse(source).unwrap());
        assert_eq!(analysis.blocks.len(), 3);
        assert_eq!(analysis.blocks[0].kind, BlockKind::Commentary);
        assert_eq!(analysis.blocks[1].kind, BlockKind::Commentary);
        assert_eq!(analysis.blocks[2].kind, BlockKind::Definition);
        assert_eq!(
            &source[analysis.blocks[0].source_start..analysis.blocks[0].source_end],
            b"First."
        );
        assert_eq!(
            &source[analysis.blocks[1].source_start..analysis.blocks[1].source_end],
            b"Second."
        );
    }

    #[test]
    fn paragraph_boundaries_do_not_split_escaped_whitespace_atoms() {
        let source = b"<\\body>Explicit\\ \n\nNext. Escaped newline\\\ncontinues.\n\nLast.</body>";
        let analysis = analyze(&parse(source).unwrap());
        assert_eq!(analysis.blocks.len(), 3);
        assert_eq!(
            &source[analysis.blocks[0].source_start..analysis.blocks[0].source_end],
            b"Explicit\\ "
        );
        assert_eq!(
            &source[analysis.blocks[1].source_start..analysis.blocks[1].source_end],
            b"Next. Escaped newline\\\ncontinues."
        );
    }

    #[test]
    fn paragraph_boundaries_accept_crlf_and_whitespace_only_blank_lines() {
        for body in [
            b"First.\r\n\r\nSecond.".as_slice(),
            b"First.\n  \t\nSecond.".as_slice(),
        ] {
            let mut source = b"<\\body>".to_vec();
            source.extend_from_slice(body);
            source.extend_from_slice(b"</body>");
            let analysis = analyze(&parse(&source).unwrap());
            assert_eq!(analysis.blocks.len(), 2);
            assert_eq!(
                &source[analysis.blocks[0].source_start..analysis.blocks[0].source_end],
                b"First."
            );
            assert_eq!(
                &source[analysis.blocks[1].source_start..analysis.blocks[1].source_end],
                b"Second."
            );
        }
    }

    #[test]
    fn a_line_join_inside_a_block_tag_name_names_the_block() {
        let source = b"<\\body><\\the\\\n   orem>x</theorem></body>";
        let analysis = analyze(&parse(source).unwrap());
        assert_eq!(analysis.blocks.len(), 1);
        assert_eq!(analysis.blocks[0].kind, BlockKind::Theorem);
    }

    #[test]
    fn block_without_canonical_text_is_reported_and_has_none() {
        let source = "<\\body><\\theorem>x<with|mls-id|0f8fad5b-d9cb-469f-a165-70867728950e|></theorem></body>";
        let analysis = analyze(&parse(source.as_bytes()).unwrap());
        let block = &analysis.blocks[0];
        assert_eq!(analysis.canonical_text(block.id), None);
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "canonicalization_failed")
        );
    }

    #[test]
    fn hash_normalization_retains_non_identity_bindings() {
        let id = BlockId::new();
        let red = format!(
            "<\\body><\\theorem>T<with|color|red|mls-id|{id}|<equation|x=x>></theorem></body>"
        );
        let blue = red.replace("color|red", "color|blue");
        let other_id = red.replace(&id.to_string(), &BlockId::new().to_string());
        let red_hash = analyze(&parse(red.as_bytes()).unwrap()).blocks[0]
            .text_hash
            .clone();
        let blue_hash = analyze(&parse(blue.as_bytes()).unwrap()).blocks[0]
            .text_hash
            .clone();
        let other_id_hash = analyze(&parse(other_id.as_bytes()).unwrap()).blocks[0]
            .text_hash
            .clone();
        assert_ne!(red_hash, blue_hash);
        assert_eq!(red_hash, other_id_hash);
    }
}
