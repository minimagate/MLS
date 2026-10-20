//! Byte-oriented parser for TeXmacs native compound and long tags.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Half-open byte range in the original source.
pub struct Span {
    /// Inclusive start byte.
    pub start: usize,
    /// Exclusive end byte.
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One lossless syntax-tree node backed by source spans.
pub enum Node {
    /// Unparsed source text, including escapes and legacy bytes.
    Text(Span),
    /// A short or long native tag and its ordered arguments.
    Tag {
        /// The tag name without delimiters: escapes are kept, and line joins are removed as
        /// TeXmacs' `read_char` removes them.
        name: Vec<u8>,
        /// Parsed arguments in native source order.
        args: Vec<Vec<Node>>,
        /// Span of the complete tag.
        span: Span,
    },
}

impl Node {
    /// Return the complete source span occupied by this node.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Text(span) | Self::Tag { span, .. } => *span,
        }
    }

    /// Return this tag's name bytes, or `None` for text.
    #[must_use]
    pub fn name(&self) -> Option<&[u8]> {
        match self {
            Self::Tag { name, .. } => Some(name),
            Self::Text(_) => None,
        }
    }

    /// Return every ordered tag argument, or an empty slice for text.
    #[must_use]
    pub fn args(&self) -> &[Vec<Node>] {
        match self {
            Self::Tag { args, .. } => args,
            Self::Text(_) => &[],
        }
    }

    /// Return the final rendered-content argument.
    #[must_use]
    pub fn children(&self) -> &[Node] {
        self.args().last().map_or(&[], Vec::as_slice)
    }
}

#[derive(Clone, Debug)]
/// A lossless parsed view that borrows the original bytes.
pub struct Document<'a> {
    source: &'a [u8],
    nodes: Vec<Node>,
}

impl<'a> Document<'a> {
    /// Return the exact original bytes.
    #[must_use]
    pub fn source(&self) -> &'a [u8] {
        self.source
    }
    /// Return top-level syntax nodes.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A fatal native-syntax error with its byte offset.
pub struct ParseError {
    /// Byte offset at or immediately after the failure.
    pub offset: usize,
    /// Human-readable failure reason.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}
impl Error for ParseError {}

/// Parse TeXmacs native bytes without decoding or reserializing them.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed, unmatched, or unterminated tags.
pub fn parse(source: &[u8]) -> Result<Document<'_>, ParseError> {
    let mut parser = Parser { source, pos: 0 };
    let nodes = parser.sequence(None, &[])?;
    Ok(Document { source, nodes })
}

