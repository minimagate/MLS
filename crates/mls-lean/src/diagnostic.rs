//! Conversion of Lean's Unicode-aware positions to generated shadow block locations.

use crate::DeclarationSpan;
use serde::{Deserialize, Serialize};

/// Lean message severity.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational output, including trusted MLS payloads.
    Info,
    /// Non-fatal warning.
    Warning,
    /// Elaboration or command error.
    Error,
    /// Trace output.
    Trace,
}

/// A Lean position: one-based line and zero-based Unicode scalar column.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Position {
    /// One-based line.
    pub line: usize,
    /// Zero-based Unicode scalar column.
    pub column: usize,
}

/// A message returned by the community Lean REPL.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ReplDiagnostic {
    /// Severity assigned by Lean.
    pub severity: Severity,
    /// Start position, when supplied.
    #[serde(default)]
    pub pos: Option<Position>,
    /// End position, when supplied.
    #[serde(rename = "endPos", default)]
    pub end_pos: Option<Position>,
    /// Rendered diagnostic or trusted payload text.
    pub data: String,
}

/// A Lean diagnostic associated with a generated declaration block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappedDiagnostic {
    /// Stable block identity.
    pub block_id: Option<mls_core::BlockId>,
    /// Original Lean diagnostic.
    pub diagnostic: ReplDiagnostic,
    /// Zero-based UTF-8 byte offset within the declaration, if positioned inside it.
    pub declaration_byte: Option<usize>,
}

/// Map positioned messages by exact spans; unpositioned extractor errors use `requested_block`.
#[must_use]
pub fn map_diagnostics(
    source: &str,
    spans: &[DeclarationSpan],
    diagnostics: Vec<ReplDiagnostic>,
    requested_block: Option<mls_core::BlockId>,
) -> Vec<MappedDiagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            let byte = diagnostic
                .pos
                .and_then(|position| position_to_byte(source, position));
            let span = byte.and_then(|offset| {
                spans
                    .iter()
                    .find(|span| offset >= span.start_byte && offset < span.end_byte)
            });
            let block_id = span.map(|span| span.block_id).or(requested_block);
            let declaration_byte = span
                .zip(byte)
                .map(|(span, offset)| offset - span.start_byte);
            MappedDiagnostic {
                block_id,
                diagnostic,
                declaration_byte,
            }
        })
        .collect()
}

fn position_to_byte(source: &str, position: Position) -> Option<usize> {
    let line_start = if position.line == 1 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(position.line.checked_sub(2)?)?
            .0
            + 1
    };
    let line = source[line_start..]
        .split_once('\n')
        .map_or(&source[line_start..], |pair| pair.0);
    let column_byte = line.char_indices().nth(position.column).map_or_else(
        || (line.chars().count() == position.column).then_some(line.len()),
        |(offset, _)| Some(offset),
    )?;
    Some(line_start + column_byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> mls_core::BlockId {
        "00000000-0000-4000-8000-000000000001".parse().unwrap()
    }

    #[test]
    fn unicode_columns_map_to_utf8_bytes() {
        let source = "-- block:a\ntheorem a : α = β := by sorry\n";
        let spans = [DeclarationSpan {
            block_id: block(),
            start_byte: 11,
            end_byte: source.len(),
            start_line: 2,
            end_line: 2,
        }];
        let diagnostic = ReplDiagnostic {
            severity: Severity::Error,
            pos: Some(Position {
                line: 2,
                column: 14,
            }),
            end_pos: None,
            data: "bad".into(),
        };
        let mapped = map_diagnostics(source, &spans, vec![diagnostic], None);
        assert_eq!(mapped[0].block_id, Some(block()));
        assert_eq!(mapped[0].declaration_byte, Some("theorem a : α ".len()));
    }
}
