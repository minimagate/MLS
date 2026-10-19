# M2 model adapter review

Review date: 2026-10-19. Independent review of `crates/mls-models`, model
configuration, synthetic recordings, and the requirements in `docs/spec.md` and
`docs/research/m2-models.md`. This review does not approve the M2 pipeline.

Status: approved for adapter integration after the second re-review. Live account
access and actual provider behavior remain subject to the parent-owned smoke
test; this is not approval of the full M2 pipeline.

## Final re-review

All initial blocking findings and the specific first re-review cases below are
resolved in the current adapter:

- Typed response parsing now consumes recursively sanitized JSON. Regression
  cases verify that a reflected credential in finish reason or model is absent
  from error Display/Debug and recordings.
- Failure accounting recovers top-level model and optional usage independently
  of typed-envelope validation, including non-2xx errors and JSON longer than
  4096 bytes. Missing cost remains unknown.
- Successful and failed calls retain sanitized provider response evidence, HTTP
  status, normalized usage, and configured/resolved models. Existing logs are
  preserved through exclusive creation.
- Replay validates complete request identity, recording version/hash, output
  schema, proof prohibition, answer IDs/types/options/distributions, normal finish,
  successful status, and consistency of duplicated envelope/reply metadata.
- The gated live smoke uses a unique ignored `.mls/log/live-smoke/` directory and
  prints model/cost/latency only. It creates no live client without `MLS_LIVE=1`.

Independent final verification: `cargo test -p mls-models` passes all eleven
adapter tests with approved loopback access. No live requests were made by this
reviewer. Parent orchestration owns the final whole-workspace check and the
single gated live smoke.

## First re-review

The implementation now uses exclusive file creation to preserve existing logs,
validates replay schema/answer content and configured model identity, and records
HTTP status and normalized failure evidence. Present malformed choice
distributions are rejected, missing evidence remains uncertain, zero timeouts are
rejected, and truncation is UTF-8 safe. These changes address substantial parts of
the initial findings.

Approval remains withheld for these specific remaining cases:

- `RawResponse` parses the original unredacted text. An API key reflected in
  `finish_reason` enters the displayed error message; a key reflected in `model`
  enters `ProviderFailure.resolved_model`, Debug output, and recording. Redacting
  only the raw response field does not protect the parsed metadata.
- Typed envelope parse failures and HTTP errors still supply no normalized model
  or usage, even when those fields exist in valid response JSON. Recover them
  independently before typed validation. Truncation before parsing can also erase
  the structured evidence in an ordinary long invalid completion.
- Successful raw provider responses and token-detail metadata remain discarded.
- Replay accepts a successful normalized generation whose finish reason is
  `length`, and does not check successful status or consistency between envelope
  metadata and reply metadata.

The configured model or a version suffix beginning with `-` permits the canonical
Jev version documented by research and the synthetic Sonnet version fixture.
It rejects unrelated model families. Live identity behavior still needs the
parent-owned gated smoke test.

## Initial findings

1. **Call accounting loses failed-response evidence.** The recording wrapper
   retains metadata only from successful normalized replies. Provider HTTP bodies,
   success status, and model/usage from schema-invalid, refused, or truncated
   completions are discarded. Those responses may incur cost and need to remain
   auditable as attempted calls, with unknown cost distinct from zero.
2. **Replay does not enforce the model boundary.** Replay deserializes normalized
   replies without validating generation output against the requested schema,
   rejecting Lean proof content, or checking decision answer IDs/options/types.
   Correct request hashes do not protect malformed or manually authored outputs.
   The direct constructor also bypasses the recording-version check.
3. **A new recorder overwrites previous calls.** Every instance starts at index
   zero and uses `fs::write`, replacing existing `call-0000.json` files.
4. **Resolved model identity is unchecked.** Any nonmatching or empty model
   string can be accepted as a successful result. Preserve valid resolved versions
   while rejecting unrelated model replies and inconsistent replay metadata.
5. **Error sanitation is incomplete.** Transport sanitation truncates bytes rather
   than redacting credentials and can split a UTF-8 character. Schema errors can
   embed untrusted provider content. Raw-body retention must redact credentials
   before errors, logs, or debug output can expose them.

Choice confidence and probability are correctly distinct, and missing evidence
produces uncertainty in `assess`. Malformed present distributions should also be
validated consistently at live/replay boundaries. Add regression coverage for
model/prompt/schema mismatches, malformed recorded replies, HTTP/error recording,
and preserving existing log files.

## Baseline evidence

- Documented wire routes and typed Noul/choice envelopes match the research:
  `/api/v1/chat/completions` and `/api/alpha/decisions`.
- Live generation runs actual local JSON Schema validation, checks normal finish,
  null content, refusal, and unexpected Lean proof content. Missing usage/cost
  remains optional rather than becoming zero.
- Full logical generation and decision requests are hashed for ordered replay;
  replay has no live-client transport fallback or credential access.
- `make test` passed: formatting, strict workspace Clippy, and workspace tests,
  including all seven model adapter tests. Loopback transport mocks needed the
  approved sandbox escalation; three mock tests fail with EPERM in the restricted
  sandbox. The live test returns early unless `MLS_LIVE=1`.
- No live model calls were made by this reviewer. Parent orchestration owns the
  separately gated live smoke verification.
