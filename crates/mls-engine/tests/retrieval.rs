//! Offline retrieval tests: exact index/cache provenance, actual-SIR query inputs and rank order.

use mls_core::source_hash;
use mls_engine::retrieval::{
    DEFAULT_TOP_K, EmbeddingConfig, IndexManifest, MAX_EMBEDDING_CHARS, MAX_QUERY_CHARS,
    PremiseIndex, PremiseVectors, PromptLimits, QueryVector, QueryVectorCache, REPAIR_TOP_K,
    TRUNCATION_MARKER, query_text, render_prompt, tokenize,
};
use mls_models::EmbeddingInputType;
use mls_sir::{Sir, StatementKind, Variable};
use serde_json::{Value, json};

fn row(name: &str, type_text: &str, doc: Option<&str>) -> Value {
    json!({"name":name,"type":type_text,"doc":doc,"kind":"theorem","module":"Mathlib.Test","instance":false,"deprecated":false})
}

fn source(rows: &[Value]) -> (IndexManifest, Vec<u8>) {
    let text = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>()
        .into_bytes();
    let manifest = IndexManifest {
        schema_version: "mls-premises-v1".into(),
        mathlib_commit: "a".repeat(40),
        toolchain: "leanprover/lean4:v4.19.0".into(),
        extractor_sha256: "b".repeat(64),
        filter_version: 1,
        row_count: rows.len(),
        sha256: source_hash(&text),
    };
    (manifest, text)
}

fn parse(
    manifest: &IndexManifest,
    text: &[u8],
) -> Result<PremiseIndex, mls_engine::retrieval::RetrievalError> {
    PremiseIndex::parse(&serde_json::to_vec(manifest).unwrap(), text)
}

fn index(rows: &[Value]) -> PremiseIndex {
    let (manifest, text) = source(rows);
    parse(&manifest, &text).unwrap()
}

fn embedding() -> EmbeddingConfig {
    EmbeddingConfig {
        model: "test/embedding-v1".into(),
        dimensions: 2,
    }
}

fn query_vector(query: &str, values: Vec<f32>) -> QueryVector {
    QueryVector::new(embedding(), query, EmbeddingInputType::SearchQuery, values).unwrap()
}

// Change persisted payload bytes while updating their checksum. This tests numerical/shape
// validation independently of the ordinary corruption checksum guard.
fn with_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
    let length = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let header = std::str::from_utf8(&bytes[16..16 + length]).unwrap();
    let value: Value = serde_json::from_str(header).unwrap();
    let identity = &header["{\"identity\":".len()..header.rfind(",\"content_sha256\":").unwrap()];
    let mut signed = identity.as_bytes().to_vec();
    signed.extend_from_slice(source_hash(payload).as_bytes());
    let updated = header.replace(
        value["content_sha256"].as_str().unwrap(),
        &source_hash(&signed),
    );
    let mut result = bytes[..8].to_vec();
    result.extend_from_slice(&(updated.len() as u64).to_le_bytes());
    result.extend_from_slice(updated.as_bytes());
    result.extend_from_slice(payload);
    result
}

fn sir(conclusion: &str) -> Sir {
    Sir::try_new(
        StatementKind::Theorem,
        Some("thm:kernel".into()),
        vec![Variable {
            name: "f".into(),
            type_description: "linear map ℝ → ℝ".into(),
        }],
        vec!["f is injective".into()],
        conclusion.into(),
        vec!["irrelevant:reference-label".into()],
        vec!["the scalar field is real".into()],
    )
    .unwrap()
}

