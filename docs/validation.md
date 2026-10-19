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

This experiment used hand-authored carriers. Repeating it with the production
writer is still required before M1 acceptance.
