# MLS — Math Language Server

A language server for mathematical writing. MLS watches a TeXmacs document as you write, keeps a shadow Lean 4 project in sync with it, and reports back per block whether what you wrote is formalized, confirmed and verified.

The engine is editor-agnostic; the first and only client in scope is a TeXmacs plugin.

---

## 0. Instructions for the implementing agent

This spec is meant to be implemented end to end by an autonomous coding agent. Read this section first and follow it throughout.

### 0.1 Autonomy
- Work autonomously. Do not stop to ask for confirmation on routine decisions; make the most reasonable choice, record it in `docs/decisions.md` (one short entry per decision: context, choice, why), and continue.
- Stop and ask only when a decision is irreversible or would contradict this spec.
- When something in this spec turns out to be wrong or impossible in practice, document the problem in `docs/decisions.md`, choose the closest workable alternative, and keep going.

### 0.2 Subagent workflow
- Act as an **orchestrator**. Keep your own context for the plan, the current milestone, and integration; delegate focused work to subagents.
- Delegate one well-scoped task per subagent (e.g. "implement the `.tm` parser module with tests", "write the Lean structure-extraction metaprogram", "investigate the TeXmacs plugin pipe protocol"). Give each subagent: the relevant spec sections, the exact files/modules it owns, the interfaces it must respect, and the tests that define done.
- Use separate subagents for **research/investigation** (reading TeXmacs, Lean REPL, OpenRouter/Jev docs) and for **review** (an independent subagent that reviews a finished module and its tests without having written it).
- Subagents report back a short summary: what was built, files touched, tests added and passing, open issues. Do not pull full file dumps back into your own context.

### 0.3 Step-by-step implementation
- Implement strictly milestone by milestone (§10). Do not start a milestone until the previous one meets its definition of done.
- Within a milestone, break the work into small tasks, track them in `docs/progress.md` (checklist per milestone), and tick them off as they land.
- Each task ends in a working, tested state. Commit after each completed task with a clear message.

### 0.4 Testing (deep, at every layer)
Nothing counts as done without tests. Required layers:
- **Unit tests** for every module (parser, block store, hashing, dependency graph, scheduler, SIR schema validation, Lean error mapping, status logic).
- **Property-based tests** where invariants exist: block IDs stable under edits and moves; cosmetic edits never change cache keys; dependency invalidation always propagates downstream; generated Lean files are always in dependency order.
- **Golden-file tests** with a fixture corpus of real `.tm` documents (`tests/fixtures/tm/`): parsed block trees, generated SIRs, generated Lean files compared against checked-in expected outputs.
- **Model calls mocked by default.** All Sonnet and Jev calls go through a client trait with a record/replay implementation; tests run offline against recorded responses (`tests/fixtures/recordings/`).
- **Live smoke tests** against OpenRouter, gated behind an env var (e.g. `MLS_LIVE=1`), never run by default.
- **Lean integration tests** that run the real Lean REPL with Mathlib on small fixture projects: statements elaborate, `sorry` handling, structure extraction, error → block mapping.
- **End-to-end tests**: `.tm` in → statuses out, through the full pipeline with recorded model responses.
- **Evaluation set for M2**: a corpus of first-year analysis / linear algebra statements with hand-checked expected SIRs, and a script that reports formalization success rate, structure-check pass rate, and cost. Track results over time in `docs/evals.md`.
- CI-equivalent command (`make test` or `cargo test` + scripts) must pass before any commit.

### 0.5 Clean and tidy project structure
- Keep the repository organized, small modules with one responsibility each, and no dead code, stray scripts, commented-out blocks, or temp files left behind.
- Formatting and linting enforced (`cargo fmt`, `cargo clippy -D warnings`, and equivalents for Scheme and Lean code).
- Public interfaces documented; every module has a short header comment stating its purpose.
- Target layout (adjust if justified, and record the change in `docs/decisions.md`):