#[test]
fn parses_exact_sorted_extractor_rows_and_excludes_nonpremises() {
    let mut rows = vec![
        row("A", "True", None),
        row("B", "True", None),
        row("C", "True", None),
        row("D", "Nat", None),
        row("E", "Nat", None),
    ];
    rows[1]["instance"] = json!(true);
    rows[2]["deprecated"] = json!(true);
    rows[3]["kind"] = json!("constructor");
    rows[4]["kind"] = json!("def");
    let idx = index(&rows);
    assert_eq!(
        idx.rows()
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["A", "E"]
    );
    assert_eq!(idx.manifest().row_count, 5);
    assert!(idx.rows()[0].doc.is_none());

    let (manifest, mut text) = source(&rows);
    text.push(b' ');
    assert!(parse(&manifest, &text).is_err(), "exact-byte hash checked");
    let (mut manifest, text) = source(&rows);
    manifest.row_count += 1;
    assert!(parse(&manifest, &text).is_err());
    manifest.row_count -= 1;
    manifest.schema_version = "future".into();
    assert!(parse(&manifest, &text).is_err());
    let (manifest, text) = source(&[row("B", "True", None), row("A", "True", None)]);
    assert!(parse(&manifest, &text).is_err(), "unsorted order rejected");
    let (manifest, text) = source(&[row("A", "True", None), row("A", "True", None)]);
    assert!(parse(&manifest, &text).is_err(), "duplicate names rejected");
    let (manifest, text) = source(&[json!({"name":"A","type":"True"})]);
    assert!(
        parse(&manifest, &text).is_err(),
        "missing required extractor flags rejected"
    );
}

#[test]
fn embedding_text_and_runtime_sir_query_are_deterministic_and_explicitly_bounded() {
    let idx = index(&[row(
        "LinearMap.ker",
        "  (f : LinearMap ℝ ℝ)\n: Set ℝ",
        Some("The kernel of f. A second sentence is excluded."),
    )]);
    assert_eq!(
        idx.rows()[0].embedding_text(),
        "LinearMap.ker\n(f : LinearMap ℝ ℝ) : Set ℝ\nThe kernel of f."
    );
    let query = query_text(&sir("ker f = {0}"));
    assert_eq!(
        query,
        "label: thm:kernel\nvariable: f : linear map ℝ → ℝ\nhypothesis: f is injective\nconclusion: ker f = {0}\nassumption: the scalar field is real"
    );
    assert!(!query.contains("irrelevant:reference-label"));
    assert_ne!(
        query,
        query_text(&sir("ker f is nontrivial")),
        "actual runtime claim changes query"
    );
    let huge = "ℝ".repeat(20_000);
    let idx = index(&[row("A", &huge, Some(&huge))]);
    let text = idx.rows()[0].embedding_text();
    assert!(text.chars().count() <= MAX_EMBEDDING_CHARS);
    assert_eq!(text.matches(TRUNCATION_MARKER).count(), 2);
    let query = query_text(&sir(&huge));
    assert_eq!(query.chars().count(), MAX_QUERY_CHARS);
    assert!(query.ends_with(TRUNCATION_MARKER));
}

#[test]
fn fixed_tokenizer_bm25f_and_fallback_have_stable_name_ties() {
    assert_eq!(
        tokenize("LinearMap.ker BddAbove sq_nonneg ℝ ≤ |x|"),
        [
            "linear",
            "map",
            "kernel",
            "bounded",
            "above",
            "square",
            "nonnegative",
            "real",
            "le",
            "abs",
            "abs"
        ]
    );
    let idx = index(&[
        row("A.kernel", "True", None),
        row("B.kernel", "True", None),
        row("C.other", "True", Some("A kernel.")),
        row("D.unrelated", "Nat", None),
    ]);
    let ranked = idx.rank_bm25f("kernel", 10);
    assert_eq!(
        ranked
            .iter()
            .map(|r| r.premise.name.as_str())
            .collect::<Vec<_>>(),
        ["A.kernel", "B.kernel", "C.other"]
    );
    assert!(
        ranked[0].score > ranked[2].score,
        "name-field weight exceeds doc weight"
    );
    let duplicate = idx.rank_bm25f("kernel kernel", 10);
    assert_eq!(
        ranked.iter().map(|h| h.score.to_bits()).collect::<Vec<_>>(),
        duplicate
            .iter()
            .map(|h| h.score.to_bits())
            .collect::<Vec<_>>()
    );
    assert!(idx.rank_bm25f("unmatched", 10).is_empty());
    assert!(idx.rank_bm25f("kernel", 0).is_empty());
    assert_eq!(
        idx.retrieve(&sir("kernel"), None, false).unwrap()[0]
            .premise
            .name,
        "A.kernel"
    );
}

