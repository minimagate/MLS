# M2 evaluation history

The M2 evaluation uses ten manually reviewed, specialized first-year analysis
and linear-algebra statements in `evals/`. It is a small curated regression
corpus, not a representative course benchmark. Results must keep model/replay
provenance and denominators visible.

## Metric interpretation

- SIR reference agreement is exact equality of canonical JSON strings. It is
  reproducible, but it does not establish mathematical correctness or mark a
  mathematically equivalent rewording as incorrect.
- Elaboration means Lean accepted the generated statement with the pipeline's
  admitted body.
- Each structure check has a scope. `structure` checks (counts, binder names,
  roles and binder modes, outer logical head, definition body presence, theorem
  admission) are mechanical. `semantics` checks (variable types, hypothesis and
  conclusion content, definition body content) are always unchecked in M2.
- Complete structure means the candidate was accepted and every structure-scoped
  check was implemented and matched; it says nothing about semantic content.
  Partial means accepted while some structure-scoped check (typically an outer
  head that could not be recognized conservatively from the SIR prose) remained
  unchecked. Hard mismatches are reported separately.
- `accepted_speculatively` is an unconfirmed draft state, not human confirmation
  or verification.
- Known provider cost is shown alongside the number of calls with unknown cost.
  Reported token totals have separate counters for calls with missing input or
  output usage. Latency remains unavailable until included in `BatchReport`.

## Run history

Immutable machine-readable run records are produced by `python3 evals/run.py`
under the ignored `evals/results/` directory; call records stay under the
ignored document-adjacent `evals/.mls/log/`. Results are not committed.

### Run 1 — 2026-10-19, live, `--max-attempts 2`

Run `20261019T221020Z-81634e864c`; generation `anthropic/claude-sonnet-5`
(resolved identically); corpus `cases_sha256` `e8e3bd91…02d93a`. The offline
replay of the 28 captured calls (`20261019T221526Z-20b7043c66`) reproduced
every block report and the usage summary exactly.

| Metric | Result |
|---|---|
| SIR drafted | 10/10 |
| Exact canonical SIR agreement | 0/10 |
| Lean statement elaborated | 6/10 |
| Accepted speculatively (unconfirmed) | 6/10 |
| Hard structural mismatch (of elaborated) | 0/6 |
| Complete / partial structure (of elaborated) | 0/6 / 6/6 |
| Provider calls / known cost | 28 / $0.267934 (no unknown cost or tokens) |
| Reported tokens | 21,492 input, 22,495 output |

Manual inspection (parent agent) of the generated SIRs and Lean statements:

- Five accepted drafts look faithful to the source text: `kernel-add`,
  `intermediate`, `square-nonnegative`, `linear-zero`, `monotone-lower`
  (ℝ² rendered as `Fin 2 → ℝ`, linearity as `IsLinearMap ℝ T`).
- `def:kernel` was accepted but defines the membership predicate `T v = 0`
  with `v` as a parameter, not the set-valued kernel the text defines.
- Three failures were our admission grammar, not Lean or the model: absolute
  value `|x|` (`triangle`, `convergent-bounded`) and the binder predicate
  `∃ c ∈ S` (`maximum`).
- `def:bounded-on` exhausted the 4,096-token limit: 3,988 reasoning tokens spent
  reconciling a SIR that listed the existentially bound `M` as a variable.
- SIR contract weaknesses: typing facts restated as hypotheses led the Lean
  generator to pad with vacuous `(h0 : True)` binders (`triangle`,
  `convergent-bounded`, `bounded-on`); bound variables listed as SIR variables
  (`maximum`, `bounded-on`); TeX or prose conclusions left every outer head
  unchecked, so no accepted draft had complete structure.

Exact SIR agreement 0/10 reflects wording differences from the references, not
a correctness judgement. Compilation was not equated with faithfulness.

### Run 2 — 2026-10-19, live, `--max-attempts 2`

Run `20261019T224344Z-4b36ccecbc`, after the iteration-2 changes recorded in
`docs/decisions.md` (SIR/Lean contract, vacuous-hypothesis check, admission
notations). Same corpus and models.

| Metric | Result |
|---|---|
| SIR drafted | 10/10 |
| Exact canonical SIR agreement | 0/10 |
| Lean statement elaborated | 8/10 |
| Accepted speculatively (unconfirmed) | 8/10 |
| Hard structural mismatch (of elaborated) | 0/8 |
| Complete / partial structure (of elaborated) | 5/8 / 3/8 |
| Provider calls / known cost | 25 / $0.179008 (no unknown cost or tokens) |
| Reported tokens | 24,034 input, 13,094 output |

Manual inspection: all eight accepted drafts look faithful, including the three
statements that run 1 lost to admission (`maximum`, `convergent-bounded`) or the
token limit (`bounded-on`, now a `Prop`-valued definition with an existential
body). No vacuous hypotheses or shadowed bound variables appeared; SIR
assumptions now record the interpretive choices (closed interval, ℕ including
0, ε–N convergence). Monotonicity was written out as `∀ n m, n ≤ m → u n ≤ u m`
rather than `Monotone u`; both are faithful.
Both failures (`triangle`, `kernel`) prepended `set_option autoImplicit false`.
Admission correctly rejected the extra command, and the raw parser message did
not lead the model to remove it in its repair attempt. The kernel draft itself
was the set `{v | T v = 0}` over a linear map, as the text defines.
Partial structure: conclusions with `|·|` or `≥` have no conservatively
recognized outer head.

### Run 3 — 2026-10-19, live, `--max-attempts 2`

Run `20261019T224931Z-8fbafe30fb`, after the single-declaration Lean contract,
admission repair hints and quantifier-prefix `|·|` recognition. The offline
replay of its 23 captured calls (`20261019T225157Z-088cd77f3d`) reproduced every
block report and the usage summary exactly.

| Metric | Result |
|---|---|
| SIR drafted | 10/10 |
| Exact canonical SIR agreement | 0/10 |
| Lean statement elaborated | 10/10 |
| Accepted speculatively (unconfirmed) | 10/10 |
| Hard structural mismatch (of elaborated) | 0/10 |
| Complete / partial structure (of elaborated) | 7/10 / 3/10 |
| Provider calls / known cost | 23 / $0.163104 (no unknown cost or tokens) |
| Reported tokens | 23,907 input, 11,529 output |

Manual inspection: all ten Lean statements are faithful formalizations of the
source statements. `triangle` and the set-valued `kernel` definition now pass
(the latter after one repair). Partial structure remains for `triangle` and
`square-nonnegative` (`≤`/`≥` conclusions have no recognized outer head) and
`kernel` (an equality-headed defining expression is deliberately unchecked).
Limitation: ℝ² was rendered as `ℝ × ℝ` in three statements and as
`EuclideanSpace ℝ (Fin 2)` in `linear-zero`; independently formalized
statements need a shared representation once they depend on one another.

## M2 go/no-go (2026-10-20)

**Go, with stated limits.** On this curated ten-statement corpus the final
pipeline produced faithful, elaborating Lean statements for all ten, with
human-reviewable SIRs that surface interpretive choices, at about $0.016 per
statement. The deterministic structure check never produced a hard mismatch on
a faithful draft in the final run and is conservative by construction (unknown
forms are unchecked, never matched). Limits that carry into later milestones:
the corpus is small and specialized; exact SIR agreement with references stays
0/10 because wording varies, so SIR faithfulness relies on the human review in
M3; representation consistency across blocks is not yet enforced; three
iterations of prompt and admission changes were tuned on this same corpus, so
these results are optimistic for unseen notes.
