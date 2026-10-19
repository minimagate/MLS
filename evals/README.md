# M2 statement evaluation corpus

`cases.json` contains ten small first-year analysis and linear-algebra statements,
with manually reviewed reference SIRs. `statements.tm` contains their native
TeXmacs counterparts with fixed statement IDs. These are curated paraphrases and
specializations, not verbatim course-note extracts or a representative benchmark.
The model must receive only source blocks and ordinary dependency context, never
`expected_sir`.

Mathematical references, accessed 2026-10-19:

- [Lebl, Basic Analysis I, §1.3](https://www.jirka.org/ra/html/sec_absval.html):
  absolute bounds and the triangle inequality.
- [Lebl, §3.3](https://www.jirka.org/ra/html/sec_minmaxint.html):
  maximum attainment and the intermediate value theorem.
- [Lebl, §2.1](https://www.jirka.org/ra/html/sec_seqsandlims.html):
  boundedness of convergent sequences and lower bounds for increasing sequences.
- [Scholbach, Linear Algebra, §4](https://jakobscholbach.github.io/lectures/linear-algebra/maps/):
  linear maps, their kernels, and preservation of zero and addition.

The square example is an original elementary check. The source text explicitly
states specializations to real sequences, zero-based indexing, real functions
defined on all of ℝ, and real two-dimensional vector spaces. Those choices are
not hidden assumptions. Intervals are closed, maximum attainment requires
`a ≤ b`, and intermediate values use the increasing endpoint orientation only.
Definitions have mathematical bodies; an admitted result type alone cannot
validate them. The kernel definition is set-valued, not a theorem that a kernel
is a subspace.

Reference SIRs are review oracles, not claims that a different wording is wrong.
Exact semantic-hash agreement may be reported as a reproducible string-level
metric but must not be called mathematical equivalence. Independent elaboration,
supported structural checks, unchecked fields, failures and provider cost must
be reported separately. Model recordings used for offline pipeline tests must
identify whether they are synthetic or captured from live calls.

Corpus parsing currently produces ten statement blocks, three section blocks,
and no diagnostics. There is no success-rate claim until a real replay or live
run is recorded.

Run the evaluator against an existing recording directory after building the
CLI:

```sh
cargo build -p mls-cli
python3 evals/run.py --replay PATH/TO/RECORDINGS
```

Replay is mandatory by default. A paid live run requires both `--live` and the
explicit `MLS_LIVE=1` environment gate. The runner passes only `statements.tm`
to `mls batch`; it never passes `cases.json` or an expected SIR to the model.

Each invocation preserves the CLI's unmodified `BatchReport` and writes a
separate, uniquely named summary under `evals/results/` (or
`--output-directory`). Files are opened exclusively and are never silently
overwritten. If `mls batch` exits nonzero after writing a complete report (for
example, an incomplete live call recording or a shadow-project write failure),
`run.py` still saves the raw report and summary with `cli_exit_code` set and
`complete: false`, then exits nonzero. The summaries include corpus/label integrity, SIR drafting,
canonical JSON equality with reference SIRs, elaboration, hard mismatch,
partial and complete structural checking, failures and blocked dependencies,
unconfirmed speculative acceptance, and available usage/cost fields. The
canonical metric is string-level after deterministic JSON serialization. A
different wording can be mathematically correct, so it is not a mathematical
correctness score. Latency is marked unavailable because the current batch
report does not expose it. Reported token totals and separate counts of calls
with missing input/output-token usage are retained; missing counters in older
reports remain unknown.

Run the stdlib-only tests without making model calls:

```sh
python3 -m unittest evals/test_runner.py
```
