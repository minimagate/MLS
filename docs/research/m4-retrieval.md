# M4 premise retrieval research

**PARTIAL — remaining:** the final recommendation (embeddings primary with
configurable model id, default `baai/bge-m3`, alternative `openai/text-embedding-3-small`;
BM25F as offline fallback), embedding text per declaration, vector storage format,
query text from the SIR, top-k for prompts and replay determinism. Embedding recall is
unmeasured; the orchestrator runs the paid build later. `lean/MlsDumpIndex.lean` is in
place and verified: the repo copy reproduces the index byte-for-byte.

Investigated 2026-10-20 with the pinned Lean 4.19.0 and Mathlib
`c44e0c8ee63ca166450922a373c7409c5d26b00b`. Measurement prototypes remain in
scratch (`m4-research-ret/`); the verified extractor is now versioned at
`lean/MlsDumpIndex.lean`. No paid model calls were made in this research checkpoint;
OpenRouter was read only through unauthenticated GETs.

## 1. Index extraction (measured)

Prototype `MlsDumpIndex.lean` is an `elab "#mls_dump_index"` command. It walks
`env.constants.map₁` (imported constants) and writes sorted JSON Lines. It runs with
`lake env lean` inside `.mls` (already built, no build step), so no native executable
is linked and the dyld workaround is not needed.

Filters: module root `Mathlib` (drops 381,262 Lean, Batteries, Aesop, Std constants);
drop private names and names with `_`, numeric, `match_N`, `proof_N`, `eq_N`, `eq_def`,
`sizeOf_spec`, `casesOn`, `recOn`, `brecOn`, `below`, `noConfusion` components (538,137);
drop `isAuxRecursor`, `isNoConfusion`, `isMatcherCore` (20); keep theorem, def,
inductive, constructor, axiom, opaque (drops recursors and quotients, 3,095).
Instances are kept with `instance: true` (24,492) and excluded by the retriever.
The component blacklist is intentionally lossy: it also excludes rare public names
such as `NONote.below`. Counts and recall below describe this fixed filter, not every
public Mathlib declaration.

| Measure | Value |
|---|---|
| Rows kept (incl. deprecated) | 249,110 (theorem 196,020; def 45,960; constructor 3,734; inductive 3,090; opaque 306) |
| Retrievable rows (theorem or def, not deprecated) | 237,585 |
| Rows with docstring | 68,201 |
| Deprecated rows (flagged) | 4,436 |
| Size / wall time | 102 MB JSONL (11 MB gzipped) / 4 min 50 s |
| Determinism | Two runs byte-identical (sha256 `ae41fbb8...`) |

Proposed row format, one JSON object per line, keys in Lean's sorted order:
`{"name","kind","module","instance","deprecated","doc"|null,"type"}`. `type` is
`ppExpr` output and may contain newlines. A `manifest.json` beside it records the
Mathlib commit, toolchain, extractor version, filter version, row count and file sha256.

## 2. Embeddings on OpenRouter (checked 2026-10-20)

- Available. `POST https://openrouter.ai/api/v1/embeddings` takes `model`, `input`,
  optional `dimensions` (integer, at least 1), `encoding_format` (`float` or `base64`)
  and `input_type`. Docs: https://openrouter.ai/docs/api/reference/embeddings
- Catalog: `GET https://openrouter.ai/api/v1/embeddings/models` lists 33 models.
  Prices are catalog `pricing.prompt` values, USD per token, shown here per 1M tokens.

| Model | USD / 1M tokens | Context | Dimensions |
|---|---|---|---|
| baai/bge-m3 | 0.01 | 8,194 | 1024 (catalog description) |
| openai/text-embedding-3-small | 0.02 | 8,192 | `dimensions` parameter (docs example 1536) |
| voyageai/voyage-4-lite | 0.02 | 32,000 | 2048, 1024, 512, 256 (catalog description) |
| qwen/qwen3-embedding-4b | 0.02 | 32,768 | not stated in catalog |
| openai/text-embedding-3-large | 0.13 | 8,192 | `dimensions` parameter |
| google/gemini-embedding-001 | 0.15 | 20,000 | not stated in catalog |

Free `:free` embedding models exist but are unsuitable for a reproducible index.

- Index cost, estimated and not tokenizer-measured. The retrievable text (name, type,
  docstring) is 65.6M characters, or about 16-22M tokens at 4 to 3 characters per
  token. That is about $0.33-0.44 with the 0.02 models, $0.16-0.22 with bge-m3, and
  $2-3 with the large models. Embedding is paid once per Mathlib commit.
- Query cost is under $0.00001 per query (about 300 tokens).
- Caching: `.mls/index/mathlib-<commit>/vec-<model>-<dims>.f32` with a header (commit,
  model, dims, per-row text sha256); query vectors keyed by sha256(model | dims | query).
- Hosted vectors may change across provider versions. Replay must read stored vectors
  and never call the API.

## 3. Lexical baseline (measured, stdlib Python prototype)

Prototype `bm25_proto.py` runs fielded BM25 (k1 1.2, b 0.75) over the 237,585
retrievable rows. Fields: name tokens (weight 3), docstring tokens (weight 1), type
tokens (weight 1). Instance `[...]` and implicit `{...}` binder groups are removed
from type text first.

