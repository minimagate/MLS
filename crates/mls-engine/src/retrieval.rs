//! Pure local premise retrieval. Inputs are the extracted index and the SIR actually in use.
//!
//! Embeddings are supplied by the caller; this module performs no HTTP or filesystem I/O and
//! never changes a review decision or a checking status. Absent embeddings select the fixed
//! BM25F fallback. Invalid supplied caches/vectors are errors, never silently reused.

use mls_core::{ContentHasher, source_hash};
use mls_models::EmbeddingInputType;
use mls_sir::Sir;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::OnceLock;

/// Premises included in an initial generation prompt.
pub const DEFAULT_TOP_K: usize = 10;
/// Premises included in a repair prompt.
pub const REPAIR_TOP_K: usize = 20;
/// Explicit marker included within every truncated text's character bound.
pub const TRUNCATION_MARKER: &str = " [truncated]";
/// Maximum embedding text characters (name, type, first doc sentence and separators).
pub const MAX_EMBEDDING_CHARS: usize = 2_048;
/// Maximum query text characters, including its truncation marker.
pub const MAX_QUERY_CHARS: usize = 8_000;

/// A malformed index, incompatible cache, or unusable vector.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    /// A caller-supplied cache stream failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON input could not be decoded under its strict schema.
    #[error("invalid retrieval JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// An integrity, configuration or numerical invariant failed.
    #[error("invalid retrieval data: {0}")]
    Invalid(String),
}

fn invalid(message: &str) -> RetrievalError {
    RetrievalError::Invalid(message.into())
}

/// Manifest of the exact, unfiltered JSONL output of `MlsDumpIndex.lean`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexManifest {
    /// Must be `mls-premises-v1`.
    pub schema_version: String,
    /// Full lowercase Mathlib Git commit.
    pub mathlib_commit: String,
    /// Pinned Lean toolchain identifier.
    pub toolchain: String,
    /// SHA-256 of the extractor source.
    pub extractor_sha256: String,
    /// Extractor filter policy version; currently 1.
    pub filter_version: u32,
    /// Number of JSONL rows, before retrieval filtering.
    pub row_count: usize,
    /// SHA-256 of the exact JSONL bytes, including newlines.
    pub sha256: String,
}

impl IndexManifest {
    fn validate(&self) -> Result<(), RetrievalError> {
        if self.schema_version != "mls-premises-v1"
            || self.filter_version != 1
            || !hex_digest(&self.mathlib_commit, 40)
            || !hex_digest(&self.extractor_sha256, 64)
            || !hex_digest(&self.sha256, 64)
            || self.toolchain.trim().is_empty()
        {
            return Err(invalid("unsupported or malformed index manifest"));
        }
        Ok(())
    }
}

/// One trusted extractor row. Only noninstance, nondeprecated theorems and definitions rank.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Premise {
    /// Fully qualified Lean declaration name.
    pub name: String,
    /// Extractor declaration kind (`theorem`, `def`, and other excluded kinds).
    pub kind: String,
    /// Originating Mathlib module.
    pub module: String,
    /// Whether Lean registered this declaration as an instance.
    pub instance: bool,
    /// Whether the declaration is deprecated.
    pub deprecated: bool,
    /// Optional documentation.
    pub doc: Option<String>,
    /// Pretty-printed Lean type.
    #[serde(rename = "type")]
    pub type_text: String,
}

impl Premise {
    /// Deterministic bounded input for declaration embeddings. Each field is bounded separately
    /// so a long type cannot hide the doc sentence. Text-policy changes invalidate vector keys.
    #[must_use]
    pub fn embedding_text(&self) -> String {
        format!(
            "{}\n{}\n{}",
            bounded(&compact(&self.name), 254),
            bounded(&compact(&self.type_text), 1_536),
            bounded(
                &first_sentence(self.doc.as_deref().unwrap_or_default()),
                256
            )
        )
    }
}

