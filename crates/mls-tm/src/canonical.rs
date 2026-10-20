//! Layout-independent canonical text for blocks.
//!
//! TeXmacs re-serializes a document on every save: it indents long-form tags, rewraps
//! lines and changes between short and long forms, but reads back the same tree. This
//! module renders a block from the parsed syntax with the reader rules of TeXmacs'
//! `src/Data/Convert/Texmacs/fromtm.cpp` (`tm_reader::read`, `read_apply`, `read_next`,
//! `flush`, `decode`), so layouts that read as the same tree get the same text.
//!
//! Rules applied, with their reader counterparts:
//! - A run of space, tab, CR and LF is one space, or a paragraph break when it holds at
//!   least two LF (`read_next`, `skip_blank`). A single LF is therefore a space.
//! - A backslash followed by LF joins lines: the backslash, the LF and the spaces and tabs
//!   after it are dropped (`read_char`).
//! - Escapes decode as in `decode`; an escaped space is a literal space, not a separator.
//! - The leading whitespace of a long-form argument that follows `>` is skipped, and the
//!   trailing whitespace of such an argument is dropped (`skip_blank`, `skip_flag`). An
//!   argument that follows `|` keeps both.
//! - A bare `|` or `>` in argument content ends the argument; a `>` also trims the next one.
//! - A paragraph break splits an argument into paragraphs (`ret_flag`). An argument of one
//!   paragraph has no document wrapper, so short and long forms give the same text.
//! - An `mls-id` binding is identity metadata. A carrier that binds nothing else is
//!   transparent, and its content reads as if the carrier were absent. Other bindings stay.
//!   TeXmacs keeps the trailing space of a short-form body, so that space stays too.
//!
//! Refused, with `canonicalization_failed`, because TeXmacs reads them as trees that the text
//! cannot show: raw data `<#...>` (TeXmacs drops the token after it), and a transparent carrier
//! whose body is blank, holds a paragraph break, or holds a bare `|` or `>` (TeXmacs keeps the
//! body as a node of its own, or splits it into extra `with` arguments).
//!
//! Output syntax (TeXmacs-like): text escapes `\`, `<`, `>`, `|` and control bytes (as
//! `\xHH`); paragraphs of one argument are separated by a blank line; arguments are joined
//! by `|`; a tag without arguments is `<name>` and a tag with arguments is `<name|a|b>`.
//! Non-UTF-8 bytes pass through unchanged.

use crate::parser::{Node, ParseError, parse};

/// Canonical text of the source slice that holds one block.
///
/// The slice is parsed on its own. Block spans come from a parse of the whole document,
/// so this succeeds for them.
///
/// # Errors
///
/// Returns [`ParseError`] when the slice is not valid TeXmacs native syntax.
pub(crate) fn block_text(slice: &[u8]) -> Result<Vec<u8>, ParseError> {
    let document = parse(slice)?;
    if let Some(offset) = raw_data_offset(document.nodes(), slice) {
        return Err(ParseError {
            offset,
            message:
                "raw data <#...> has no canonical form: TeXmacs 2.1.5 drops the token after it"
                    .into(),
        });
    }
    let canon = Canon { source: slice };
    if let Some(offset) = canon.split_carrier_offset(document.nodes()) {
        return Err(ParseError {
            offset,
            message:
                "mls-id carrier with an empty body or a paragraph break has no canonical form: \
                      TeXmacs reads its body as a separate node"
                    .into(),
        });
    }
    let pieces = canon.args(document.nodes(), false);
    Ok(pieces.join(&b'|'))
}

/// Offset of the first `<#…>` raw-data tag, if any. TeXmacs reads the token after raw data as
/// one it discards, so two different trees can share the text that remains.
fn raw_data_offset(nodes: &[Node], source: &[u8]) -> Option<usize> {
    nodes.iter().find_map(|node| match node {
        Node::Text(_) => None,
        Node::Tag { span, .. } if source.get(span.start + 1) == Some(&b'#') => Some(span.start),
        Node::Tag { args, .. } => args.iter().find_map(|arg| raw_data_offset(arg, source)),
    })
}

struct Canon<'a> {
    source: &'a [u8],
}

