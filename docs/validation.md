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