Tokenizer: fixed notation map (`≤`→le, `∀`→forall, `|x|`→abs, `ℝ`→real), CamelCase,
dot and underscore splitting, lowercasing, dropping one-character tokens and a
stoplist. Ties break on name.

Queries use only the reference SIR: variable names and types, hypotheses and
conclusion. Variant L additionally uses the SIR `label`, which carries the defined
term (`def:kernel`). `source_text` and `kind` are never used. The relevant sets were
fixed before scoring, and every name was checked to exist in the index:
triangle {abs_add, abs_add_le}; maximum {IsCompact.exists_isMaxOn,
ContinuousOn.exists_isMaxOn', CompactIccSpace.isCompact_Icc}; intermediate
{intermediate_value_Icc, intermediate_value_Icc', ContinuousOn.surjOn_Icc};
bounded-on {BddAbove, Bornology.IsBounded, abs_le}; convergent-bounded
{Filter.Tendsto.bddAbove_range, Metric.isBounded_range_of_tendsto, Bornology.IsBounded};
monotone-lower {Monotone, monotone_nat_of_le_succ}; linear-zero {map_zero,
LinearMap.map_zero}; kernel {LinearMap.ker, LinearMap.mem_ker}; kernel-add {map_add,
LinearMap.map_add, LinearMap.mem_ker}; square-nonnegative {sq_nonneg, pow_two_nonneg}.

Variant B adds a fixed abbreviation table (nonneg, bdd, ker, sq, monotone, tendsto,
max), written after inspecting candidate names, so B and LB are optimistic.

| Variant (instances excluded) | Case hit@10 | Name recall@10 | @20 | @100 |
|---|---|---|---|---|
| A: SIR content only | 0/10 | 0.00 | 0.00 | 0.27 |
| B: A + abbreviation table | 0/10 | 0.00 | 0.00 | 0.32 |
| L: A + SIR label | 2/10 | 0.13 | 0.27 | 0.42 |
| LB: B + label | 5/10 | 0.40 | 0.47 | 0.63 |
| LB, name field only | 5/10 | 0.23 | 0.42 | 0.60 |

Name recall is the mean fraction of each case's relevant names in the top k.

LB top-10 results, with rank of each relevant name in parentheses:
- Hits: triangle (abs_add_le 1, abs_add 2); square-nonnegative (sq_nonneg 1);
  kernel (LinearMap.ker 1, LinearMap.mem_ker 3); maximum (ContinuousOn.exists_isMaxOn'
  3, IsCompact.exists_isMaxOn 4); intermediate (intermediate_value_Icc 7).
- Misses: bounded-on (BddAbove 43); convergent-bounded (Filter.Tendsto.bddAbove_range
  507); monotone-lower (monotone_nat_of_le_succ 51, Monotone 523); linear-zero
  (LinearMap.map_zero 83); kernel-add (LinearMap.mem_ker 29, LinearMap.map_add 932).

Findings:
- The scorer is sound: with concept words in the query (`abs add`), the right name ranks first.
- Recall fails when the SIR lacks the Mathlib concept word (compact, ker, Tendsto,
  BddAbove); the kernel definition's term appears only in its label.
- Glue tokens (real, le, add, forall) dominate and pull in Real.* lemmas. On SIR
  content alone, lexical retrieval is unusable at k=10.

## 4. Recommendation

1. Retriever: use embedding similarity as the primary ranker, configured in
   `config/models.json`. The default is `baai/bge-m3` at 1024 dimensions;
   `openai/text-embedding-3-small` remains an alternative. BM25F is the deterministic
   offline fallback when the validated vector cache is unavailable. Query fields are
   the runtime SIR's variables, hypotheses, conclusion and label. Reference SIRs are
   used only by this offline lexical measurement and never enter a model request.
2. Embedding text: join the declaration name, pretty-printed type and first docstring
   sentence with explicit field markers and bounds. Send index rows as
   `search_document` and SIR queries as `search_query`. Cache every returned vector;
   replay reads cached vectors and makes no provider call.
3. Index location and versioning: `.mls/index/mathlib-<commit>/` holds
   `premises.jsonl`, `manifest.json` (commit, toolchain id, extractor version, filter
   version, row count, sha256) and later the vector cache. Regenerate when any manifest
   field changes. The directory is git-ignored. The versioned extractor is
   `lean/MlsDumpIndex.lean`.
4. Premises per prompt: top 10 (`name : type`, type truncated to about 300 characters,
   plus the first docstring sentence); 20 on a repair retry. LB recall is 0.40 at 10
   and 0.47 at 20.
5. Determinism for record and replay: the ranking is a pure function of the manifest
   sha256, the sorted query tokens, the weights, k1, b and name tie-breaking. Record
   the query tokens, the ordered premise names and the manifest hash in each call
   record. On replay, recompute and fail on mismatch. Embedding vectors are read from
   the cache and never re-requested. Use a fixed prompt template with premises in rank
   order.

## 5. Limits

- Embedding recall is unmeasured (no paid calls). The relevance sets are small (10
  cases, 2-3 names each) and hand-picked, so this is a sanity check, not a benchmark.
  Reference SIRs stand in for human-confirmed SIRs.