impl Canon<'_> {
    /// Render a sequence of nodes as argument pieces. Bare separators create extra pieces.
    fn args(&self, nodes: &[Node], trim: bool) -> Vec<Vec<u8>> {
        let mut sink = Sink {
            pieces: Vec::new(),
            arg: Arg::new(trim),
        };
        self.feed(&mut sink, nodes);
        sink.pieces.push(sink.arg.finish());
        sink.pieces
    }

    fn feed(&self, sink: &mut Sink, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Text(span) => feed_text(sink, &self.source[span.start..span.end]),
                Node::Tag { .. } => self.feed_tag(sink, node),
            }
        }
    }

    fn feed_tag(&self, sink: &mut Sink, node: &Node) {
        sink.arg.skip_blank = false;
        if node.name() == Some(b"with")
            && let Some(kept) = self.kept_arguments(node)
        {
            if let [body] = kept.as_slice() {
                let body = node.args()[*body].as_slice();
                let trim = self.trimmed(body);
                // The carrier is a tag, so pending text before it ends there, as in `read`.
                sink.arg.flush();
                sink.arg.skip_blank |= trim;
                self.feed(sink, body);
                if trim {
                    sink.arg.space = false;
                    sink.arg.ret = false;
                } else {
                    // The body is a tree of its own: its trailing space is kept there and must
                    // not be trimmed by the enclosing argument. Without this, `x<with|…|y > `
                    // inside `<\theorem>` reads as `xy` where TeXmacs keeps `xy `.
                    sink.arg.flush();
                }
                return;
            }
            let args: Vec<&[Node]> = kept.iter().map(|&i| node.args()[i].as_slice()).collect();
            let rendered = self.render_tag(b"with", &args, false);
            sink.arg.push_tag(&rendered);
            return;
        }
        let name = decode(node.name().unwrap_or_default());
        let args: Vec<&[Node]> = node.args().iter().map(Vec::as_slice).collect();
        // Only `<#…>` raw data is hex, so only its digits are lowercased. `<\#4A>` is an ordinary
        // tag named `#4A`, and it keeps its case.
        let raw = self.source.get(node.span().start + 1) == Some(&b'#');
        let rendered = self.render_tag(&name, &args, raw);
        sink.arg.push_tag(&rendered);
    }

    /// Indices of the arguments left after removing `mls-id` bindings, or `None` when the
    /// node is not a valid identity carrier. The body (last argument) is always kept.
    fn kept_arguments(&self, node: &Node) -> Option<Vec<usize>> {
        super::with_id(node, self.source)?;
        let count = node.args().len();
        let mut removed = vec![false; count];
        for pair in 0..(count - 1) / 2 {
            let key = pair * 2;
            if super::plain_bytes(&node.args()[key], self.source) == Some(b"mls-id") {
                removed[key] = true;
                removed[key + 1] = true;
            }
        }
        Some((0..count).filter(|&index| !removed[index]).collect())
    }

    /// Offset of the first transparent `mls-id` carrier whose body TeXmacs keeps apart: an empty
    /// body, one with a paragraph break, or one with a bare separator. TeXmacs then holds the body
    /// as its own node (an empty child, a nested document) or as extra `with` arguments, which the
    /// carrier-free text cannot reproduce.
    fn split_carrier_offset(&self, nodes: &[Node]) -> Option<usize> {
        nodes.iter().find_map(|node| {
            if let Node::Tag { .. } = node {
                if node.name() == Some(b"with")
                    && let Some(kept) = self.kept_arguments(node)
                    && let [body] = kept.as_slice()
                    && splits_body(node.args()[*body].as_slice(), self.source)
                {
                    return Some(node.span().start);
                }
                return node
                    .args()
                    .iter()
                    .find_map(|arg| self.split_carrier_offset(arg));
            }
            None
        })
    }

    /// Whether an argument follows `>` (a long-form body or continuation), which trims it.
    fn trimmed(&self, nodes: &[Node]) -> bool {
        nodes
            .first()
            .map(|node| node.span().start)
            .is_some_and(|start| start > 0 && self.source[start - 1] == b'>')
    }

    /// Render a tag. `name` is the decoded tag name; `raw` marks `<#…>` raw data.
    fn render_tag(&self, name: &[u8], args: &[&[Node]], raw: bool) -> Vec<u8> {
        let mut out = vec![b'<'];
        let name = match name.split_first() {
            Some((b'#', hex)) if raw => [b"#".as_slice(), &hex.to_ascii_lowercase()].concat(),
            _ => name.to_vec(),
        };
        escape_into(&name, &mut out);
        if args.is_empty() {
            out.push(b'>');
            return out;
        }
        for arg in args {
            for piece in self.args(arg, self.trimmed(arg)) {
                out.push(b'|');
                out.extend_from_slice(&piece);
            }
        }
        out.push(b'>');
        out
    }
}

/// Pieces of one sequence being read: a bare `|` or `>` starts the next piece.
struct Sink {
    pieces: Vec<Vec<u8>>,
    arg: Arg,
}

