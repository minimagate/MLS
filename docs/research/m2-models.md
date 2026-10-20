# M2 model integration research

Checked 2026-10-19. This is an implementation recommendation, not a live inference
result. Research made only unauthenticated GET requests; no paid model requests,
API-key inspection, dependency installs, or runtime changes.

## Verified models and endpoint correction

The live [text catalog](https://openrouter.ai/api/v1/models) contains both requested
Sonnet IDs. The live [decisions catalog](https://openrouter.ai/api/v1/models?output_modalities=decisions)
contains Jev. The default catalog filters to text; absence there does not imply
that Jev is unavailable. This query parameter is specified in the
[public OpenAPI schema](https://openrouter.ai/openapi.json).

| Config role | Exact request ID | Context tokens | Input/output USD per million tokens |
| --- | --- | ---: | ---: |
| Generation default | `anthropic/claude-sonnet-5` | 1,000,000 | 2 / 10 |
| Optional later A/B | `anthropic/claude-sonnet-5.5` | 1,000,000 | 2 / 10 |
| Decisions default | `typesafe/jev-1.14` | 262,144 | 0.042 / 0 |
| Moving alias, avoid in reproducible runs | `~typesafe/jev-latest` | 262,144 | 0.042 / 0 |

The Jev catalog reports canonical slug `typesafe/jev-1.14-20261014`; configure the
listed request ID and retain the returned `model` on every call. Do not assume a
canonical slug is itself an accepted request ID. No unavailable-model substitute
is needed. Jev 1.13 remains listed, currently also at 262,144 context, rather than
the historical 32k figure in the spec. Keep local context small regardless.

Both Sonnet entries advertise `response_format` and `structured_outputs`. Neither
Sonnet 5 entry advertises `temperature`; omit that parameter. These observations
are a catalog snapshot, not a guarantee of account access or future availability.

Spec correction: one OpenRouter key serves two distinct API surfaces:

- Sonnet: `POST https://openrouter.ai/api/v1/chat/completions`.
- Jev: `POST https://openrouter.ai/api/alpha/decisions`.

The Jev route is **not** under `/api/v1`, and it is not a chat-completions model.
OpenRouter also supports `/api/v1/systemone` for the TypeSafe SDK, but direct HTTP
via Decisions is the simpler Rust integration. See the
[official Jev guide](https://openrouter.ai/docs/guides/community/jev).

## Sonnet structured generation

Use an ordinary non-streaming chat request with explicit configured `model`,
`messages`, a bounded `max_tokens`, and:

```json
{
  "provider": {"require_parameters": true},
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "mls_statement_sir",
      "strict": true,
      "schema": {"type": "object", "properties": {}, "required": [], "additionalProperties": false}
    }
  }
}
```

The empty schema above is only the envelope illustration: populate it with the
full SIR schema. Use separate schema names for SIR and Lean output. Put
`additionalProperties: false` on nested object schemas as well. Provider support
varies; `require_parameters` prevents silently routing to an endpoint that cannot
honor requested parameters. Still parse and validate locally; enforcement differs
across providers. [Structured-output documentation](https://openrouter.ai/docs/guides/features/structured-outputs).

Project requirements from §5/§8:

- SIR requires all fields: `kind`, nullable `label`, `variables`, `hypotheses`,
  `conclusion`, `uses`, `assumptions_made`. Variables require `name` and `type`.
  Arrays may be empty; missing `assumptions_made` must fail validation.
- One formalization output requires exactly `sir`, `lean_statement`, nullable
  `lean_proof`, and `notes` (string array). M2/M4 require a null proof;
  `mathlib_names_used` is not part of the schema.
- Reject null/absent assistant content, refused or truncated completion, malformed
  JSON, unrecognized fields, invalid kind, and invalid dependency references.
  Structured shape alone says nothing about mathematical faithfulness.
- Each initial and repair call returns the SIR and Lean statement together. Include
  source, accepted dependency SIRs/declarations and applicable review context. A repair
  includes the full previous combined response and exact Lean/structure diagnostics;
  the model may correct both SIR and Lean. Never provide expected/reference SIRs.
- M2 batch checking is a draft/evaluation path, not human confirmation. Successful
  elaboration must not imply that a user confirmed the statement.

Recommended starting project defaults (policy choices, not vendor requirements):
`formalization_max_tokens=4096`, request timeout 120 seconds, and a bounded
combined-formalization attempt count. Record all attempts and costs.
Do not silently substitute models when a configured model fails.

## Typed Jev requests and confidence

The [Decisions API reference](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request)
specifies `{model, state, questions}`. `state` accepts a string, object, or array.
Questions are keyed by caller-selected IDs, and answers preserve those IDs.

Illustrative MLS request (not a recorded response):

```json
{
  "model": "typesafe/jev-1.14",
  "state": {
    "block": "Every bounded monotone real sequence converges.",
    "sir_conclusion": "Every real sequence converges."
  },
  "questions": {
    "omitted_condition": {
      "type": "noul",
      "instructions": "Does sir_conclusion omit a condition explicitly present in block?",
      "criteria": {
        "true": "An explicit restriction in block is absent from sir_conclusion.",
        "false": "All explicit restrictions are retained."
      }
    }
  }
}
```

Use `noul` for Boolean questions: answer `{type: "noul", noul: p}` where `p` is
the probability of yes. There is no separate Boolean or Noul confidence field.
Use `choice` with `criteria` mapping allowed strings to clear descriptions for
bounded alternatives; the answer has a `choice` and normally `probabilities` and
`confidence`. Score/Box are unnecessary for current MLS requirements.

The upstream [TypeSafe API](https://docs.typesafe.ai/api) notes that question IDs
are not sent to the model, so all semantics must appear in instructions/criteria.
Choice is single-selection, not a set of arbitrary labels: for multiple implicit
dependencies use one Boolean question per candidate or repeated bounded choices
including `none`; never accept a label outside the candidates. Send just the
block, the relevant SIR field and a bounded list of nearby labels/context.

Confidence measures distribution concentration, not correctness. Do not equate
it to the probability of the chosen answer. Keep both the confidence and full
probability distribution. Thresholds require evaluation on MLS examples.
[TypeSafe confidence guidance](https://docs.typesafe.ai/confidence).

Recommended initial policy, explicitly provisional until calibrated:

- Choice: act only if returned confidence and selected-option probability are both
  at least 0.9. Missing probability/confidence means uncertainty, not success.
- Noul: yes for `p >= 0.9`, no for `p <= 0.1`, otherwise uncertain.
- Reject non-finite/out-of-range probabilities, unknown answers, mismatched answer
  types/IDs, and incomplete distributions. Allow reasonable probability-sum
  rounding tolerance; malformed responses follow the uncertain/error path.
- Preserve §6 fallbacks: commentary for uncertain classification, wait for an
  uncertain completion gate, semantic for an uncertain edit, Sonnet linking for
  uncertain references, and flagged fields for uncertain SIR checks.
- These decisions annotate review/evaluation. They cannot confirm a SIR or turn a
  model failure into mathematical refutation. M2 needs field checks; live gating,
  debounce, and scheduler belong to M4.

## Usage, cost and replay design

Chat response usage uses `prompt_tokens`, `completion_tokens`, `total_tokens`,
optional token-detail objects and `cost`. Current usage docs say this is returned
automatically; `usage: {include: true}` is deprecated. Save `id`, returned `model`,
provider if present, and finish reason as well.
[Official usage accounting](https://openrouter.ai/docs/guides/administration/usage-accounting).

Decisions usage instead uses `input_tokens`, `output_tokens`, and optional `cost`.
Its OpenAPI schema requires `model`, `answers`, and `usage`, but makes `id`,
`provider`, choice probabilities/confidence and cost optional. Keep options in the
wire types; apply stricter MLS decision validation before acting. Never map
missing cost to zero. Record missing usage as unknown and exclude it from claims
of complete cost accounting. Retain raw response bodies with normalized usage.
[Public API schema](https://openrouter.ai/openapi.json).

Recommended Rust boundary (names illustrative, no runtime implementation here):

```rust
trait ModelClient {
    fn generate(&mut self, request: &GenerationRequest)
        -> Result<ModelReply, ModelError>;
    fn decide(&mut self, request: &DecisionRequest)
        -> Result<DecisionReply, ModelError>;
}
```

`GenerationRequest` should include task stage, model, messages, schema version and
schema, generation limits, and provider policy. `DecisionRequest` holds model,
state and typed questions. A live client supplies these through HTTP; a recording
wrapper saves sanitized request/response pairs; replay implements the same trait
and has no network or credential access.

Record a versioned envelope containing endpoint kind, block ID, attempt index,
canonical request body/hash, requested and served models, response status/body,
parsed usage, elapsed milliseconds, thresholds applied, and outcome/error. Secret
headers must never be serialized. Keep local run logs under `.mls/log`; check in
only deliberate fixture recordings. Mark manually authored fixtures as synthetic
rather than claiming they came from OpenRouter. Live fixtures include their source
and capture date. Costs from replay are historical, not newly incurred spend.

Replay must compare the full canonical request or a hash covering all its fields;
matching only a label/stage can conceal prompt regressions. On missing/mismatched
recording fail offline, never fall back to live. For retry sequences use ordered
recordings with explicit attempt numbers. Keep error responses replayable too.

## Definition of tested

Before integration, tests should cover:

1. Exact request envelopes for both endpoints and all required SIR/Lean fields.
2. Parsing representative success plus error, truncated, refusal, malformed JSON,
   unknown label, missing probabilities, out-of-range and absent-cost cases.
3. Threshold boundaries and every deterministic fallback without paid calls.
4. Round-trip recording/replay, prompt/model/schema mismatch, and proof that an
   unmatched recording cannot invoke live transport.
5. Aggregation of all attempts, unknown-cost propagation, and no fabricated zero.
6. Environment-gated live tests (`MLS_LIVE=1`) using the local
   `OPENROUTER_API_KEY`; require explicit gating before constructing live clients.
7. M2 evaluation reports success, structure-check rate, expected-SIR agreement,
   repair count, recorded-versus-live provenance, and measured cost separately.

No live inference was performed in this research. Catalog and schema verification
prove availability/documented shape, not successful authentication or model quality.