```
mls/
  Cargo.toml                 workspace
  crates/
    mls-core/                block model, hashing, dependency graph, statuses
    mls-tm/                  TeXmacs .tm parser and writer
    mls-sir/                 SIR schema, validation, diffing
    mls-models/              OpenRouter client, Sonnet + Jev wrappers, record/replay
    mls-lean/                Lean REPL worker, shadow project generator, structure extraction, error mapping
    mls-engine/              scheduler, pipeline, protocol server (daemon)
    mls-cli/                 CLI for M1/M2 batch runs and evals
  lean/
    MlsExtract/              Lean metaprogram for statement structure extraction
  texmacs-plugin/            Scheme plugin (thin client)
  tests/
    fixtures/tm/
    fixtures/recordings/
    fixtures/lean/
    e2e/
  evals/                     M2 evaluation corpus and scripts
  docs/
    spec.md                  this document
    decisions.md
    progress.md
    evals.md
    protocol.md              engine ↔ client protocol reference
  Makefile
  README.md
```

### 0.6 Definition of done (per milestone)
- All tasks in `docs/progress.md` for the milestone are ticked.
- All test layers relevant to the milestone exist and pass.
- An independent review subagent has reviewed the milestone's code and its findings are resolved.
- `README.md` explains how to build, run and test what exists so far.
- `docs/decisions.md` is up to date.

---

## 1. Principles

1. **Deterministic first, models only for fuzzy decisions.** Structure comes from the TeXmacs tree. Truth comes from Lean. Models fill only the gaps between the two.
2. **Statements and proofs are different problems.** Statements (definitions, theorem statements) must be *faithful* to the text, and a human confirms that. Proofs only need to *compile*.
3. **Faithfulness goes through an intermediate representation (SIR).** Prose → SIR → Lean. The human confirms the SIR; a mechanical check confirms the Lean matches the SIR.
4. **Everything is incremental and cached by content hash.** Nothing is re-formalized unless its meaning, or the meaning of something it depends on, changed.
5. **Statuses are honest.** "The model couldn't prove it" is never shown as "your maths is wrong."

---

## 2. Models and provider

All model calls go through **OpenRouter** (OpenAI-compatible API, `https://openrouter.ai/api/v1`, one API key).

| Role | Model | OpenRouter ID | Used for |
|---|---|---|---|
| Generation | Claude Sonnet 5 | `anthropic/claude-sonnet-5` | Writing SIRs, writing Lean statements and proofs, repair loops |
| Decisions | Jev (TypeSafe) | `typesafe/jev-1.14` (pin a version; `~typesafe/jev-latest` tracks newest) | Classification, gating, linking, field-level SIR checks |

Notes:
- Pin Jev to a specific version in config so decision behaviour doesn't shift under you; log the version with every decision.
- Jev 1.13's listing has a 32k context. Send Jev only the block plus minimal local context, never the whole document.
- Jev returns probabilities. Every Jev decision has a confidence threshold; below it, escalate (to Sonnet or to a deterministic fallback) rather than act.
- Claude Sonnet 5.5 (`anthropic/claude-sonnet-5.5`) is listed at the same price and is faster; worth an A/B once the pipeline works. Keep the model ID in config, not in code.

---

## 3. Architecture

```
TeXmacs (Scheme plugin, thin client)
   │  document tree / changed subtrees      ▲ statuses, markers, review cards
   ▼                                        │
MLS engine (Rust daemon, LSP-style JSON protocol over a pipe)
   ├─ Block store       IDs, hashes, kinds, SIRs, statuses
   ├─ Dependency graph  labels, refs, implicit references
   ├─ Scheduler         what to process next, and when
   ├─ Decision layer    Jev via OpenRouter
   ├─ Formalizer        Sonnet 5 via OpenRouter, structured output
   └─ Lean worker       Lean REPL with Mathlib loaded, generated shadow project
```

### 3.1 TeXmacs plugin (thin)
- Written in Scheme (Guile), talks to the engine over a pipe.
- Sends the document tree, or changed subtrees, on edit.
- Writes a hidden stable **block ID** onto every block in the `.tm` file, so identity survives edits, moves, and reopening.
- Renders only what the engine sends back:
  - a document-level **status badge**
  - a small **marker** per block (status colour, error tooltip)
  - **review cards** for SIR confirmation (confirm / reject / edit)

### 3.2 Engine
- Rust, editor-agnostic: nothing in the engine depends on TeXmacs specifics beyond the `mls-tm` parser.
- Protocol modeled on LSP: `didOpen`, `didChange`, `blockStatus`, `reviewRequest`, `reviewResponse`, `diagnostics`.
- Owns all state. The plugin is replaceable.