/// The actual runtime SIR is the sole query input. Dependency labels (`uses`) and source/reference
/// statements are excluded. Surfaced assumptions are included because they are part of this SIR's
/// chosen mathematical reading, not evidence that a human has confirmed it.
#[must_use]
pub fn query_text(sir: &Sir) -> String {
    let mut lines = Vec::new();
    if let Some(label) = sir.label() {
        lines.push(format!("label: {}", compact(label)));
    }
    for variable in sir.variables() {
        lines.push(format!(
            "variable: {} : {}",
            compact(&variable.name),
            compact(&variable.type_description)
        ));
    }
    for hypothesis in sir.hypotheses() {
        lines.push(format!("hypothesis: {}", compact(hypothesis)));
    }
    lines.push(format!("conclusion: {}", compact(sir.conclusion())));
    for assumption in sir.assumptions_made() {
        lines.push(format!("assumption: {}", compact(assumption)));
    }
    bounded(&lines.join("\n"), MAX_QUERY_CHARS)
}

/// A ranked premise, borrowed from its index. Scores are finite and ordered descending;
/// equal scores break ties by declaration name.
#[derive(Clone, Copy, Debug)]
pub struct Hit<'a> {
    /// Retrieved declaration.
    pub premise: &'a Premise,
    /// BM25F or cosine score, according to the chosen method.
    pub score: f64,
}

/// Validated, filtered index and deterministic BM25F postings.
pub struct PremiseIndex {
    manifest: IndexManifest,
    rows: Vec<Premise>,
    row_text_sha256: Vec<String>,
    postings: BTreeMap<String, Vec<(usize, [usize; 3])>>,
    lengths: Vec<[usize; 3]>,
    averages: [f64; 3],
}

impl PremiseIndex {
    /// Decode a strict manifest and sorted JSONL, verifying exact bytes, count and unique names
    /// before excluding instances, deprecated rows and kinds other than theorem/def.
    pub fn parse(manifest_json: &[u8], jsonl: &[u8]) -> Result<Self, RetrievalError> {
        let manifest: IndexManifest = serde_json::from_slice(manifest_json)?;
        manifest.validate()?;
        if source_hash(jsonl) != manifest.sha256 {
            return Err(invalid("JSONL checksum mismatch"));
        }
        let text = std::str::from_utf8(jsonl).map_err(|_| invalid("JSONL is not UTF-8"))?;
        let mut rows = Vec::new();
        let mut previous = String::new();
        let mut count = 0;
        for line in text.lines() {
            let row: Premise = serde_json::from_str(line)?;
            if row.name.is_empty()
                || row.name.chars().any(char::is_whitespace)
                || row.name.chars().any(char::is_control)
                || row.name <= previous
                || row.type_text.trim().is_empty()
                || !(row.module == "Mathlib" || row.module.starts_with("Mathlib."))
            {
                return Err(invalid("invalid, duplicate or unsorted premise row"));
            }
            previous.clone_from(&row.name);
            count += 1;
            if !row.instance && !row.deprecated && matches!(row.kind.as_str(), "theorem" | "def") {
                rows.push(row);
            }
        }
        if count != manifest.row_count {
            return Err(invalid("JSONL row count mismatch"));
        }
        let mut index = Self {
            manifest,
            row_text_sha256: rows
                .iter()
                .map(|row| source_hash(row.embedding_text().as_bytes()))
                .collect(),
            rows,
            postings: BTreeMap::new(),
            lengths: Vec::new(),
            averages: [0.0; 3],
        };
        index.build_postings();
        Ok(index)
    }

    /// Exact extractor provenance.
    #[must_use]
    pub fn manifest(&self) -> &IndexManifest {
        &self.manifest
    }

    /// Retrievable rows in the original, validated name order.
    #[must_use]
    pub fn rows(&self) -> &[Premise] {
        &self.rows
    }

