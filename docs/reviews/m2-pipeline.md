# M2 pipeline independent review

2026-10-19. First pass: **changes required; not approved**. Reviewed the uncommitted engine/CLI implementation, the SIR/model/Lean interfaces, and the M2 portions of the specification. No paid calls were made. Test implementation was still in progress, so this is not a final milestone audit.

## Blocking findings

1. **The CLI can overwrite its source document.** `crates/mls-cli/src/main.rs`, `batch`, writes `--output` with unrestricted `fs::write`. An independent disposable-directory probe with `--output` equal to the input `.tm` returned exit 0 and replaced the TeXmacs source with report JSON. Reject source aliases (including symlinks/hardlinks) before side effects. Regression must verify source bytes survive.

2. **Ordinary relative input paths fail.** `initialize_project` returns relative `.mls`, while `scripts/run-lean-repl.sh` requires an absolute project path. Independently reproduced `mls batch notes.tm --replay <absolute fixture directory>` in a disposable directory: exit 1, `MLS Lean project path must be absolute: .mls`, after project creation. Resolve paths consistently before initialization/spawn.

3. **Project initialization has no ownership or alias protection.** `initialize_project` unconditionally copies four template files and compiled extractor artifacts over any existing `.mls` files, follows directory/file symlinks, and attempts template-to-itself copies when the input lives at workspace root. `write_shadow` likewise overwrites section paths, and leaves old generated sections after accepted content is removed. Establish generated-file ownership, reject unexpected existing content/links, handle template identity explicitly, and update only owned artifacts. Verify repeated runs and failure paths, not merely first creation.

4. **Batch IDs and shadow IDs diverge on ordinary unannotated input.** `run_batch` analyzes the source, and `write_shadow` analyzes it again. `mls_tm::analyze` generates fresh UUIDs for missing IDs; the second analysis therefore cannot match any accepted report entry. The batch report can accept statements while writing no declarations. Persist IDs explicitly, require valid stored IDs before calls, or share one analysis throughout the batch. Duplicate/invalid identity diagnostics also need a deliberate preflight policy rather than generating unstable requests. This must be tested with actual unannotated input.

5. **Failed formalization is reported as a draft, and attempt evidence can be stale.** `run_batch` sets `SirDrafted` immediately after SIR decoding and never changes it after identity failure, exhausted elaboration failures, structural mismatch, or candidate-acceptance failure. In addition, `elaborated` and `checks` survive from an earlier elaborated-but-rejected candidate when a later `lean_statement` fails to elaborate. Preserve the SIR for review, but report formalization failure and ensure displayed candidate/evidence belong to the same attempt. Test a hard mismatch followed by a syntax error, and complete repair exhaustion.

6. **Outer-head checks impose unsound hard errors.** `recognized_head` searches arbitrary substrings: `{v ∈ ℝ² | T v = 0}` is classified as equality, and `x = 0 ∧ y = 0` as equality rather than conjunction; function arrows in nested/type text are treated as implications. Unsupported or ambiguous structure must remain unchecked. Further, definitions compare the SIR defining expression with `actual.goal_outer_head`, which describes the *result type*, instead of the extracted definition body: the checked-in boundedness definition has an existential SIR body but Lean result type `Prop`. A faithful definition will therefore fail. Compare supported definition body structure separately, conservatively; do not infer it from the result type.

7. **Provider failures disappear from aggregate accounting.** Usage is added only after `generate_sir`/`generate_lean` fully succeeds. `ModelError::Provider` already carries recoverable usage, but those costs/tokens are dropped. A schema-valid reply rejected by strict `Sir` deserialization also loses its successful-call metadata. Unknown token usage is summed as zero without an unknown indicator. Account at the model-call boundary, including recoverable failure evidence, and distinguish missing accounting from known zero. Test billable malformed responses and successful provider replies with invalid local SIR content.

8. **Lean generation lacks dependency declarations and their generated names.** SIR extraction receives dependency SIRs, but Lean generation receives only the current SIR and labels in `uses`; it never receives the accepted dependency's `mls_<UUID>` name, signature, or definition. The model cannot reliably reference a generated definition from a human label. Pass a minimal explicit mapping/context for accepted dependencies and test a theorem whose type actually uses a preceding generated definition, including across sections. A successful shared REPL environment alone does not prove generated section files compile independently.

9. **Real Lean diagnostics are discarded before repair.** The `StatementChecker for LeanWorker` adapter converts every worker error with `error.to_string()`. `WorkerError::Diagnostics` displays only `Lean rejected the request`, discarding the underlying unknown-name/type-error messages and locations. Repair prompts consequently lack the evidence they need. Preserve structured diagnostics through this boundary and map them to the source block; add a regression proving the next repair request contains the actual unknown-name/type diagnostic from real Lean.

## Additional correctness concerns