### 3.3 Lean worker
- Long-running Lean REPL process with Mathlib imported once, so each check costs seconds, not minutes.
- Generated shadow Lake project with pinned Mathlib version.
- One generated `.lean` file per document section, declarations in dependency order.
- Every declaration carries a `-- block:<id>` comment for mapping errors back.
- Fully generated from the block store. Never hand-edited.

---

## 4. Data model

```
Block {
  id            stable, stored in the .tm file
  text_hash     hash of the block's source
  kind          definition | theorem | lemma | proposition | corollary
                | proof | equation | prose_claim | commentary
  label         e.g. thm:cont-bounded
  deps          [block id]
  sir           SIR | null
  sir_hash      hash of the confirmed SIR
  lean          { statement, proof, built_from: cache_key } | null
  status        see §7
}
```

**Cache key** for a block's Lean = `sir_hash` + the `sir_hash`es of its dependencies' confirmed SIRs.
- Rewording that doesn't change the SIR → nothing re-runs.
- Changing a definition → everything downstream re-runs.

---

## 5. SIR — Statement Intermediate Representation

The SIR is the first-class `sir` field in Sonnet's single structured formalization result. The same result contains the Lean statement and notes, so every initial attempt and repair proposes a coherent SIR/Lean pair. The SIR remains deliberately small: it captures statement shape, while mathematical content stays semi-formal text.

```json
{
  "kind": "theorem",
  "label": "thm:cont-bounded",
  "variables": [
    {"name": "f", "type": "function ℝ → ℝ"},
    {"name": "a", "type": "real"},
    {"name": "b", "type": "real"}
  ],
  "hypotheses": ["a ≤ b", "f is continuous on [a, b]"],
  "conclusion": "∃ M, ∀ x ∈ [a, b], |f x| ≤ M",
  "uses": ["def:continuous"],
  "assumptions_made": ["interval is closed (text says 'the interval')"]
}
```

- `assumptions_made` is mandatory and surfaces silent choices for human review.
- The SIR is independently deserialized and identity-checked before its paired Lean declaration is checked against it.
- Generated outputs remain drafts until a human confirms the SIR. A confirmed or edited SIR stays pinned while the source text is unchanged.
- A changed source that regenerates the same semantic SIR is a cosmetic reconfirmation. Keep the exact confirmed SIR and its compatible accepted Lean artifact. A changed SIR supersedes confirmation and returns to draft review.
- An exact rejected SIR remains rejected. Repairs receive the prior combined output and exact diagnostics, then return a corrected SIR and Lean statement together.

### What the SIR buys
1. A concise, reviewable account of what the block claims.
2. Deterministic semantic diffing: unchanged SIR ⇒ cosmetic edit.
3. A mechanical Lean ↔ SIR structure check (see §8).
4. A stable key for accepted Lean artifacts: SIR hash plus dependency SIR hashes.

---

## 6. What Jev decides

Jev is only used for narrow, typed, high-frequency decisions. Never for holistic "is this formalization faithful?" judgements.

| Decision | Output type | Fallback if low confidence |
|---|---|---|
| Kind of an untagged prose block | choice: claim / proof step / commentary | treat as commentary (do nothing) |
| Is this block finished? | yes/no | wait for debounce |
| Is this edit semantic or cosmetic? (before SIR exists) | yes/no | treat as semantic |
| Which earlier labels does this block rely on? | choice from list of labels | ask Sonnet during SIR generation |
| Field-level SIR checks vs text: universal or existential? stated or implied? open or closed? | choice | flag the field on the review card |
| Does this SIR obviously differ from the text? | yes/no + probability | highlight review card |

Jev's flags only decide **what to highlight** for the human. Confirmation is always the human's.

---

## 7. Statuses

| | Status | Meaning |
|---|---|---|
| ⚪ | not formalized | no SIR yet |
| 🟡 | SIR drafted | waiting for your confirmation |
| 🔵 | statement confirmed | Lean statement elaborates; proof is `sorry` |
| 🟢 | verified | statement confirmed and proof compiles |
| 🟠 | unproven | proof attempts failed — *not* evidence the maths is wrong |
| 🔴 | refuted | engine proved the negation or found a counterexample |
| ⚠️ | formalization error | statement could not be made to elaborate |

Document badge = summary counts of the above.

---

## 8. Pipeline per edit