impl Sink {
    fn split(&mut self, separator: u8) {
        let trim = separator == b'>';
        let done = std::mem::replace(&mut self.arg, Arg::new(trim)).finish();
        self.pieces.push(done);
    }
}

/// Reader state for one argument (`tm_reader::read`): finished paragraphs, the open
/// paragraph (`C`), pending text (`S`), and the pending space and break flags.
struct Arg {
    trim: bool,
    skip_blank: bool,
    paragraphs: Vec<Vec<u8>>,
    current: Vec<u8>,
    has_pieces: bool,
    pending: Vec<u8>,
    space: bool,
    ret: bool,
}

impl Arg {
    fn new(trim: bool) -> Self {
        Self {
            trim,
            skip_blank: trim,
            paragraphs: Vec::new(),
            current: Vec::new(),
            has_pieces: false,
            pending: Vec::new(),
            space: false,
            ret: false,
        }
    }

    fn space(&mut self) {
        self.space = true;
    }

    fn paragraph_break(&mut self) {
        self.ret = true;
    }

    /// `flush` in `fromtm.cpp`: append pending text, then close a paragraph on a break.
    fn flush(&mut self) {
        if self.space {
            self.pending.push(b' ');
        }
        if !self.pending.is_empty() {
            escape_into(&self.pending, &mut self.current);
            self.pending.clear();
            self.has_pieces = true;
        }
        self.space = false;
        if self.ret {
            self.paragraphs.push(std::mem::take(&mut self.current));
            self.has_pieces = false;
            self.ret = false;
        }
    }

    fn word(&mut self, decoded: &[u8]) {
        self.flush();
        self.pending.extend_from_slice(decoded);
        if self.pending.is_empty() && !self.has_pieces {
            self.has_pieces = true;
        }
    }

    fn push_tag(&mut self, rendered: &[u8]) {
        self.flush();
        self.current.extend_from_slice(rendered);
        self.has_pieces = true;
    }

    /// End of `read(skip_flag)`: a trimmed argument drops trailing flags, then flushes.
    fn finish(mut self) -> Vec<u8> {
        if self.trim {
            self.space = false;
            self.ret = false;
        }
        self.flush();
        if self.has_pieces {
            self.paragraphs.push(std::mem::take(&mut self.current));
        }
        if let [only] = self.paragraphs.as_slice()
            && only.is_empty()
        {
            return Vec::new();
        }
        self.paragraphs.join(&b"\n\n"[..])
    }
}

fn feed_text(sink: &mut Sink, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if sink.arg.skip_blank {
            if is_space(byte) {
                i += 1;
                continue;
            }
            sink.arg.skip_blank = false;
        }
        if byte == b'\\' && bytes.get(i + 1) == Some(&b'\n') {
            i = skip_spaces(bytes, i + 2);
            continue;
        }
        if is_space(byte) {
            let mut newlines = 0;
            while i < bytes.len() && is_space(bytes[i]) {
                newlines += usize::from(bytes[i] == b'\n');
                i += 1;
            }
            if newlines >= 2 {
                sink.arg.paragraph_break();
            } else {
                sink.arg.space();
            }
            continue;
        }
        if byte == b'|' || byte == b'>' {
            sink.split(byte);
            i += 1;
            continue;
        }
        let mut word = Vec::new();
        while i < bytes.len() {
            let current = bytes[i];
            if current == b'\\' {
                match bytes.get(i + 1) {
                    Some(b'\n') => i = skip_spaces(bytes, i + 2),
                    Some(&next) => {
                        word.push(current);
                        word.push(next);
                        i += 2;
                    }
                    None => {
                        word.push(current);
                        i += 1;
                    }
                }
                continue;
            }
            if is_space(current) || current == b'|' || current == b'>' {
                break;
            }
            word.push(current);
            i += 1;
        }
        sink.arg.word(&decode(&word));
    }
}

/// Whether a carrier body is blank, holds a paragraph break, or holds a bare separator, so TeXmacs
/// keeps it apart.
fn splits_body(body: &[Node], source: &[u8]) -> bool {
    let text = |node: &Node| match node {
        Node::Text(span) => Some(&source[span.start..span.end]),
        Node::Tag { .. } => None,
    };
    let blank = body.iter().all(|node| text(node).is_some_and(is_blank));
    blank
        || body.iter().filter_map(text).any(has_bare_separator)
        || body.iter().filter_map(text).any(has_paragraph_break)
}

/// Whether text holds a bare `|` or `>`, which TeXmacs reads as a separator. An escape pair such as
/// `\|` or `\>` is content, as in `feed_text`'s word loop.
fn has_bare_separator(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'|' | b'>' => return true,
            _ => i += 1,
        }
    }
    false
}

