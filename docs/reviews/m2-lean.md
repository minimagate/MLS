# M2 Lean runtime and worker review

Reviewed 2026-10-19 against the in-progress `lean/`, `scripts/`,
`tests/lean/`, and `crates/mls-lean/` implementation. This review does not cover
the subsequent SIR comparison or batch pipeline.

**Disposition: approved for the scoped M2 runtime and worker task.** All
blocking findings below are resolved. This is not acceptance of the full M2
pipeline or a claim of semantic equivalence between SIR and Lean.

## Final re-review and evidence

The worker fixes for findings 2 and 3 are accepted on inspection and tests:
shadow IDs/dependencies/spans use `BlockId`; section names must be a single
ASCII module component beginning with an uppercase letter; generation and
path construction share this validation without lossy rewriting. The
stdout reader now checks its bound incrementally, including unterminated
output. The regression uses 100 KB of output without a newline.

Repeated `cargo test -p mls-lean` passed five unit and six subprocess tests;
`cargo clippy -p mls-lean --all-targets -- -D warnings` passed. The real test
was still gated off in this offline run.

The runtime replaced the syntax-kind substring blacklist with an explicit
allowlist, closing the identified `eval%` gap. The theorem placeholder now
requires a `byTactic` body root and a single admitted sorry tactic; tactics
in signature types are rejected. Extraction of trusted fixture proofs is
separate from admission of model-generated statements.

An additional positive review probe found that ordinary `ℝ` notation was
initially rejected. Explicit standard numeric-domain syntax kinds were
added. The checked-in suite now admits real-valued declarations, a real
square inequality, and a boundedness definition using sets, quantifiers,
membership, arrows, and `abs`.

Final independent `make test-lean` completed with exit 0: `MlsExtract`
compiled, all four Python real Lean tests passed (3.820 seconds), and the
enabled Rust persistent-worker integration passed (3.62 seconds). This
includes persistent environments, direct/transitive `sorryAx`, a clean
trusted `rfl` proof, header/body extraction, rejected executable syntax,
and repair from the prior environment. The interpreter launcher now loads
compiled `REPL.Main` through a small versioned entry module; the run remains
a persistent pinned REPL and starts in seconds.

Additional independent real parse-only probes rejected a wrapper around
`by sorry`, a tactic in a header type, appended `#eval`, duplicate sorry
tactics, and `eval%`. A real unknown-name error after Unicode `α` appeared
at line 2, scalar column 36, confirming the column convention used by the
Rust mapper. No executable IO payload was run.

## Initial findings (resolved)

1. **Executable term syntax escapes the admission blacklist.**
   `lean/MlsExtract.lean` checks syntax-kind substrings `Tactic` and `Command`
   plus a few atom values. The imported Mathlib `eval%` elaborator has kind
   `Mathlib.Meta.eval_expr`, so that filter admits it. Its implementation in
   the pinned `Mathlib/Tactic/Eval.lean` explicitly invokes `Meta.evalExpr`
   with `safety := .unsafe`. This permits interpreter execution while a
   generated definition or type is elaborated; `unsafeIO` is an identifier,
   not the forbidden atom `unsafe`. The Rust pre-validation round trip is
   correctly ordered, but its guarantee is only as strong as this validator.
   Use a conservative supported syntax policy and reject executable term
   extensions before elaboration. Require real parse-only regressions for
   `eval%`, tactics inside signature types, command splices, and appended
   commands, together with positive first-year mathematical expressions.
   This review inspected the installed implementation; it did not execute an
   IO payload.

2. **Shadow metadata can become Lean source.**
   `ShadowDeclaration.block_id` remains an arbitrary `String` written after
   `-- block:`; a newline ends that comment. `ShadowSection.name` is also
   interpolated into comments and `import Shadow.<name>` without validation.
   Meanwhile `shadow_path_for_document` silently replaces invalid filename
   characters, allowing collisions and disagreement between import and
   filename. The worker's typed `BlockId` and restricted declaration names
   already close the equivalent request-composition hole. Apply consistent
   typed/validated identities to shadow generation and paths; reject invalid
   module names instead of silently rewriting them. Add newline, malformed
   module-name, and collision regressions.

3. **The stdout byte bound does not bound allocation.**
   `protocol.rs` uses `read_until(b'\n', &mut line)` before checking
   `max_stdout_bytes`. A child can emit an arbitrarily long unterminated line
   and allocate far beyond the configured maximum while the request waits.
   Read incrementally with the bound checked before extending buffers. Test
   oversized unterminated output, not just a large newline-terminated frame.

4. **Real extractor/admission tests need alignment and completion.**
   The inspected Python helper executes submitted source before extraction,
   while its trusted `rfl` and dependency-proof fixtures conflict with the
   current strict placeholder-only `parseHeader`. Separate trusted extractor
   inspection from model-source admission, and validate untrusted fixtures
   before execution. Keep direct/transitive `sorryAx`, clean trusted proofs,
   actual header boundaries, proposition/typeclass binder classification,
   function result versus definition body, and failed-branch repair cases.
   The runtime implementer is actively updating this suite. A completed real
   run is required before approval; startup-in-progress is not evidence.

## Initial checks and sound design

- `cargo test -p mls-lean` passed in this review: four unit tests and six
  subprocess tests. The one real-runtime test returned early because
  `MLS_LEAN_TESTS` was unset; its displayed pass is **not** runtime evidence.
- Generated declaration names are restricted before entering trusted
  commands, and request block IDs are typed UUIDs.
- Candidates contain worker identity, accepted-parent environment, and
  generation. Acceptance is explicit; reset invalidates candidates;
  errors do not advance the accepted environment.
- The worker requests trusted source validation before sending source for
  elaboration and requires exactly one matching extraction payload.
- Header counts come from parsed declaration syntax, not model metadata.
  Bounded telescopes preserve conclusion quantifiers and function result
  arrows; definition bodies and transitive axioms are separately extracted.
- Cross-section imports and dependency ordering are implemented, with
  missing dependencies and cycles rejected. A section-level cycle can be
  rejected even when the underlying block graph is acyclic; document this
  limitation or resolve it at the pipeline's section assignment boundary.
- The persistent interpreter launcher uses the exact pinned REPL source and
  built modules as a workaround for the host's native dyld failure. This is
  consistent with a persistent process; no per-declaration restart is needed.

## Scope limits

Admission intentionally supports a conservative mathematical syntax subset;
unsupported notation produces an explicit validation error. Extensions must
be individually reviewed. Cross-section cycles are rejected, including
section-level cycles whose underlying block graph might be acyclic. The
pipeline must keep that limitation visible. SIR structure comparison,
semantic faithfulness, user confirmation, and M5 proof admission remain
outside this task's approval.
