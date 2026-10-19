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

## M2 — Batch statements (not started)
- [ ] Research actual provider model availability, Lean REPL and pinned Mathlib.
- [ ] Implement SIR schemas, validation, semantic hashing and diffs.
- [ ] Implement configurable OpenRouter clients, typed decisions, logs, replay.
- [ ] Implement shadow projects, persistent Lean worker, error mapping and extraction.
- [ ] Implement statement generation, bounded repair and structural hard checks.
- [ ] Add first-year analysis/linear algebra evaluation corpus and metrics history.
- [ ] Pass offline, real Lean/Mathlib integration and opt-in live smoke tests.
- [ ] Resolve independent review; document and commit tested tasks.

## M3 — TeXmacs review (not started)
- [ ] Implement batch protocol and thin Scheme plugin with persistent IDs.
- [ ] Implement review cards (confirm/reject/edit), markers and document badge.
- [ ] Verify human-confirmation invariants and real TeXmacs interaction.
- [ ] Pass relevant tests, resolve independent review, document and commit.

## M4 — Live mode (not started)
- [ ] Implement daemon, document diffing, narrow Jev decisions and active-block gates.
- [ ] Implement scheduler, transitive invalidation and SIR/dependency cache keys.
- [ ] Implement incremental Lean checks and Mathlib premise retrieval.
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