/// Whether text is only white space and line joins, which TeXmacs reads as nothing.
fn is_blank(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'\n') {
            i = skip_spaces(bytes, i + 2);
        } else if is_space(bytes[i]) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

/// Whether text holds two line feeds in one run of white space. A line join ends a run, as
/// `read_next` does: `a\n\\\n\nb` is one space, not a paragraph break.
fn has_paragraph_break(bytes: &[u8]) -> bool {
    let mut i = 0;
    let mut newlines = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'\n') {
            newlines = 0;
            i = skip_spaces(bytes, i + 2);
        } else {
            newlines = if is_space(bytes[i]) {
                newlines + usize::from(bytes[i] == b'\n')
            } else {
                0
            };
            i += 1;
        }
        if newlines >= 2 {
            return true;
        }
    }
    false
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

/// Decode one raw word the way `tm_reader::decode` does.
fn decode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            i += 1;
            match raw[i] {
                b';' => {}
                b'0' => out.push(0),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'n' => out.push(b'\n'),
                b'\\' => out.push(b'\\'),
                byte @ 0x40..=0x5f => out.push(byte - b'@'),
                byte => out.push(byte),
            }
        } else {
            out.push(raw[i]);
        }
        i += 1;
    }
    out
}

fn escape_into(bytes: &[u8], out: &mut Vec<u8>) {
    for &byte in bytes {
        match byte {
            b'\\' | b'<' | b'>' | b'|' => {
                out.push(b'\\');
                out.push(byte);
            }
            0..=0x1f | 0x7f => out.extend_from_slice(format!("\\x{byte:02x}").as_bytes()),
            _ => out.push(byte),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block_text;

    fn text(source: &[u8]) -> String {
        String::from_utf8(block_text(source).unwrap()).unwrap()
    }

    #[test]
    fn short_and_long_forms_agree() {
        let short = "<theorem|Truth holds.<label|thm:first>>";
        let long = "<\\theorem>Truth holds.<label|thm:first></theorem>";
        let indented = "<\\theorem>\n    Truth holds.<label|thm:first>\n  </theorem>";
        assert_eq!(
            text(short.as_bytes()),
            "<theorem|Truth holds.<label|thm:first>>"
        );
        assert_eq!(text(long.as_bytes()), text(short.as_bytes()));
        assert_eq!(text(indented.as_bytes()), text(short.as_bytes()));
    }

    #[test]
    fn line_breaks_inside_text_are_spaces_and_runs_collapse() {
        assert_eq!(text(b"Truth\n  holds"), "Truth holds");
        assert_eq!(text(b"Truth    holds"), "Truth holds");
        assert_eq!(text(b"Truth\tholds"), "Truth holds");
    }

    #[test]
    fn a_space_before_a_tag_is_kept_as_text() {
        assert_eq!(
            text(b"<\\theorem>Truth holds. <label|x></theorem>"),
            "<theorem|Truth holds. <label|x>>"
        );
    }

    #[test]
    fn blank_lines_split_paragraphs() {
        assert_eq!(text(b"<\\theorem>A\n\nB</theorem>"), "<theorem|A\n\nB>");
        assert_eq!(text(b"A\n\n\n\nB"), "A\n\nB");
    }

    #[test]
    fn long_form_trims_content_but_short_form_does_not() {
        assert_eq!(text(b"<\\theorem>\n\n  X  \n\n</theorem>"), "<theorem|X>");
        assert_eq!(text(b"<theorem| X >"), "<theorem| X >");
    }

    #[test]
    fn escapes_decode_and_reencode() {
        assert_eq!(text(b"a\\ b"), "a b");
        assert_eq!(text(b"a\\;b"), "ab");
        assert_eq!(text(b"x\\\\y"), "x\\\\y");
        assert_eq!(text(b"a\\|b"), "a\\|b");
        assert_eq!(text(b"a\\nb"), "a\\x0ab");
    }

    #[test]
    fn backslash_newline_joins_lines() {
        assert_eq!(text(b"a\\\n   b"), "ab");
        assert_eq!(text(b"<\\theorem>a\\\nb</theorem>"), "<theorem|ab>");
    }

    #[test]
    fn bare_separators_split_long_arguments() {
        assert_eq!(text(b"<\\theorem>a|b</theorem>"), "<theorem|a|b>");
        assert_eq!(text(b"<\\theorem>a>  b</theorem>"), "<theorem|a|b>");
        assert_eq!(text(b"<\\theorem>a|  b</theorem>"), "<theorem|a| b>");
        assert_eq!(text(b"<\\theorem>|x</theorem>"), "<theorem||x>");
        assert_eq!(text(b"<\\theorem>a|b|</theorem>"), "<theorem|a|b|>");
    }

    #[test]
    fn empty_arguments_and_nullary_tags_differ() {
        assert_eq!(text(b"<theorem>"), "<theorem>");
        assert_eq!(text(b"<theorem|>"), "<theorem|>");
        assert_eq!(text(b"<\\theorem></theorem>"), "<theorem|>");
    }

    #[test]
    fn identity_carrier_is_transparent_and_other_bindings_stay() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let plain = text(b"Truth<label|x>");
        assert_eq!(
            text(format!("<with|mls-id|{id}|Truth<label|x>>").as_bytes()),
            plain
        );
        assert_eq!(
            text(format!("<with|mls-id|{id}|Tr<with|font-series|bold|u>th>").as_bytes()),
            "Tr<with|font-series|bold|u>th"
        );
        assert_eq!(
            text(format!("<with|font-series|bold|mls-id|{id}|x>").as_bytes()),
            "<with|font-series|bold|x>"
        );
    }

    #[test]
    fn at_sign_escape_does_not_hide_a_label() {
        assert_ne!(
            text(br"<\theorem>\@<label|thm:a>_</theorem>"),
            text(br"<\theorem>\@\<label|thm:a|_</theorem>")
        );
    }

    #[test]
    fn bare_angle_bracket_in_a_name_is_refused_and_escaped_one_is_kept() {
        assert!(block_text(b"<a<b|c>").is_err());
        assert_eq!(text(br"<a\<b|c>"), r"<a\<b|c>");
    }

    #[test]
    fn carrier_keeps_the_trailing_space_of_a_short_body() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        assert_eq!(
            text(format!("<\\theorem>x<with|mls-id|{id}|y > </theorem>").as_bytes()),
            "<theorem|xy >"
        );
        assert_eq!(text(b"<\\theorem>xy </theorem>"), "<theorem|xy>");
    }

    #[test]
    fn carrier_bodies_that_texmacs_keeps_apart_have_no_canonical_form() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        for source in [
            format!("<\\theorem>A\n\n<with|mls-id|{id}|></theorem>"),
            format!("<\\theorem>x<with|mls-id|{id}|A\n\nB>y</theorem>"),
            format!("<\\theorem><\\with|mls-id|{id}>  </with>aTruth</theorem>"),
        ] {
            assert!(block_text(source.as_bytes()).is_err(), "{source}");
        }
    }

    #[test]
    fn carrier_bodies_with_a_bare_separator_have_no_canonical_form() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        for source in [
            format!("<\\theorem>\n    <\\with|mls-id|{id}>|</with>x\n  </theorem>"),
            format!("<\\theorem>x<\\with|mls-id|{id}>a>b</with>y</theorem>"),
        ] {
            assert!(block_text(source.as_bytes()).is_err(), "{source}");
        }
        // The same separator outside a carrier splits the argument, and an escaped one is content.
        assert_eq!(text(b"<\\theorem>\n    |x\n  </theorem>"), "<theorem||x >");
        assert_eq!(
            text(format!("<\\theorem>x<with|mls-id|{id}|a\\|b\\>c>y</theorem>").as_bytes()),
            "<theorem|xa\\|b\\>cy>"
        );
    }

    #[test]
    fn a_line_join_inside_a_tag_name_reads_as_the_joined_name() {
        assert_eq!(text(b"<w\\\n      ith|mode|math|x>"), "<with|mode|math|x>");
        assert_eq!(text(b"<w\\ ith|x>"), r"<w ith|x>");
    }

    #[test]
    fn a_line_join_ends_a_run_of_white_space() {
        assert_eq!(text(b"a\n\\\n\nb"), "a b");
        assert_eq!(
            text(b"<\\theorem>a\n\n\\\n\nb</theorem>"),
            "<theorem|a \n\nb>"
        );
    }

    #[test]
    fn raw_data_has_no_canonical_form() {
        for source in [&b"<#4A>"[..], b"<\\theorem><#4a>x</theorem>", b"x<#>"] {
            let error = block_text(source).unwrap_err();
            assert!(error.message.starts_with("raw data"), "{error}");
        }
    }

    #[test]
    fn tag_names_keep_their_case_outside_raw_data() {
        assert_eq!(text(b"<\\#4A>x</#4A>"), "<#4A|x>");
    }

    #[test]
    fn non_utf8_bytes_are_kept() {
        assert_eq!(block_text(b"caf\xe9").unwrap(), b"caf\xe9".to_vec());
    }
}
