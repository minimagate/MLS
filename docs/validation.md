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
