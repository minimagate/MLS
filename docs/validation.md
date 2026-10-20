# Executed validation evidence

This log records executed checks, not planned capabilities. The completion
checklist remains in [progress.md](progress.md).

## 2026-10-19 — M1 metadata carrier experiment

Installed TeXmacs reported **2.1.5**. A disposable document contained a theorem
inside a short `<with|mls-id|UUID|...>` and a two-paragraph proof inside a long
`<\\with|mls-id|UUID>...</with>`.

Executed the native application headlessly:

```sh
/Applications/TeXmacs.app/Contents/MacOS/TeXmacs \
  -H -C /tmp/mls-id-roundtrip.tm /tmp/mls-id-saved.tm -q
```

The saved native document retained both UUID values and both proof paragraphs.
The native version header changed from the test's 2.1.4 to 2.1.5, showing that
TeXmacs parsed and rewrote the file rather than just copying it.

Exported the wrapped document and a baseline with wrappers removed to LaTeX
using `-H -C INPUT.tm OUTPUT.tex -q`. `diff -u` returned exit status 0: the two
exports were byte-identical, including theorem environment, proof paragraphs,
and proof end marker. This supports the choice of an unused `with` binding as
rendering-neutral persistent metadata; it does not prove all editing operations
or all document styles preserve IDs.

This initial experiment used hand-authored carriers. The production checks below
supersede it for M1 acceptance.

## 2026-10-19 — M1 final acceptance evidence

- `CARGO_HOME="$PWD/.cargo-home" make test` passes formatting, warnings-as-errors
  Clippy, and 34 tests: 3 CLI, 2 core, 25 parser/extractor/writer, 2 persistence,
  and 2 golden tests. The local Cargo home is used only to keep dependency-cache
  writes within the execution sandbox. Runtime tests are offline and use no model.
- The independent reviewer reran the final suite and resolved every reported
  finding; see [review and approval](reviews/m1.md).
- A read-only sweep accepted all **424** English `.tm` documentation files in
  the installed TeXmacs distribution. A second sweep copied each document into a
  disposable directory, ran `mls --write-ids`, reopened it with the same command,
  and compared the `(kind, id)` sequence and saved bytes. **424 passed, 0 failed**;
  the second write changed no bytes. Original installed files were untouched.
- The checked-in three-document official corpus has normalized golden JSON and
  persistent-ID roundtrip tests; its license and provenance accompany the files.
- A generated fixture containing formatted prose, an inline reference, a
  definition, a theorem, and a two-paragraph proof survived native TeXmacs 2.1.5
  save with all four identities unchanged and no diagnostics. LaTeX exports
  before and after MLS annotation were byte-identical (`diff -u`, exit 0).
- The same native save/export check on the real `man-environments.en.tm` fixture
  retained all **9** block identities with no diagnostics and byte-identical
  LaTeX exports. This exercised the actual production writer, not manually added
  wrappers.

The broader corpus test exposed escaped-space trimming that the smaller corpus
missed; it was fixed with a regression and the writer now validates generated
syntax before replacing the source. Native export comparison similarly caught
paragraph separators being absorbed into wrappers; the final comparison passes.
These checks establish M1 behavior only. No Lean/model/plugin capability has yet
been validated.

## 2026-10-19 — M2 SIR task acceptance

The isolated `mls-sir` task passed independent review after fixing quoted-content
and source-label hash collisions. Its 11 unit tests and 2 property tests pass;
reviewer probes also checked literal whitespace, exact identities, malformed
wire values and mandatory-nullable labels. See [SIR review](reviews/m2-sir.md).

To validate the exact incremental commit while the model adapter was changing
the shared workspace, the parent created a disposable copy of M1 commit
`6a10a97`, added the final SIR source and its workspace membership, and regenerated
the lockfile offline. The lockfile delta contains only the new SIR package.
`make test` in that exact snapshot passed formatting, Clippy and all **47** tests
(34 M1 + 13 SIR). This establishes the SIR task, not the entire M2 milestone.

## 2026-10-19 — M2 model adapter acceptance

The adapter passed independent review after resolving failed-call accounting,
credential reflection, replay validation, resolved-model identity and recording
overwrite findings. See [model review](reviews/m2-models.md). Eleven adapter tests
cover schema validation, typed decisions, synthetic replay, malformed outputs,
HTTP failures, redaction, log append and metadata consistency. The live test is
explicitly gated and returns without calling a provider in ordinary test runs.

The parent ran `MLS_LIVE=1 cargo test -p mls-models
live_openrouter_smoke_explicitly_gated -- --nocapture`; it passed both real calls:

- Sonnet resolved to `anthropic/claude-sonnet-5`: 201 input / 12 output tokens,
  reported cost $0.000522, transport latency 1576 ms.
- Jev resolved to `typesafe/jev-1.14-20261014`: 345 input / 49 output tokens,
  reported cost $0.00001449, transport latency 313 ms.

Total provider-reported cost was $0.00053649. Sanitized records remain local at
`.mls/log/live-smoke/25511-1792442239096/`; no credential was printed or committed.
These calls validate provider integration only, not autoformalization quality.

