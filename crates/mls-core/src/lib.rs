//! Editor-independent block identities and source-derived metadata.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fmt::Write as _, str::FromStr};
use uuid::Uuid;

/// A persistent block identity stored in the source document.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlockId(Uuid);

impl BlockId {
    /// Generate a fresh random identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for BlockId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for BlockId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Block categories known deterministically from native `TeXmacs` tags.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// A mathematical definition.
    Definition,
    /// A theorem statement.
    Theorem,
    /// A lemma statement.
    Lemma,
    /// A proposition statement.
    Proposition,
    /// A corollary statement.
    Corollary,
    /// A proof environment.
    Proof,
    /// A displayed equation environment.
    Equation,
    /// A document section heading.
    Section,
    /// Prose classified as a mathematical claim by a later milestone.
    ProseClaim,
    /// Untagged prose that has not been semantically classified.
    Commentary,
}

/// One source block emitted by the M1 parser.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// Persistent identity carried by the source document.
    pub id: BlockId,
    /// Deterministically classified source kind.
    pub kind: BlockKind,
    /// SHA-256 of the block's layout-independent canonical text: the content as TeXmacs reads
    /// it, with indentation, line breaks, short or long form and `mls-id` carriers normalized
    /// away. Layout-only saves keep it stable. Model prompts use the same canonical text.
    pub text_hash: String,
    /// Primary source label, when present.
    pub label: Option<String>,
    /// IDs of explicitly referenced blocks.
    pub deps: Vec<BlockId>,
    /// Inclusive start byte of this block in the parsed source.
    pub source_start: usize,
    /// Exclusive end byte of this block in the parsed source.
    pub source_end: usize,
}

/// A non-fatal source issue discovered while linking blocks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable machine-readable diagnostic category.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Inclusive start byte of the affected source.
    pub source_start: usize,
    /// Exclusive end byte of the affected source.
    pub source_end: usize,
}

/// Stable lowercase SHA-256 for the exact bytes occupied by a block.
#[must_use]
pub fn source_hash(source: &[u8]) -> String {
    let mut hasher = ContentHasher::new();
    hasher.update(source);
    hasher.finish()
}

/// Incremental SHA-256 with the same lowercase digest as [`source_hash`].
#[derive(Default)]
pub struct ContentHasher(Sha256);

impl ContentHasher {
    /// Start hashing an empty byte stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Include the next bytes, without retaining a copy of them.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Finish the stream and return its lowercase hexadecimal SHA-256.
    #[must_use]
    pub fn finish(self) -> String {
        let digest = self.0.finalize();
        digest
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
                hex
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_hash_matches_exact_bytes_for_every_chunk_size() {
        let bytes = b"abc\0\nmath: \xce\xb1";
        for chunk_size in 1..=bytes.len() {
            let mut hasher = ContentHasher::new();
            for chunk in bytes.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish(), source_hash(bytes));
        }
        assert_eq!(ContentHasher::new().finish(), source_hash(b""));
    }

    #[test]
    fn ids_round_trip() {
        let id = BlockId::new();
        assert_eq!(id.to_string().parse(), Ok(id));
    }

    #[test]
    fn hashing_uses_exact_bytes() {
        assert_eq!(
            source_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(source_hash(b"abc"), source_hash(b"abc "));
    }
}
