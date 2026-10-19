# M2 SIR crate independent review

Scope: `mls-sir` only, against spec §§4–5, §8 and applicable unit/property/fixture
requirements. Approval of this task does not approve the entire M2 pipeline.

## Candidate 1

Good: `Sir` fields are private; `try_new` and custom `Deserialize` share content
validation. Unknown fields are denied at SIR and variable object levels. Mandatory
`assumptions_made` is represented without a default. No mutable field accessors
allow a validated object to become invalid. `StatementShape` documents explicit
SIR entry counts and does not claim proof of mathematical equivalence. Diffs use
stable schema order and preserve list order. The example fixture matches the spec.

Changes required:

1. **P1 — unsafe whitespace normalization can suppress semantic changes.**
   `normalize_text` uses `split_whitespace`, including within quoted/code text.
   Statements `s = "a  b"` and `s = "a b"` hash identically and are reported
   cosmetic, although their string values differ. Semi-formal fields allow these
   statements. Preserve significant quoted/code content conservatively, document
   the supported cosmetic normalization, and test both semantic differences and
   ordinary cosmetic text whitespace.
2. **P2 — identifier normalization merges distinct labels.**
   `label`, `uses`, and names use the same normalization as prose. Native labels
   `thm:two  spaces` and `thm:two spaces` are distinct source anchors but produce
   identical SIR hashes/diffs. Preserve exact identifier identity or validate and
   enforce a canonical identifier contract across all consumers.
3. **P2 — object-key-order property does not vary serialized key order.**
   `serde_json::Map` without `preserve_order` is sorted; inserting keys in arbitrary
   order still creates the same serialized JSON. Construct JSON objects with
   deliberately reordered textual fields and assert permutations differ, then
   verify decoded hash invariance. Add per-field missing/null/wrong-type and nested
   unknown/missing-field tests for the strict wire contract.

Integration decision for parent: the SIR crate requires a nonempty `label: String`,
while model research proposes a present nullable label and M1 permits unlabeled
blocks. Both can work, but the contract must choose one. A mandatory source string
requires a documented deterministic synthetic-label policy at the batch boundary;
a nullable label must remain explicitly present on the wire so absence still fails.

Independent compiled API probes confirmed identical hash/cosmetic classification
for changed quoted whitespace and changed label whitespace; token concatenation
`x y` versus `xy` correctly remains distinct. Final result pending fixes.

## Final candidate — approved for the SIR crate

All three findings are resolved. Identifier values are exact; semi-formal text
normalizes whitespace outside single/double quoted and backtick-delimited content,
retains content inside delimiters, and falls back to the exact original string for
an unclosed delimiter. `label` is explicitly present and nullable, supporting
unlabeled M1 blocks without fabricating source anchors. Missing label still fails.
The object-order property now constructs reordered JSON text, and strict wire
validation coverage includes every missing top-level field and nested variables.

Independent validation:
- `cargo fmt --check -p mls-sir` passes on the current workspace source.
- `cargo test --offline -p mls-sir` passes all 11 unit + 2 property tests using the
  default Cargo cache, after the local `.cargo-home` proved stale for new model deps.
- An isolated unchanged source copy also passes the same 13 tests and two reviewer
  probes covering quoted/backtick/unclosed whitespace, token and operator changes,
  exact label/name/dependency identities, mandatory-nullable label, and invalid
  nested/wire values. Isolated library Clippy with warnings denied passes.
- Workspace `make test` was attempted; its format gate currently fails only in
  concurrently implemented `mls-models`. The parent explicitly owns the integrated
  gate before committing; this is not a remaining SIR finding.

**APPROVED for `mls-sir` only.** Shape counts remain conservative entry counts,
not a mathematical faithfulness or Lean-equivalence claim. This approval does not
cover model adapters, Lean extraction, evaluation, or the full M2 milestone.

Final direct workspace Clippy also passes:
`cargo clippy --offline -p mls-sir --all-targets -- -D warnings`.