#[test]
fn cosine_ranks_supplied_vectors_and_rejects_invalid_numerics() {
    let idx = index(&[
        row("A", "True", None),
        row("B", "True", None),
        row("C", "True", None),
    ]);
    let vectors = PremiseVectors::new(
        &idx,
        embedding(),
        &[vec![0.0, 1.0], vec![1.0, 0.0], vec![2.0, 0.0]],
    )
    .unwrap();
    let ranked = idx.rank_cosine(&vectors, &[1.0, 0.0], 10).unwrap();
    assert_eq!(
        ranked
            .iter()
            .map(|r| r.premise.name.as_str())
            .collect::<Vec<_>>(),
        ["B", "C", "A"]
    );
    assert_eq!(ranked[0].score, 1.0);
    assert!(idx.rank_cosine(&vectors, &[f32::NAN, 0.0], 10).is_err());
    assert!(idx.rank_cosine(&vectors, &[0.0, 0.0], 10).is_err());
    assert!(idx.rank_cosine(&vectors, &[1.0], 10).is_err());
    for rows in [
        vec![vec![1.0, 0.0]],
        vec![vec![1.0], vec![1.0], vec![1.0]],
        vec![vec![f32::INFINITY, 0.0]; 3],
    ] {
        assert!(PremiseVectors::new(&idx, embedding(), &rows).is_err());
    }
    let zeros = PremiseVectors::new(&idx, embedding(), &vec![vec![0.0; 2]; 3]).unwrap();
    assert!(idx.rank_cosine(&zeros, &[1.0, 0.0], 10).is_err());
}

#[test]
fn initial_and_repair_limits_apply_to_embedding_rankings() {
    let rows: Vec<_> = (0..25)
        .map(|i| row(&format!("A{i:02}"), "True", None))
        .collect();
    let idx = index(&rows);
    let vectors = PremiseVectors::new(&idx, embedding(), &vec![vec![1.0, 0.0]; 25]).unwrap();
    assert_eq!(
        idx.retrieve(
            &sir("True"),
            Some((
                &vectors,
                &query_vector(&query_text(&sir("True")), vec![1.0, 0.0])
            )),
            false
        )
        .unwrap()
        .len(),
        DEFAULT_TOP_K
    );
    assert_eq!(
        idx.retrieve(
            &sir("True"),
            Some((
                &vectors,
                &query_vector(&query_text(&sir("True")), vec![1.0, 0.0])
            )),
            true
        )
        .unwrap()
        .len(),
        REPAIR_TOP_K
    );
}

