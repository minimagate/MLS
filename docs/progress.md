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

## M2 — Batch statements (active)
- [x] Research actual provider model availability, Lean REPL and pinned Mathlib.
- [x] Implement SIR schemas, validation, semantic hashing and diffs.
- [ ] Implement configurable OpenRouter clients, typed decisions, logs, replay.
- [ ] Implement shadow projects, persistent Lean worker, error mapping and extraction.
- [ ] Implement statement generation, bounded repair and structural hard checks.
- [ ] Add first-year analysis/linear algebra evaluation corpus and metrics history.
- [ ] Pass offline, real Lean/Mathlib integration and opt-in live smoke tests.
- [ ] Resolve independent review; document and commit tested tasks.

Current focused tasks (started after M1 commit `6a10a97`):
- `research_m2_models`: verify OpenRouter catalog, structured output and decision
  probability API against primary sources; no paid calls during investigation.
- `research_m2_lean`: verify a compatible pinned Lean/Mathlib/REPL setup and
  deterministic extraction interface; local installation follows the research.
- `implement_m2_sir` (GPT-5.6 Sol): schema, mandatory surfaced assumptions,
  validation, semantic hashes/diffs, and offline tests in `mls-sir` only.
The SIR task has independent approval (13 tests, plus collision probes). Provider
and Lean investigation reports are in `docs/research/`. `implement_m2_models`
owns the OpenRouter adapters and replay layer; `implement_m2_lean_runtime` owns
the pinned local runtime and compiled syntax-aware extractor. Both use GPT-5.6
Sol. The parent retains structure-check decisions, review and integration.
The batch pipeline and M3–M5 have not started.

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