The exact model-adapter commit snapshot passed `make test`: formatting, strict
Clippy and 58 test entries (including the live-gated entry, which made no network
call in that run). The separate two-call live smoke above also passed. Parallel
Lean work was excluded from this snapshot and is not covered by this acceptance.

## 2026-10-19 — M2 Lean runtime and worker acceptance

Independent [Lean review](reviews/m2-lean.md) approved the scoped runtime and
worker after fixes for executable term admission, shadow metadata injection and
unbounded unterminated stdout reads. Five Rust unit and six subprocess tests
passed, along with strict Clippy. `make test-lean` rebuilt the trusted extractor
and passed four real persistent-REPL Python tests plus the real Rust worker test.

The real suite checks parsed header boundaries, universal conclusions, function
result types and definition bodies, proposition variables versus hypotheses and
instances, direct/transitive `sorryAx`, clean trusted proofs, failed branches and
repair. Parse-only admission rejects appended commands, effectful elaborators,
tactics in types and non-placeholder theorem bodies. Explicitly supported real
number arithmetic and boundedness definitions pass. Independent probes also
confirmed Unicode diagnostic columns. Unsupported syntax is rejected, not
silently accepted.

Lean 4.19.0, Mathlib and REPL pins match the investigation. The official downloaded
archive's local SHA-256 is
`09f5534815844869414274628b8dfa86be0a20df2d38c2a615e9afc0b06536c0`;
this is repeatability evidence, not an upstream published digest. The native
linked executable fails this host's dyld checks; the tested launcher runs the
same compiled pinned REPL module through official Lean's interpreter. It stays
persistent and imports Mathlib once. No pipeline, SIR equivalence or proof-status
acceptance is implied by these component tests.

The exact incremental runtime/worker commit snapshot passed `make test` (70
test entries, including two inactive live/runtime gates) and `make test-lean`
(four real Python cases and one real Rust case). A separate `lake build Shadow`
probe compiled two generated section modules with a dependency import. The
versioned Lake template declares the Shadow submodule target; setup creates its
directory. The proof-placeholder warning was expected. Parallel batch-engine
changes were excluded from the snapshot.

## 2026-10-20 — M2 batch pipeline acceptance

- `make test` (evaluator unit tests, `cargo fmt --check`, Clippy with
  `-D warnings`, workspace tests) and `make test-lean` (extractor rebuild, real
  REPL admission/extraction tests, real Lean worker, engine pipeline and CLI
  batch tests) passed after the final iteration-3 changes.
- Regression tests cover interleaved pending candidates in two
  `LeanStatementChecker`s (real Lean), stale evidence after a mismatch followed
  by an elaboration failure, diagnostics cleared after a later hard mismatch,
  non-UTF-8 blocks, report-before-shadow and output/ownership preflights.
- Independent differential probes compared the engine's outer-head recognizer
  with pinned-Lean extraction on several thousand generated conclusions;
  reviewers found no wrong `matched` result in the final rounds. Admission
  probes (about 190 real REPL cases) found no admitted tactic or elaborator
  after `deriving` and proof-escape identifiers were blocked.
- Three live runs over the ten-case corpus cost $0.267934, $0.179008 and
  $0.163104 (74 calls, all with reported cost). Each was replayed offline from
  its captured calls with byte-identical block reports and usage. Results and
  manual mathematical inspection are in [evals.md](evals.md).

These results support the M2 go decision only within the limits stated in
`evals.md`. No status in M2 is confirmed or verified.

## 2026-10-20 — M3 TeXmacs review acceptance

- `make test` (243 Rust test entries plus evaluator tests, formatting and
  strict Clippy), `make test-lean` (real REPL, worker, pipeline and batch),
  `make test-texmacs` and `MLS_LEAN_TESTS=1 make test-texmacs` (319 headless
  TeXmacs 2.1.5 checks in 10 files) passed. The gated real end-to-end test drives
  the plugin's own procedures against the real `mls` binary with replayed model
  calls and real Lean: two drafts, confirming the dependent block waits for its
  dependency, confirming both yields `statement_confirmed` without model calls,
  and rejection yields `not_formalized` with a rerun request (42 checks).
- A 40-seed property test asserts the confirmation contract over random
  sequences of confirmations, rejections, edits, text edits and label renames.
- Block identity: real TeXmacs load-and-save of 40 sampled installed documents
  (340 blocks) left every canonical block text unchanged; an oracle using
  TeXmacs' own parser on 44,000 generated sources found 0 cases where different
  trees share a canonical text (63 before the reader-alignment fixes). All 424
  installed English documents parse, and `--write-ids` is idempotent on copies
  of all 424. One document is refused for raw data, a TeXmacs limitation.
- Plugin safety reviews found no shell injection or evaluation of engine text
  (hostile names, paths and `<action|…>` content stay inert). Saves are verified
  by re-importing the file; reloads by carrier-stripped tree equality.
- Not verified (no GUI automation available): menu and shortcut, colored panel
  rendering, tooltips, review-card dialog rendering and button callbacks, footer
  repaint. The logic behind each is exercised headless; a manual GUI pass is
  still owed.