#[test]
fn document_vector_cache_is_exact_and_detects_header_payload_and_shape_corruption() {
    let rows = [row("A", "True", None), row("B", "Nat", None)];
    let idx = index(&rows);
    let vectors =
        PremiseVectors::new(&idx, embedding(), &[vec![0.25, -2.0], vec![3.0, 4.0]]).unwrap();
    let bytes = vectors.to_bytes().unwrap();
    assert_eq!(
        PremiseVectors::from_bytes(&idx, embedding(), &bytes)
            .unwrap()
            .to_bytes()
            .unwrap(),
        bytes
    );
    for prefix in [0, 8, 15, 16, bytes.len() - 1, bytes.len() - 4] {
        assert!(PremiseVectors::from_bytes(&idx, embedding(), &bytes[..prefix]).is_err());
    }
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        PremiseVectors::from_bytes(&idx, embedding(), &corrupt).is_err(),
        "even finite float corruption detected"
    );
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(&[0; 4]);
    assert!(PremiseVectors::from_bytes(&idx, embedding(), &trailing).is_err());
    let header_length = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let payload = &bytes[16 + header_length..];
    let short = with_payload(&bytes, &payload[..payload.len() - 4]);
    assert!(
        PremiseVectors::from_bytes(&idx, embedding(), &short).is_err(),
        "shape checked even with a valid checksum"
    );
    let mut nonfinite = payload.to_vec();
    nonfinite[..4].copy_from_slice(&f32::NAN.to_le_bytes());
    let nonfinite = with_payload(&bytes, &nonfinite);
    assert!(
        PremiseVectors::from_bytes(&idx, embedding(), &nonfinite).is_err(),
        "cached nonfinite values rejected independently of checksum"
    );
    let mut corrupt = bytes.clone();
    corrupt[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(PremiseVectors::from_bytes(&idx, embedding(), &corrupt).is_err());
    let mut changed_model = embedding();
    changed_model.model.push_str("-v2");
    assert!(PremiseVectors::from_bytes(&idx, changed_model, &bytes).is_err());
    let mut changed_dims = embedding();
    changed_dims.dimensions = 1;
    assert!(PremiseVectors::from_bytes(&idx, changed_dims, &bytes).is_err());
    for field in ["mathlib_commit", "extractor_sha256", "toolchain"] {
        let (mut manifest, text) = source(&rows);
        match field {
            "mathlib_commit" => manifest.mathlib_commit = "c".repeat(40),
            "extractor_sha256" => manifest.extractor_sha256 = "d".repeat(64),
            _ => manifest.toolchain.push_str("-different"),
        }
        let changed = parse(&manifest, &text).unwrap();
        assert!(
            PremiseVectors::from_bytes(&changed, embedding(), &bytes).is_err(),
            "{field}"
        );
        assert!(changed.rank_cosine(&vectors, &[1.0, 0.0], 10).is_err());
    }
    let changed = index(&[row("A", "False", None), rows[1].clone()]);
    assert!(
        PremiseVectors::from_bytes(&changed, embedding(), &bytes).is_err(),
        "row embedding text changed"
    );
    assert!(
        PremiseVectors::new(
            &idx,
            EmbeddingConfig {
                dimensions: 0,
                ..embedding()
            },
            &[]
        )
        .is_err()
    );
}

#[test]
fn query_cache_is_deterministic_and_keys_model_dimensions_and_exact_query() {
    let mut cache = QueryVectorCache::new(embedding()).unwrap();
    cache
        .insert(query_vector("kernel query", vec![1.0, 0.0]))
        .unwrap();
    cache
        .insert(query_vector("other query", vec![0.0, 1.0]))
        .unwrap();
    let mut reversed = QueryVectorCache::new(embedding()).unwrap();
    reversed
        .insert(query_vector("other query", vec![0.0, 1.0]))
        .unwrap();
    reversed
        .insert(query_vector("kernel query", vec![1.0, 0.0]))
        .unwrap();
    let bytes = cache.to_bytes().unwrap();
    assert_eq!(bytes, reversed.to_bytes().unwrap());
    let restored = QueryVectorCache::from_bytes(embedding(), &bytes).unwrap();
    assert_eq!(restored.get("kernel query"), Some(&[1.0, 0.0][..]));
    assert!(restored.get("kernel query ").is_none());
    assert!(restored.get("unknown query").is_none());
    assert!(
        QueryVector::new(
            embedding(),
            "bad",
            EmbeddingInputType::SearchQuery,
            vec![f32::NAN, 0.0]
        )
        .is_err()
    );
    assert!(
        QueryVector::new(
            embedding(),
            "bad",
            EmbeddingInputType::SearchQuery,
            vec![1.0]
        )
        .is_err()
    );
    assert!(
        QueryVectorCache::from_bytes(
            EmbeddingConfig {
                model: "other".into(),
                ..embedding()
            },
            &bytes
        )
        .is_err()
    );
    assert!(
        QueryVectorCache::from_bytes(
            EmbeddingConfig {
                dimensions: 1,
                ..embedding()
            },
            &bytes
        )
        .is_err()
    );
    let mut corrupt = bytes.clone();
    let header_length = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let header = std::str::from_utf8(&bytes[16..16 + header_length]).unwrap();
    let query_hash_offset =
        header.find("query_sha256\":[\"").unwrap() + "query_sha256\":[\"".len() + 16;
    corrupt[query_hash_offset] = if corrupt[query_hash_offset] == b'a' {
        b'b'
    } else {
        b'a'
    };
    assert!(
        QueryVectorCache::from_bytes(embedding(), &corrupt).is_err(),
        "query identity corruption is checksummed too"
    );
    assert!(QueryVectorCache::from_bytes(embedding(), &bytes[..bytes.len() - 1]).is_err());
    let empty = QueryVectorCache::new(embedding())
        .unwrap()
        .to_bytes()
        .unwrap();
    assert!(
        QueryVectorCache::from_bytes(embedding(), &empty)
            .unwrap()
            .get("missing")
            .is_none()
    );
}