1. **Parse and diff.** Deterministic. Find changed blocks by `text_hash`.
2. **Classify.** Tagged environments come from the tree. Jev classifies only untagged prose.
3. **Gate.** Skip until the block looks finished (Jev + cursor left the block + debounce).
4. **Link and schedule.** Resolve references, then run ready blocks in dependency order.
5. **Generate one formalization result.** Sonnet receives the source, accepted dependency SIRs and declarations, plus applicable review context. It returns exactly `{sir, lean_statement, lean_proof, notes}`. M2/M4 require `lean_proof: null`; `notes` is an array of strings. Expected/reference SIRs are never included in the prompt.
6. **Validate and check the pair.** Strictly deserialize and identity-check the returned SIR. Elaborate its paired Lean declaration and compare extracted binders, hypotheses and goal structure against that same SIR.
7. **Repair jointly.** On identity, Lean elaboration/name-resolution, or structure mismatch, retry within `max_attempts`. Each repair receives the complete previous combined output and exact errors and must return both a corrected SIR and Lean statement. Imported Mathlib is available; actual diagnostics and name resolution can guide repairs. There is no automatic `#loogle` or `#leansearch` step.
8. **Review after Lean checking.** Successful generated results are drafts. Human review confirms, rejects or edits the SIR. Confirmed SIRs stay pinned while text is unchanged. On a text edit, a matching generated SIR retains the exact confirmed SIR and compatible accepted Lean artifact; a changed SIR supersedes confirmation. An exact rejected SIR remains rejected.
9. **Map back.** Formalization and proof diagnostics map through `block:<id>` to
   the marker on the corresponding block.
10. **Prove confirmed statements (M5).** Only an exactly confirmed statement
    enters bounded proof generation and repair. Mark it `verified` only after a
    complete Lean proof passes without `sorryAx` or disallowed axioms; exhausted
    attempts remain `unproven`. Use `refuted` only when Lean checks positive
    evidence for a negation or counterexample. M2/M4 formalization calls keep
    `lean_proof: null`.

### Sonnet structured output

```json
{ "sir": { "kind": "theorem", "label": null, "variables": [], "hypotheses": [], "conclusion": "…", "uses": [], "assumptions_made": [] }, "lean_statement": "theorem … := by sorry", "lean_proof": null, "notes": [] }
```

## 9. Shadow Lean project layout

```
.mls/
  lakefile.lean          pinned Mathlib
  lean-toolchain
  Shadow/
    Section01.lean       generated, dependency-ordered
    Section02.lean
  cache/                 cache_key → lean artifacts
  log/                   every Jev and Sonnet call (model version, inputs, outputs, probabilities, latency, cost)
```

Lives next to the `.tm` file. Safe to delete; it's rebuilt from the block store.

---

## 10. Build order

**M1 — Parser CLI (no AI).**
Read a `.tm` file, assign block IDs, print blocks with kinds, labels and dependencies.

**M2 — Batch statements.**
CLI: for every definition/theorem, one Sonnet structured-output call returns SIR and Lean statement together; Lean elaborates that statement, checks its structure against that returned SIR, and bounded repairs return a corrected pair. No proofs, no TeXmacs.
*This is the go/no-go milestone: is statement autoformalization good enough on real course notes (first-year analysis, linear algebra)?*

**M3 — TeXmacs review.**
Plugin with block IDs, review cards, confirm/reject, status badge. Still batch-triggered.

**M4 — Live mode.**
Daemon, diffing, Jev gates, scheduler, SIR-hash cache, incremental Lean checks.

**M5 — Proofs.**
Proof SIRs, proof attempts, repair loop, unproven/refuted split, error markers.

---

## 11. Risks

- **Autoformalization quality** on statements is the core risk. M2 exists to measure it early.
- **Compounding errors** (prose → SIR → Lean). Mitigated by human confirmation at the SIR and the mechanical SIR ↔ Lean structure check.
- **Schema creep.** Keep SIR to statement shape. If it starts needing its own logic, stop.
- **Mathlib coverage.** Pick domains Mathlib covers well for early testing.
- **TeXmacs plugin internals** (Scheme, tree observers) are the least documented part. M1–M2 avoid them entirely.
- **Jev drift.** Pin the version; log every decision with its version and probabilities.
- **Cost and latency.** Jev runs constantly and cheaply; Sonnet runs once per formalization attempt after gating; Lean checks and repairs stay local apart from the model repair call.