    fn build_postings(&mut self) {
        for (row_id, row) in self.rows.iter().enumerate() {
            let fields = [
                tokenize(&row.name),
                // Braces/brackets can describe sets, intervals, lists or binders. Retain their
                // mathematical content rather than guessing a Lean binder parse lexically.
                tokenize(&row.type_text),
                tokenize(row.doc.as_deref().unwrap_or_default()),
            ];
            let lengths = fields.each_ref().map(Vec::len);
            self.lengths.push(lengths);
            let mut counts = BTreeMap::<String, [usize; 3]>::new();
            for (field, words) in fields.into_iter().enumerate() {
                self.averages[field] += lengths[field] as f64;
                for word in words {
                    counts.entry(word).or_default()[field] += 1;
                }
            }
            for (word, counts) in counts {
                self.postings
                    .entry(word)
                    .or_default()
                    .push((row_id, counts));
            }
        }
        for average in &mut self.averages {
            *average /= self.rows.len().max(1) as f64;
            if *average == 0.0 {
                *average = 1.0;
            }
        }
    }

    /// Fixed BM25F: k1=1.2, b=0.75, weights name/type/doc=3/1/1. Query terms are deduplicated
    /// and sorted; zero-score rows are omitted. Tokens/rules are versioned alongside this module.
    #[must_use]
    pub fn rank_bm25f(&self, query: &str, k: usize) -> Vec<Hit<'_>> {
        let mut scores = BTreeMap::<usize, f64>::new();
        for word in tokenize(query).into_iter().collect::<BTreeSet<_>>() {
            let Some(postings) = self.postings.get(&word) else {
                continue;
            };
            let n = self.rows.len() as f64;
            let df = postings.len() as f64;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (row, counts) in postings {
                let tf: f64 = (0..3)
                    .map(|field| {
                        let normalization =
                            0.25 + 0.75 * self.lengths[*row][field] as f64 / self.averages[field];
                        [3.0, 1.0, 1.0][field] * counts[field] as f64 / normalization
                    })
                    .sum();
                *scores.entry(*row).or_default() += idf * 2.2 * tf / (1.2 + tf);
            }
        }
        self.sort_hits(scores, k)
    }

    /// Rank supplied declaration/query embeddings by cosine. The cache must match this exact
    /// index, row order and embedding-text policy. Zero-norm vectors cannot define a cosine.
    /// This low-level numerical API cannot identify the query text; use [`Self::retrieve`] to
    /// enforce the runtime SIR, embedding model, dimensions and search-query purpose together.
    pub fn rank_cosine(
        &self,
        vectors: &PremiseVectors,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<Hit<'_>>, RetrievalError> {
        if vectors.key.manifest != self.manifest
            || vectors.key.row_text_sha256 != self.row_text_sha256
        {
            return Err(invalid(
                "vectors belong to another index or embedding-text policy",
            ));
        }
        validate_vector(query, vectors.key.embedding.dimensions)?;
        let qnorm = norm(query);
        if qnorm == 0.0 {
            return Err(invalid("zero query vector"));
        }
        let mut scores = BTreeMap::new();
        for (row, vector) in vectors.values.chunks_exact(query.len()).enumerate() {
            let vnorm = norm(vector);
            if vnorm == 0.0 {
                return Err(invalid("zero declaration vector"));
            }
            let dot: f64 = vector
                .iter()
                .zip(query)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum();
            let score = (dot / (qnorm * vnorm)).clamp(-1.0, 1.0);
            scores.insert(row, if score == 0.0 { 0.0 } else { score });
        }
        Ok(self.sort_hits(scores, k))
    }

    /// Retrieve from the runtime SIR with default prompt sizes. Omit embeddings to use BM25F;
    /// supplied invalid vectors are rejected so replay cannot silently change ranking methods.
    pub fn retrieve(
        &self,
        sir: &Sir,
        embeddings: Option<(&PremiseVectors, &QueryVector)>,
        repair: bool,
    ) -> Result<Vec<Hit<'_>>, RetrievalError> {
        let k = if repair { REPAIR_TOP_K } else { DEFAULT_TOP_K };
        match embeddings {
            Some((vectors, query)) => {
                if query.embedding != vectors.key.embedding
                    || query.input_type != EmbeddingInputType::SearchQuery
                    || query.query_sha256 != source_hash(query_text(sir).as_bytes())
                {
                    return Err(invalid(
                        "query vector does not match the runtime SIR, model, dimensions or purpose",
                    ));
                }
                self.rank_cosine(vectors, &query.values, k)
            }
            None => Ok(self.rank_bm25f(&query_text(sir), k)),
        }
    }

    fn sort_hits(&self, scores: BTreeMap<usize, f64>, k: usize) -> Vec<Hit<'_>> {
        let mut hits: Vec<_> = scores
            .into_iter()
            .map(|(row, score)| Hit {
                premise: &self.rows[row],
                score,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.premise.name.cmp(&b.premise.name))
        });
        hits.truncate(k);
        hits
    }

    fn vector_key(&self, embedding: EmbeddingConfig) -> Result<PremiseVectorKey, RetrievalError> {
        embedding.validate()?;
        Ok(PremiseVectorKey {
            version: "mls-premise-vectors-v1".into(),
            manifest: self.manifest.clone(),
            embedding,
            row_text_sha256: self.row_text_sha256.clone(),
        })
    }
}