#[test]
fn prompts_preserve_rank_and_bound_each_field_and_the_total_with_markers() {
    let idx = index(&[
        row(
            "A",
            &"α".repeat(1_000),
            Some("First sentence. Secret second sentence."),
        ),
        row("B", "Nat", None),
    ]);
    let vectors =
        PremiseVectors::new(&idx, embedding(), &[vec![0.0, 1.0], vec![1.0, 0.0]]).unwrap();
    let hits = idx.rank_cosine(&vectors, &[1.0, 0.0], 10).unwrap();
    let prompt = render_prompt(&hits, PromptLimits::default()).unwrap();
    assert!(prompt.starts_with("B : Nat\n\nA : "));
    assert!(prompt.contains(TRUNCATION_MARKER));
    assert!(prompt.contains("First sentence."));
    assert!(!prompt.contains("Secret second sentence"));
    assert_eq!(
        prompt.lines().nth(2).unwrap().chars().count(),
        "A : ".len() + 300
    );
    let small = render_prompt(
        &hits,
        PromptLimits {
            total_chars: 40,
            ..PromptLimits::default()
        },
    )
    .unwrap();
    assert!(small.chars().count() <= 40);
    assert!(small.ends_with(TRUNCATION_MARKER));
    assert!(
        !small.contains("A :"),
        "do not emit a partial declaration at the total limit"
    );
    assert!(
        render_prompt(
            &hits,
            PromptLimits {
                total_chars: 1,
                ..PromptLimits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn high_level_retrieval_binds_query_vectors_to_runtime_sir_and_model() {
    let idx = index(&[row("A", "True", None), row("B", "Nat", None)]);
    let vectors =
        PremiseVectors::new(&idx, embedding(), &[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
    let runtime_sir = sir("kernel is trivial");
    let text = query_text(&runtime_sir);
    let query = query_vector(&text, vec![1.0, 0.0]);
    assert_eq!(
        idx.retrieve(&runtime_sir, Some((&vectors, &query)), false)
            .unwrap()[0]
            .premise
            .name,
        "A"
    );
    assert!(
        idx.retrieve(
            &sir("kernel is nontrivial"),
            Some((&vectors, &query)),
            false
        )
        .is_err(),
        "another SIR's query cannot be reused"
    );
    let wrong_model = QueryVector::new(
        EmbeddingConfig {
            model: "other/model".into(),
            ..embedding()
        },
        &text,
        EmbeddingInputType::SearchQuery,
        vec![1.0, 0.0],
    )
    .unwrap();
    assert!(
        idx.retrieve(&runtime_sir, Some((&vectors, &wrong_model)), false)
            .is_err()
    );
    let wrong_dims = QueryVector::new(
        EmbeddingConfig {
            dimensions: 1,
            ..embedding()
        },
        &text,
        EmbeddingInputType::SearchQuery,
        vec![1.0],
    )
    .unwrap();
    assert!(
        idx.retrieve(&runtime_sir, Some((&vectors, &wrong_dims)), false)
            .is_err()
    );
    assert!(
        QueryVector::new(
            embedding(),
            &text,
            EmbeddingInputType::SearchDocument,
            vec![1.0, 0.0]
        )
        .is_err(),
        "purpose cannot be silently relabeled"
    );
    let mut cache = QueryVectorCache::new(embedding()).unwrap();
    assert!(cache.insert(wrong_model).is_err());
    assert!(cache.insert(wrong_dims).is_err());
    cache.insert(query).unwrap();
    let bytes = cache.to_bytes().unwrap();
    let cache = QueryVectorCache::from_bytes(embedding(), &bytes).unwrap();
    let restored = cache.get_query(&text).unwrap();
    assert!(
        idx.retrieve(&runtime_sir, Some((&vectors, &restored)), false)
            .is_ok()
    );
    assert!(
        idx.retrieve(&sir("another claim"), Some((&vectors, &restored)), false)
            .is_err()
    );
}

#[test]
fn cosine_signed_zero_is_a_name_tie() {
    let idx = index(&[row("A", "True", None), row("B", "True", None)]);
    let vectors =
        PremiseVectors::new(&idx, embedding(), &[vec![-0.0, -1.0], vec![0.0, 1.0]]).unwrap();
    let hits = idx.rank_cosine(&vectors, &[1.0, 0.0], 2).unwrap();
    assert_eq!(
        hits.iter()
            .map(|h| h.premise.name.as_str())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert!(hits.iter().all(|h| h.score.to_bits() == 0.0_f64.to_bits()));
}

#[test]
fn lexical_retrieval_keeps_set_builder_interval_and_list_content() {
    let idx = index(&[
        row("A", "{x : Nat | UniquePredicate x}", None),
        row("B", "[ListPredicate]", None),
        row("C", "{x | nested [IntervalPredicate x]}", None),
    ]);
    for (query, name) in [
        ("unique predicate", "A"),
        ("list predicate", "B"),
        ("interval predicate", "C"),
    ] {
        assert_eq!(idx.rank_bm25f(query, 1)[0].premise.name, name, "{query}");
    }
}

#[test]
fn streaming_document_cache_matches_in_memory_format_and_rejects_bad_payloads() {
    use std::io::Cursor;
    let idx = index(&[row("A", "True", None), row("B", "Nat", None)]);
    let rows = [vec![0.25_f32, -2.0], vec![3.0, 4.0]];
    let values: Vec<u8> = rows
        .iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let mut output = Cursor::new(Vec::new());
    PremiseVectors::write_stream(&idx, embedding(), &mut Cursor::new(&values), &mut output)
        .unwrap();
    let bytes = output.into_inner();
    assert_eq!(
        bytes,
        PremiseVectors::new(&idx, embedding(), &rows)
            .unwrap()
            .to_bytes()
            .unwrap()
    );
    PremiseVectors::validate_stream(&idx, embedding(), &mut Cursor::new(&bytes)).unwrap();
    PremiseVectors::from_bytes(&idx, embedding(), &bytes).unwrap();
    for bad in [
        values[..values.len() - 1].to_vec(),
        [values.clone(), vec![0; 4]].concat(),
        [f32::NAN.to_le_bytes().to_vec(), values[4..].to_vec()].concat(),
    ] {
        assert!(
            PremiseVectors::write_stream(
                &idx,
                embedding(),
                &mut Cursor::new(bad),
                &mut Cursor::new(Vec::new())
            )
            .is_err()
        );
    }
    for bad in [
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.clone(), vec![0; 4]].concat(),
    ] {
        assert!(PremiseVectors::validate_stream(&idx, embedding(), &mut Cursor::new(bad)).is_err());
    }
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(PremiseVectors::validate_stream(&idx, embedding(), &mut Cursor::new(corrupt)).is_err());
    let mut wrong = embedding();
    wrong.model.push_str("-other");
    assert!(PremiseVectors::validate_stream(&idx, wrong, &mut Cursor::new(bytes)).is_err());
}