struct Parser<'a> {
    source: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn sequence(&mut self, closing: Option<&[u8]>, stops: &[u8]) -> Result<Vec<Node>, ParseError> {
        let mut nodes = Vec::new();
        let mut text_start = self.pos;
        while self.pos < self.source.len() {
            if stops.contains(&self.source[self.pos]) {
                break;
            }
            if self.source[self.pos] != b'<' {
                self.consume_text_unit();
                continue;
            }
            if self.starts(b"</") {
                if closing.is_some() {
                    break;
                }
                return self.err("unmatched closing tag");
            }
            if self.starts(b"<|") {
                if closing.is_some() {
                    break;
                }
                return self.err("continuation tag outside matching long tag");
            }
            if text_start < self.pos {
                nodes.push(Node::Text(Span {
                    start: text_start,
                    end: self.pos,
                }));
            }
            nodes.push(self.tag()?);
            text_start = self.pos;
        }
        if text_start < self.pos {
            nodes.push(Node::Text(Span {
                start: text_start,
                end: self.pos,
            }));
        }
        Ok(nodes)
    }

    /// A backslash escapes the byte after it, `\@` included. TeXmacs' `read_next` in
    /// `fromtm.cpp` has no other rule for `\@`.
    fn consume_text_unit(&mut self) {
        if self.source[self.pos] == b'\\' && self.pos + 1 < self.source.len() {
            self.pos += 2;
        } else {
            self.pos += 1;
        }
    }

    /// Scan a tag name as TeXmacs reads a word: a backslash escapes the next byte, and the name
    /// ends at `|` or `>`. A backslash and line feed are a line join, which `read_char` in
    /// `fromtm.cpp` removes together with the spaces and tabs after it, so `<w\` LF `  ith>` is
    /// `with`. A bare `<`, space, tab, CR or LF is not accepted in a name, because TeXmacs ends
    /// the name there and reads the rest in a different way.
    fn name(&mut self) -> Result<Vec<u8>, ParseError> {
        let mut name = Vec::new();
        while let Some(&byte) = self.source.get(self.pos) {
            match byte {
                b'|' | b'>' => break,
                b'<' | b' ' | b'\t' | b'\r' | b'\n' => {
                    return self.err("invalid character in tag name");
                }
                b'\\' => match self.source.get(self.pos + 1) {
                    None => return self.err("unterminated escape in tag name"),
                    Some(b'\n') => {
                        self.pos += 2;
                        while matches!(self.source.get(self.pos), Some(b' ' | b'\t')) {
                            self.pos += 1;
                        }
                    }
                    Some(&next) => {
                        name.extend_from_slice(&[byte, next]);
                        self.pos += 2;
                    }
                },
                _ => {
                    name.push(byte);
                    self.pos += 1;
                }
            }
        }
        if name.is_empty() {
            return self.err("empty tag name");
        }
        if self.pos == self.source.len() {
            return self.err("unterminated tag");
        }
        Ok(name)
    }

    /// Raw data `<#…>`: TeXmacs reads bytes up to `>` with no escapes. The bytes are not checked
    /// here, so a document with raw data still parses; canonicalization refuses such blocks.
    fn raw_data(&mut self, start: usize) -> Result<Node, ParseError> {
        while self.pos < self.source.len() && self.source[self.pos] != b'>' {
            self.pos += 1;
        }
        if self.pos == self.source.len() {
            return self.err("unterminated raw data");
        }
        let name = self.source[start + 1..self.pos].to_vec();
        self.pos += 1;
        Ok(Node::Tag {
            name,
            args: Vec::new(),
            span: Span {
                start,
                end: self.pos,
            },
        })
    }

    fn tag(&mut self) -> Result<Node, ParseError> {
        let start = self.pos;
        self.pos += 1;
        if self.take(b'#') {
            return self.raw_data(start);
        }
        let mut long = self.take(b'\\');
        if long && self.source.get(self.pos) == Some(&b'\n') {
            long = false;
            self.pos += 1;
            while matches!(self.source.get(self.pos), Some(b' ' | b'\t')) {
                self.pos += 1;
            }
        }
        let name = self.name()?;
        let mut args = Vec::new();
        while self.take(b'|') {
            args.push(self.sequence(None, b"|>")?);
        }
        if !self.take(b'>') {
            return self.err("unterminated tag");
        }
        if long {
            loop {
                let body = self.sequence(Some(&name), &[])?;
                args.push(body);
                if self.starts(b"</") {
                    self.pos += 2;
                    let close = self.name()?;
                    if close != name {
                        return self.err(&format!(
                            "mismatched closing tag: expected {}, found {}",
                            String::from_utf8_lossy(&name),
                            String::from_utf8_lossy(&close)
                        ));
                    }
                    while self.take(b'|') {
                        args.push(self.sequence(None, b"|>")?);
                    }
                    if !self.take(b'>') {
                        return self.err("unterminated closing tag");
                    }
                    break;
                }
                if self.starts(b"<|") {
                    self.pos += 2;
                    if self.name()? != name {
                        return self.err("mismatched continuation tag");
                    }
                    while self.take(b'|') {
                        args.push(self.sequence(None, b"|>")?);
                    }
                    if !self.take(b'>') {
                        return self.err("unterminated continuation tag");
                    }
                } else {
                    return self.err("missing closing tag");
                }
            }
        }
        Ok(Node::Tag {
            name,
            args,
            span: Span {
                start,
                end: self.pos,
            },
        })
    }

    fn starts(&self, bytes: &[u8]) -> bool {
        self.source
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(bytes))
    }
    fn take(&mut self, byte: u8) -> bool {
        if self.source.get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn err<T>(&self, message: &str) -> Result<T, ParseError> {
        Err(ParseError {
            offset: self.pos,
            message: message.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_long_tag_and_preserves_span() {
        let source = br"<\f|a|b> c <|f|d> e </f>";
        let document = parse(source).unwrap();
        assert_eq!(
            document.nodes()[0].span(),
            Span {
                start: 0,
                end: source.len()
            }
        );
        assert_eq!(document.nodes()[0].name(), Some(b"f".as_slice()));
        assert_eq!(document.nodes()[0].args().len(), 5);
    }

    #[test]
    fn accepts_escapes_raw_hex_and_non_utf8() {
        let source = b"x\\ y\\; \\<alpha\\> <#deadbeef> \\@ff_ \\n \xff";
        let document = parse(source).unwrap();
        assert_eq!(document.source(), source);
    }

    #[test]
    fn rejects_malformed_tags() {
        assert!(parse(br"<\theorem>x</lemma>").is_err());
        assert!(parse(br"</body>").is_err());
        assert!(parse(br"<|body>x>").is_err());
    }

    #[test]
    fn at_sign_is_an_ordinary_escape() {
        let source = br"\@<em|x>_";
        let document = parse(source).unwrap();
        assert_eq!(document.nodes().len(), 3);
        assert_eq!(document.nodes()[1].name(), Some(b"em".as_slice()));
    }

    #[test]
    fn tag_names_end_at_angle_brackets_and_white_space() {
        for source in [&b"<a<b|c>"[..], b"<a b>", b"<a\tb>", b"<a\nb>", b"<a\rb>"] {
            assert!(
                parse(source).is_err(),
                "{}",
                String::from_utf8_lossy(source)
            );
        }
        let source = br"<a\<b|c>";
        assert_eq!(
            parse(source).unwrap().nodes()[0].name(),
            Some(br"a\<b".as_slice())
        );
    }

    #[test]
    fn a_line_join_inside_a_tag_name_is_removed_with_the_blanks_after_it() {
        let source = b"<w\\\n      ith|mode|math|x>";
        let document = parse(source).unwrap();
        assert_eq!(document.nodes()[0].name(), Some(b"with".as_slice()));
        assert_eq!(document.nodes()[0].args().len(), 3);
        assert!(parse(b"<w\\\n\nith>").is_err());
        assert!(parse(b"<w\n\\\nith>").is_err());
        assert!(parse(b"<w\\\r\nith>").is_err());
    }

    #[test]
    fn raw_data_is_a_tag_without_arguments() {
        let source = b"x<#4a>y";
        let document = parse(source).unwrap();
        assert_eq!(document.nodes()[1].name(), Some(b"#4a".as_slice()));
        assert!(document.nodes()[1].args().is_empty());
        assert!(parse(b"<#4a").is_err());
    }

    #[test]
    fn accepts_logical_line_continuations_and_close_arguments() {
        assert!(parse(b"<\\\n  samp|Preferences>").is_ok());
        let document = parse(br"<\explain>x</explain|caption>").unwrap();
        assert_eq!(document.nodes()[0].args().len(), 2);
    }
}