/// Configured embedding identity. Dimension/model changes invalidate both cache types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// Exact configured embedding model ID, including any version suffix.
    pub model: String,
    /// Exact returned vector dimensionality.
    pub dimensions: usize,
}

impl EmbeddingConfig {
    fn validate(&self) -> Result<(), RetrievalError> {
        if self.model.trim().is_empty() || self.dimensions == 0 {
            return Err(invalid(
                "embedding model and positive dimensions are required",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PremiseVectorKey {
    version: String,
    manifest: IndexManifest,
    embedding: EmbeddingConfig,
    row_text_sha256: Vec<String>,
}

/// Checked row-major f32 embeddings, with an exact index/text/model identity.
pub struct PremiseVectors {
    key: PremiseVectorKey,
    values: Vec<f32>,
}

impl PremiseVectors {
    /// Write the existing cache format from exact row-major little-endian f32 payload bytes.
    /// Uses a bounded payload buffer, checks shape/finiteness, and patches the header checksum.
    /// The output must be a fresh stream; callers publish it only after this succeeds.
    pub fn write_stream(
        index: &PremiseIndex,
        embedding: EmbeddingConfig,
        payload: &mut impl Read,
        output: &mut (impl Write + Seek),
    ) -> Result<(), RetrievalError> {
        let key = index.vector_key(embedding)?;
        let count = vector_count(index.rows.len(), key.embedding.dimensions)?;
        let start = output.stream_position()?;
        let placeholder = serde_json::to_vec(&CacheHeader {
            identity: &key,
            content_sha256: "0".repeat(64),
        })?;
        output.write_all(MAGIC)?;
        output.write_all(&(placeholder.len() as u64).to_le_bytes())?;
        output.write_all(&placeholder)?;
        let payload_hash = copy_payload(payload, output, count)?;
        let end = output.stream_position()?;
        let header = serde_json::to_vec(&CacheHeader {
            identity: &key,
            content_sha256: cache_checksum_digest(&key, &payload_hash)?,
        })?;
        debug_assert_eq!(header.len(), placeholder.len());
        output.seek(SeekFrom::Start(start + 16))?;
        output.write_all(&header)?;
        output.seek(SeekFrom::Start(end))?;
        Ok(())
    }

    /// Validate an entire cache without allocating its vector payload. Header identity, exact
    /// length, checksum and every f32 coordinate are checked against the expected index/config.
    pub fn validate_stream(
        index: &PremiseIndex,
        embedding: EmbeddingConfig,
        input: &mut impl Read,
    ) -> Result<(), RetrievalError> {
        let key = index.vector_key(embedding)?;
        let mut prefix = [0; 16];
        input.read_exact(&mut prefix)?;
        if &prefix[..8] != MAGIC {
            return Err(invalid("invalid vector cache magic"));
        }
        let expected_size = serde_json::to_vec(&CacheHeader {
            identity: &key,
            content_sha256: "0".repeat(64),
        })?
        .len();
        if u64::from_le_bytes(prefix[8..].try_into().expect("fixed prefix")) != expected_size as u64
        {
            return Err(invalid("incompatible vector cache header length"));
        }
        let mut bytes = vec![0; expected_size];
        input.read_exact(&mut bytes)?;
        let header: CacheHeader<PremiseVectorKey> = serde_json::from_slice(&bytes)?;
        if header.identity != key {
            return Err(invalid("incompatible premise vector cache"));
        }
        let digest = copy_payload(
            input,
            &mut std::io::sink(),
            vector_count(index.rows.len(), key.embedding.dimensions)?,
        )?;
        if cache_checksum_digest(&key, &digest)? != header.content_sha256 {
            return Err(invalid("corrupt vector payload"));
        }
        Ok(())
    }

    /// Accept externally supplied vectors in `index.rows()` order, checking shape and finiteness.
    pub fn new(
        index: &PremiseIndex,
        embedding: EmbeddingConfig,
        rows: &[Vec<f32>],
    ) -> Result<Self, RetrievalError> {
        let key = index.vector_key(embedding)?;
        if rows.len() != index.rows.len() {
            return Err(invalid("vector row count mismatch"));
        }
        for row in rows {
            validate_vector(row, key.embedding.dimensions)?;
        }
        Ok(Self {
            key,
            values: rows.iter().flatten().copied().collect(),
        })
    }

    /// Encode a strict versioned JSON header and checksummed little-endian f32 payload.
    /// The caller may persist these bytes in a local `.f32` file.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RetrievalError> {
        encode_cache(&self.key, &self.values)
    }

    /// Decode only when the full header matches the expected index, model, dimensions, row-text
    /// hashes and row order. Truncation, trailing bytes, checksum failure and nonfinite values fail.
    pub fn from_bytes(
        index: &PremiseIndex,
        embedding: EmbeddingConfig,
        bytes: &[u8],
    ) -> Result<Self, RetrievalError> {
        let expected = index.vector_key(embedding)?;
        let (key, values): (PremiseVectorKey, _) = decode_cache(bytes)?;
        if key != expected
            || values.len() != vector_count(index.rows.len(), key.embedding.dimensions)?
        {
            return Err(invalid("incompatible premise vector cache"));
        }
        Ok(Self { key, values })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryVectorKey {
    version: String,
    embedding: EmbeddingConfig,
    input_type: EmbeddingInputType,
    query_sha256: Vec<String>,
}

/// An externally supplied search-query embedding bound to exact query text and model identity.
/// Private fields prevent its identity from drifting after validation.
#[derive(Clone, Debug)]
pub struct QueryVector {
    embedding: EmbeddingConfig,
    input_type: EmbeddingInputType,
    query_sha256: String,
    values: Vec<f32>,
}

impl QueryVector {
    /// Bind an embedding to the text, model, dimensions and purpose of its actual request.
    /// Document embeddings cannot be used as search queries. Pass [`query_text`] of the runtime
    /// SIR; [`PremiseIndex::retrieve`] verifies that text's hash again before scoring.
    pub fn new(
        embedding: EmbeddingConfig,
        query: &str,
        input_type: EmbeddingInputType,
        values: Vec<f32>,
    ) -> Result<Self, RetrievalError> {
        embedding.validate()?;
        validate_vector(&values, embedding.dimensions)?;
        if input_type != EmbeddingInputType::SearchQuery {
            return Err(invalid(
                "document embeddings cannot be used as query vectors",
            ));
        }
        Ok(Self {
            embedding,
            input_type,
            query_sha256: source_hash(query.as_bytes()),
            values,
        })
    }
}

/// Local query embeddings keyed by exact query-text SHA-256, model and dimensions.
/// Entries serialize in hash order, independent of insertion order.
pub struct QueryVectorCache {
    embedding: EmbeddingConfig,
    entries: BTreeMap<String, Vec<f32>>,
}

impl QueryVectorCache {
    /// Create an empty cache for one embedding model/dimensionality.
    pub fn new(embedding: EmbeddingConfig) -> Result<Self, RetrievalError> {
        embedding.validate()?;
        Ok(Self {
            embedding,
            entries: BTreeMap::new(),
        })
    }

    /// Store a query vector only when its complete embedding identity matches this cache.
    pub fn insert(&mut self, vector: QueryVector) -> Result<(), RetrievalError> {
        if vector.embedding != self.embedding
            || vector.input_type != EmbeddingInputType::SearchQuery
        {
            return Err(invalid(
                "query vector model, dimensions or purpose differ from cache",
            ));
        }
        self.entries.insert(vector.query_sha256, vector.values);
        Ok(())
    }

    /// Look up an exact query. Use [`query_text`] to construct runtime SIR queries consistently.
    #[must_use]
    pub fn get(&self, query: &str) -> Option<&[f32]> {
        self.entries
            .get(&source_hash(query.as_bytes()))
            .map(Vec::as_slice)
    }

    /// Look up a query vector retaining its validated identity for [`PremiseIndex::retrieve`].
    #[must_use]
    pub fn get_query(&self, query: &str) -> Option<QueryVector> {
        let hash = source_hash(query.as_bytes());
        Some(QueryVector {
            embedding: self.embedding.clone(),
            input_type: EmbeddingInputType::SearchQuery,
            values: self.entries.get(&hash)?.clone(),
            query_sha256: hash,
        })
    }

    /// Serialize a deterministic header and checksummed little-endian f32 values.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RetrievalError> {
        let key = QueryVectorKey {
            version: "mls-query-vectors-v2".into(),
            embedding: self.embedding.clone(),
            input_type: EmbeddingInputType::SearchQuery,
            query_sha256: self.entries.keys().cloned().collect(),
        };
        let values: Vec<_> = self.entries.values().flatten().copied().collect();
        encode_cache(&key, &values)
    }

    /// Decode and validate model, dimensions, unique sorted query hashes and exact payload length.
    pub fn from_bytes(embedding: EmbeddingConfig, bytes: &[u8]) -> Result<Self, RetrievalError> {
        embedding.validate()?;
        let (key, values): (QueryVectorKey, _) = decode_cache(bytes)?;
        if key.version != "mls-query-vectors-v2"
            || key.embedding != embedding
            || key.input_type != EmbeddingInputType::SearchQuery
            || key.query_sha256.iter().any(|hash| !hex_digest(hash, 64))
            || key.query_sha256.windows(2).any(|pair| pair[0] >= pair[1])
            || values.len() != vector_count(key.query_sha256.len(), embedding.dimensions)?
        {
            return Err(invalid("incompatible query vector cache"));
        }
        let entries = key
            .query_sha256
            .into_iter()
            .zip(
                values
                    .chunks_exact(embedding.dimensions)
                    .map(<[f32]>::to_vec),
            )
            .collect();
        Ok(Self { embedding, entries })
    }
}

const MAGIC: &[u8; 8] = b"MLSVEC01";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheHeader<T> {
    identity: T,
    content_sha256: String,
}

fn encode_cache<T: Serialize>(identity: &T, values: &[f32]) -> Result<Vec<u8>, RetrievalError> {
    let payload: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let content_sha256 = cache_checksum(identity, &payload)?;
    let header = serde_json::to_vec(&CacheHeader {
        identity,
        content_sha256,
    })?;
    let length = u64::try_from(header.len()).map_err(|_| invalid("header length overflow"))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend(header);
    bytes.extend(payload);
    Ok(bytes)
}

fn decode_cache<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
) -> Result<(T, Vec<f32>), RetrievalError> {
    if bytes.len() < 16 || &bytes[..8] != MAGIC {
        return Err(invalid("invalid vector cache magic or truncated prefix"));
    }
    let header_length = u64::from_le_bytes(bytes[8..16].try_into().expect("prefix checked"));
    let header_end = usize::try_from(header_length)
        .ok()
        .and_then(|length| length.checked_add(16))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("truncated vector cache header"))?;
    let header: CacheHeader<T> = serde_json::from_slice(&bytes[16..header_end])?;
    let payload = &bytes[header_end..];
    if !payload.len().is_multiple_of(4)
        || cache_checksum(&header.identity, payload)? != header.content_sha256
    {
        return Err(invalid("corrupt vector payload"));
    }
    let values: Vec<f32> = payload
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect();
    if values.iter().any(|value| !value.is_finite()) {
        return Err(invalid("nonfinite cached vector value"));
    }
    Ok((header.identity, values))
}

fn cache_checksum<T: Serialize>(identity: &T, payload: &[u8]) -> Result<String, RetrievalError> {
    cache_checksum_digest(identity, &source_hash(payload))
}

fn cache_checksum_digest<T: Serialize>(
    identity: &T,
    digest: &str,
) -> Result<String, RetrievalError> {
    let mut content = serde_json::to_vec(identity)?;
    // Hash the payload separately to avoid duplicating a Mathlib-sized vector buffer.
    content.extend_from_slice(digest.as_bytes());
    Ok(source_hash(&content))
}

fn copy_payload(
    input: &mut impl Read,
    output: &mut impl Write,
    count: usize,
) -> Result<String, RetrievalError> {
    let mut remaining = count
        .checked_mul(4)
        .ok_or_else(|| invalid("vector payload size overflow"))?;
    let mut buffer = [0_u8; 65_536];
    let mut hasher = ContentHasher::new();
    while remaining > 0 {
        let length = remaining.min(buffer.len());
        input.read_exact(&mut buffer[..length])?;
        if buffer[..length]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|word| !f32::from_le_bytes(*word).is_finite())
        {
            return Err(invalid("nonfinite cached vector value"));
        }
        hasher.update(&buffer[..length]);
        output.write_all(&buffer[..length])?;
        remaining -= length;
    }
    if input.read(&mut [0_u8; 1])? != 0 {
        return Err(invalid("trailing vector payload bytes"));
    }
    Ok(hasher.finish())
}

fn vector_count(rows: usize, dimensions: usize) -> Result<usize, RetrievalError> {
    rows.checked_mul(dimensions)
        .ok_or_else(|| invalid("vector shape overflow"))
}

fn validate_vector(vector: &[f32], dimensions: usize) -> Result<(), RetrievalError> {
    if vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()) {
        return Err(invalid("vector dimension mismatch or nonfinite value"));
    }
    Ok(())
}

fn norm(vector: &[f32]) -> f64 {
    vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// Bounds for ranked prompt entries. Defaults fit 20 entries within a 12,000-character prompt.
#[derive(Clone, Copy, Debug)]
pub struct PromptLimits {
    /// Maximum characters of one declaration name.
    pub name_chars: usize,
    /// Maximum characters of one type.
    pub type_chars: usize,
    /// Maximum characters of the first doc sentence.
    pub doc_chars: usize,
    /// Maximum characters across the complete rendered prompt.
    pub total_chars: usize,
}

impl Default for PromptLimits {
    fn default() -> Self {
        Self {
            name_chars: 120,
            type_chars: 300,
            doc_chars: 160,
            total_chars: 12_000,
        }
    }
}

/// Render rank order as `name : type` plus at most the first documentation sentence. Every field
/// bound includes its marker. When the total bound is reached, remaining entries are replaced by
/// an explicit truncation marker rather than an unmarked partial declaration.
pub fn render_prompt(hits: &[Hit<'_>], limits: PromptLimits) -> Result<String, RetrievalError> {
    let marker_length = TRUNCATION_MARKER.chars().count();
    if [
        limits.name_chars,
        limits.type_chars,
        limits.doc_chars,
        limits.total_chars,
    ]
    .iter()
    .any(|limit| *limit < marker_length)
    {
        return Err(invalid("prompt limits must fit the truncation marker"));
    }
    let entries: Vec<_> = hits
        .iter()
        .map(|hit| {
            let row = hit.premise;
            let mut entry = format!(
                "{} : {}",
                bounded(&compact(&row.name), limits.name_chars),
                bounded(&compact(&row.type_text), limits.type_chars)
            );
            let doc = first_sentence(row.doc.as_deref().unwrap_or_default());
            if !doc.is_empty() {
                entry.push_str(&format!("\n{}", bounded(&doc, limits.doc_chars)));
            }
            entry
        })
        .collect();
    let mut rendered = String::new();
    for (index, entry) in entries.iter().enumerate() {
        let separator = if rendered.is_empty() { "" } else { "\n\n" };
        let reserve = if index + 1 < entries.len() {
            marker_length
        } else {
            0
        };
        if rendered.chars().count() + separator.len() + entry.chars().count() + reserve
            > limits.total_chars
        {
            rendered.push_str(TRUNCATION_MARKER);
            break;
        }
        rendered.push_str(separator);
        rendered.push_str(entry);
    }
    Ok(rendered)
}

fn compact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let mut value: String = text
        .chars()
        .take(limit - TRUNCATION_MARKER.chars().count())
        .collect();
    value.push_str(TRUNCATION_MARKER);
    value
}

fn first_sentence(doc: &str) -> String {
    let doc = compact(doc);
    for (index, ch) in doc.char_indices() {
        if matches!(ch, '.' | '!' | '?')
            && doc[index + ch.len_utf8()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
        {
            return doc[..index + ch.len_utf8()].into();
        }
    }
    doc
}

fn hex_digest(text: &str, digits: usize) -> bool {
    text.len() == digits
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenRules {
    version: u32,
    notation: BTreeMap<char, String>,
    aliases: BTreeMap<String, String>,
    stopwords: BTreeSet<String>,
}

/// Fixed Unicode-notation mapping, CamelCase/dot/underscore splitting, lowercase aliases and
/// stopword/one-character removal. The output preserves token repetitions for BM25F counts.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    static RULES: OnceLock<TokenRules> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let rules: TokenRules = serde_json::from_str(include_str!("retrieval_tokens.json"))
            .expect("bundled tokenizer rules");
        assert_eq!(rules.version, 1, "unsupported bundled tokenizer policy");
        rules
    });
    let mut expanded = String::new();
    for ch in text.chars() {
        if let Some(word) = rules.notation.get(&ch) {
            expanded.push(' ');
            expanded.push_str(word);
            expanded.push(' ');
        } else {
            expanded.push(ch);
        }
    }
    let chars: Vec<_> = expanded.chars().collect();
    let mut split = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || (chars[index - 1].is_uppercase()
                    && chars.get(index + 1).is_some_and(|next| next.is_lowercase())))
        {
            split.push(' ');
        }
        if ch.is_alphanumeric() {
            split.extend(ch.to_lowercase());
        } else {
            split.push(' ');
        }
    }
    split
        .split_whitespace()
        .filter_map(|word| {
            let word = rules.aliases.get(word).map_or(word, String::as_str);
            (word.chars().count() > 1 && !rules.stopwords.contains(word)).then(|| word.to_owned())
        })
        .collect()
}
