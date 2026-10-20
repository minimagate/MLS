# MLS implementation progress

The authoritative requirements are in [spec.md](spec.md). Milestones are sequential;
unchecked items are not complete. The parent agent owns design and acceptance;
focused implementation, investigation, and independent review use separate agents.

## M1 — Parser CLI (complete)
- [x] Inspect workspace, preserve specification, check available tooling.
- [x] Investigate native TeXmacs syntax and persistent block metadata.
- [x] Implement workspace, block types, source hashing, explicit dependencies.
- [x] Implement lossless TeXmacs parser and stable ID persistence.
- [x] Implement parser CLI with explicit file-writing option.
- [x] Add unit, property, golden corpus, and CLI integration tests.
- [x] Run formatting, lint, full offline test command; resolve independent review.
- [x] Document build/run/test and decisions; commit tested tasks.

## M2 — Batch statements (complete)
- [x] Research actual provider model availability, Lean REPL and pinned Mathlib.
- [x] Implement SIR schemas, validation, semantic hashing and diffs.
- [x] Implement configurable OpenRouter clients, typed decisions, logs, replay.
- [x] Implement pinned Lean runtime, trusted extraction and persistent worker.
- [x] Implement dependency-ordered shadow generation and error mapping.
- [x] Integrate document-local shadow projects with the batch pipeline.
- [x] Implement statement generation, bounded repair and structural hard checks.
- [x] Add first-year analysis/linear algebra evaluation corpus and metrics history.
- [x] Pass offline, real Lean/Mathlib integration and opt-in live smoke tests.
- [x] Resolve independent review; document and commit tested tasks.

The pipeline passed five independent review rounds (findings and resolutions in
`reviews/m2-pipeline.md`). Three live evaluation runs, each replayed offline
with identical reports, are recorded in `evals.md`, together with the M2
go/no-go assessment: go, with the stated limits on corpus size, SIR wording,
cross-block representation consistency and tuning on the same corpus.

## M3 — TeXmacs review (complete)
- [x] Research TeXmacs plugin loading, external commands, tree access, widgets
      and headless testing (`research/m3-texmacs.md`).
- [x] Implement review store, confirmed-SIR pinning, cosmetic reconfirmation,
      rejection feedback, §4 Lean cache and M3 statuses in the engine.
- [x] Implement the plugin-facing review commands and protocol (`protocol.md`).
- [x] Implement thin Scheme plugin with persistent IDs, review cards
      (confirm/reject/edit), markers (status panel) and document badge.
- [x] Derive block identity from the TeXmacs tree so layout-only saves keep
      confirmations (found by the real end-to-end test).
- [x] Verify human-confirmation invariants and real TeXmacs interaction
      (headless; GUI-only rendering listed in `validation.md`).
- [x] Pass relevant tests, resolve independent review, document and commit.

## M4 — Live mode (active)
- [ ] Research TeXmacs edit/cursor hooks, timers and persistent connections;
      research Mathlib premise retrieval (`research/m4-*.md`).
- [x] Implement live document state, diffing, transitive invalidation,
      SIR/Lean caches, gates and scheduler with stale-result discarding.
- [x] Implement narrow Jev decisions with thresholds and fallbacks.
- [x] Implement incremental Lean checks (environment snapshots).
- [ ] Implement Mathlib premise retrieval for Lean generation.
- [ ] Implement the daemon protocol and live plugin client.
- [ ] Verify cosmetic edits, moves, reopen, dependency edits and stale responses.
- [ ] Pass relevant tests, resolve independent review, document and commit.

## M5 — Proofs (not started)
- [ ] Implement proof SIRs and bounded proof/repair attempts for confirmed statements.
- [ ] Implement honest statuses, diagnostics/markers and evidence-gated refutation.
- [ ] Verify complete .tm → statuses pipeline using replay and real Lean.
- [ ] Pass relevant tests, resolve independent review, document and commit.

## Final acceptance
- [ ] Audit each specification requirement against files and executed evidence.
- [ ] Verify fresh documented setup, complete prototype and clean tracked workspace.
