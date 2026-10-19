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
   ├─ Retriever         embeddings over Mathlib names/docstrings (premise retrieval only)
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

Structured description of what a block *claims*, written by Sonnet before any Lean exists. Deliberately small: it captures the **shape** of a statement, and the mathematical content inside fields stays semi-formal text. Do not grow it into a logic language — that's Lean's job.

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

- `assumptions_made` is mandatory. It forces the model to surface silent choices (types, domains, open vs closed), which is exactly what the human reviews.
- **Proofs** get a lighter SIR: a list of steps and the lemmas/blocks each step uses. Used to guide proof attempts. Never needs confirmation.

### What the SIR buys
1. Splits "understand the claim" from "write Lean with Mathlib names."
2. A review surface humans can read in seconds.
3. Bounded, typed questions for Jev (see §6).
4. Deterministic semantic diffing: unchanged SIR ⇒ cosmetic edit.
5. A mechanical Lean ↔ SIR structure check (see §8, step 9).

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
2. **Classify.** Tagged environments (`theorem`, `definition`, `proof`, …) come from the tree. Jev classifies only untagged prose.
3. **Gate.** Skip until the block looks finished (Jev + cursor left the block + debounce). Never process the block being actively typed.
4. **Link.** Explicit `\ref`/labels deterministically; implicit references ("by the previous lemma") via Jev choosing among earlier labels.
5. **Schedule.** Priority queue: block just left first, then its dependents.
6. **Write SIR.** Sonnet 5, structured output, given the block text + confirmed SIRs of dependencies. If the new SIR equals the old one → cosmetic edit, stop.
7. **Check SIR.** Jev field-level checks against the text. Results annotate the review card.
8. **Confirm SIR.** Review card in TeXmacs. User confirms, rejects, or edits. Confirmed SIRs are pinned until the block's meaning changes.
9. **Write and check Lean statement.** Sonnet 5 writes the Lean statement from the confirmed SIR + retrieved Mathlib premises. Lean elaborates it with `sorry`. Then a small **Lean metaprogram extracts the statement's binders, hypotheses and goal**, and the engine compares counts/structure against the SIR. Mismatch = hard error, no model involved. Errors feed a repair loop (max N attempts).
10. **Prove.** For confirmed statements only. Sonnet writes a proof guided by the proof SIR; Lean errors feed a repair loop. Failure after N attempts → 🟠 unproven. Optionally try to prove the negation / find a counterexample → 🔴 refuted.
11. **Map back.** Lean diagnostics → `block:<id>` → marker on the right block.

### Sonnet structured outputs

SIR call:
```
{ kind, label, variables[], hypotheses[], conclusion, uses[], assumptions_made[] }
```
Lean call:
```
{ lean_statement, lean_proof | null, mathlib_names_used[], notes }
```

---

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
CLI: for every definition/theorem, Sonnet writes SIR → Lean statement → Lean elaborates with `sorry` → structure check → report. No proofs, no TeXmacs.
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
- **Cost and latency.** Jev runs constantly and cheaply; Sonnet only after gating and only on SIR changes; Lean is local.