- `compare_structure` never checks `binder_info` even though the canonical generation contract requires explicit variables followed by explicit hypotheses. Add the simple mechanical check; arbitrary semi-formal type equivalence can correctly remain unchecked.
- `StatementChecker for LeanWorker` stores its pending candidate in one thread-local slot shared by all workers. Interleaved checking on two workers loses/steals the candidate (the worker does correctly reject a foreign candidate). Prefer a checker wrapper owning its pending candidate, or another per-worker design. This is an interface integrity issue rather than a false-verification exploit.
- Block reports carry block IDs but no original source span or structured mapped Lean diagnostics. M2 error reporting should retain a useful source mapping, and its tests should assert the failing block rather than only a global error.
- Fresh template Lake configuration declares `MlsExtract` but no `Shadow` library target. Real tests must compile generated section modules in dependency order, not merely check candidates in the persistent REPL, and establish the documented rebuild command.

## Positive observations and acceptance boundary

The M2 status type cannot claim confirmation, verification, or refutation. Successful structural matches still expose unchecked semi-formal semantics, and `complete_structure` stays false in their presence. Bounded repairs reuse the same SIR, and candidate environments are accepted only after hard checks. The CLI requires explicit replay or both live gates. These are appropriate boundaries to retain.

Approval requires fixes with meaningful checked-in record/replay `.tm → report` tests, accepted/mismatch/repair/dependency/error cases, generated Lean golden files, real Lean validation of the CLI output including dependency imports, and the CI-equivalent checks. The current first pass does not certify evaluation quality or M2 completion.

## Re-review evidence

The implementation now addresses the nine original findings: source/output aliases are rejected before side effects; paths are canonicalized; existing project content and generated files have ownership checks and atomic writes; one analysis supplies IDs throughout the batch; failed attempts reset candidate-specific evidence and report formalization errors; definitions compare their extracted body head; model accounting happens before local SIR decoding and retains provider failure usage; Lean generation receives accepted dependency names/declarations/SIRs; and real Lean diagnostics survive into repair prompts and block/source mapping. The pending candidate now belongs to a per-worker checker wrapper.

Re-review caught two integration errors that unit mocks had masked. The extractor's explicit binder mode is `explicit`, not `default`; the engine and test mocks now use the actual protocol value. Substring comparison also allowed `or` to match `forall`; head comparison now uses exact extractor enum values. Ambiguous function-type prose, unbalanced/quoted syntax, and suffix connective/quantifier prose remain unchecked instead of causing the reproduced false hard errors.

Independently executed after these fixes:

- Offline engine/CLI suites passed, including exact-request record/replay, bounded mismatch/transport failure, dependency blocking, report golden comparison, and source/output alias preservation.
- `MLS_LEAN_TESTS=1 cargo test -p mls-cli --test batch -- --nocapture`: three tests passed, including relative input, offline replay failure with a dummy API credential, and a nonzero-binder definition used by a theorem in a later section. The test builds the generated `Shadow` library with the pinned Lake project.
- `MLS_LEAN_TESTS=1 cargo test -p mls-engine --test pipeline real_worker_repair_feedback -- --nocapture`: passed. The repair request contained the actual `unknown identifier` diagnostic and the source block range matched the TeXmacs theorem bytes.
- A separate disposable-directory CLI replay succeeded; `lake build Shadow.Section2` and `lake env lean Shadow/Section2.lean` both passed. Repeating the batch after the build passed. Removing all formal statements and rerunning removed both previously owned generated `.lean` section files and reported zero blocks.

The initial CLI unit tests accidentally required the ignored local runtime template. The revised tests inject a tiny temporary template, preserving the offline default-test boundary. Empty precreated Shadow directories are safely adopted; nonempty unowned directories are rejected.

These checks concern pipeline correctness and safe draft reporting. Synthetic model recordings do not establish live model quality, reference-SIR faithfulness, or the M2 evaluation go/no-go decision. Arbitrary type, hypothesis, and conclusion semantics remain explicitly unchecked; a draft never becomes confirmed or verified in M2.

## Final review rounds (2026-10-20)

Five further independent rounds (separate reviewer agents, read-only) were run
after the re-review above. Resolved findings, in order:

1. Outer-head guesses: ASCII connectives, prose quantifiers, `¬`, TeX/TeXmacs
   spellings, curly quotes, prose-led sentences, relation chains in quantifier
   bodies, convergence arrows (`a_n → 0`), `lim`, unspaced `=` with glued prose,
   and `∃!`/counting quantifiers. The recognizer is now an allowlist and leaves
   anything uncertain unchecked; definitions never use an equality head.
2. `complete_structure` was unreachable; checks now carry a `structure` or
   `semantics` scope.
3. Lost paid work: missing output directory, stale shadow manifest, unowned
   shadow collisions, ownership refused only after calls, non-UTF-8 blocks
   aborting the batch, recording I/O failures discarding paid replies,
   truncated call records, `run.py` deleting a report written before a nonzero
   exit, and a report printed only to stdout. All are preflighted or preserved.
4. File safety: repository-root documents sharing the runtime template,
   outputs inside `.mls` (including via a symlink and `..`), template refresh
   for owned projects.
5. Admission: extended with pure notations; `deriving` and proof-escape
   identifiers blocked; `group` confined to two shapes.
6. Evaluator: absolute paths, corpus-mapped numerators, no per-case reference
   hashes.

Gated real-Lean tests run only under `make test-lean`; this is the documented
acceptance gate. Remaining limitations (literal-only vacuous check, small
curated corpus, cross-block representation consistency) are recorded in
`docs/decisions.md` and `docs/evals.md`.
