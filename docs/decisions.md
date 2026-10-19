# Decisions

## 2026-10-19 — Sequential delivery
Context: the repository contains only a README and the brief requires milestone gates.
Choice: implement M1 first; retain M2–M5 as pending requirements, with no completion
claim until their tests and independent reviews pass. Why: parser identity and
source preservation are foundations for every subsequent milestone.

## 2026-10-19 — Orchestration and credentials
Context: no orchestrator skill or orchestrator-specific tool is installed in the
available catalog. Choice: use the built-in subagent workflow, with the parent
retaining judgment and separate investigation, implementation and review agents.
Why: this provides the workflow requested by the brief. The requested model name
“Luna 5.6” is not exposed; after asking for clarification, provisionally select
GPT-5.6 Sol (the available 5.6 implementer), subsequently explicitly confirmed
by the user. Research/review stay separate.
The local zsh configuration defines OPENROUTER_API_KEY, already available in the
environment. Runtime clients will read that variable; no credential is copied
into this repository or printed in logs.

## 2026-10-19 — Lossless native syntax and IDs
Context: TeXmacs native files contain mixed short/long tags, escaped characters,
raw data, paragraph-sensitive whitespace and sometimes non-UTF-8 Cork bytes.
Choice: parse source bytes with spans and insert ID wrappers without normalizing
unrelated content. Source hashing operates on original bytes. Why: whole-file
serialization or lossy UTF-8 conversion could corrupt mathematical documents.
Use the built-in local binding `<with|mls-id|UUID|BLOCK>` as persistent metadata,
with a long form for multiparagraph content; an unused variable should not alter
rendering. This last claim requires a real TeXmacs roundtrip before M1 acceptance.
Investigated sources: [native format](https://www.texmacs.org/tmweb/manual/webman-format.en.html),
[upstream parser](https://raw.githubusercontent.com/texmacs/texmacs/master/src/Data/Convert/Texmacs/fromtm.cpp),
[variable scope](https://www.texmacs.org/tmweb/manual/webman-styling.en.html), and
[linking primitives](https://www.texmacs.org/tmweb/manual/webman-primitives.en.html).

## 2026-10-19 — M1 boundaries and persistence policy
Context: block identity must survive edits without corrupting native structure.
Choice: emit rendered paragraphs and tagged blocks in source order, retaining
inline formatting and references in their owning paragraph. Recognize starred
environments and retain section markers as an additional block kind for future
shadow-file grouping. Ignore preamble/control arguments for extraction.
Why: source-order ownership avoids duplicated blocks and keeps dependencies
attached to the mathematical text that uses them.

Read-only runs assign transient UUIDs; `--write-ids` persists them using native
long `with` carriers. Invalid or duplicate existing IDs produce diagnostics and
block writeback rather than silently adding nested replacement carriers. Multiple
labels on one block are aliases; ambiguous labels do not create dependency edges.
Native source bytes, including Cork text, are preserved. Non-UTF-8 labels are not
linked in M1; this restriction is documented in the README.

The writer preserves paragraph separators and complete escape atoms. It rejects
crossing insertion spans, reparses the proposed output before replacement, uses
an exclusive temporary file with failure cleanup, preserves permissions, and
checks for a changed source before replacement. Symlink write targets are refused
to avoid replacing the link rather than the document. The protocol/plugin will
handle editing in memory in later milestones; M1 is an explicit batch tool.

## 2026-10-19 — M2 provider and toolchain contracts
Context: the brief's provider names and integration protocols require verification.
Choice: retain `anthropic/claude-sonnet-5` and `typesafe/jev-1.14`, both verified in
the live OpenRouter catalogs. Jev uses the separate `/api/alpha/decisions` endpoint;
it is not an ordinary chat-completion model. Treat missing confidence/probability
as uncertainty and missing cost as unknown. Why: silently substituting chat
responses or zero-cost values would misrepresent decisions and evaluation.
See [provider investigation](research/m2-models.md).

Pin Lean 4.19.0 with Mathlib `c44e0c8ee63ca166450922a373c7409c5d26b00b`
and REPL `3b27c8543a48444ffab4077f47526d91849cca3d`; their toolchains match.
Install under ignored repository-local directories rather than modifying shell
startup files. See [Lean investigation](research/m2-lean.md).

## 2026-10-19 — SIR identity and structure-check boundaries
Context: many M1 blocks have no source label, while mathematical strings can
contain meaningful whitespace inside literals. Choice: the SIR `label` field is
mandatory but nullable; present labels and dependency labels retain exact identity.
Whitespace normalization must not collapse quoted literals or code content.
Why: inferred labels and normalization collisions would create false cache hits.

The SIR crate remains independent of block storage and exposes explicit variable
and hypothesis counts. Lean-side extraction must preserve the actual declaration
header boundary: an unbounded telescope also consumes universal quantifiers in
the conclusion and arrows in a function result type. Structural checking cannot
certify arbitrary semi-formal prose equivalence. Unsupported comparisons must be
reported as unchecked, and definition bodies must be inspected separately from
their result types. No count-only check is evidence of complete faithfulness.

## 2026-10-19 — Candidate Lean environments and model source admission
Context: Lean error recovery may create declarations even when elaboration fails,
and a successfully elaborated declaration may still disagree with its SIR.
Choice: keep checked candidates separate and advance the worker's accepted
environment only after the caller accepts all required structural checks.
Repairs branch from the previous accepted environment. Trusted Lean parsing must
validate one restricted declaration before executing generated source; model
statements cannot supply extra commands or arbitrary elaboration tactics.
Why: failed or mismatched candidates must not affect later declarations.

## 2026-10-19 — M2 evaluation interpretation
Context: a small prototype corpus cannot establish general autoformalization
quality, and the semi-formal SIR admits equivalent wording.
Choice: start with ten explicitly specialized, source-attributed teaching
examples in `evals/`, with reviewed reference SIRs excluded from model inputs.
Report elaboration, supported structural checks, unchecked content, exact SIR
agreement and cost separately. Why: neither compilation nor exact wording alone
establishes faithfulness. Broader course-note coverage remains a limitation to
state in the M2 go/no-go report.

## 2026-10-19 — Pinned interpreter runtime on this host
Context: binaries linked with the pinned Lean release fail macOS dyld validation
on this host (`__DATA_CONST` lacks `SG_READ_ONLY`), including the REPL and Mathlib
cache helper. Choice: run the same pinned modules using official Lean `--run`;
keep a persistent REPL process and use the interpreter for cache downloads.
Why: this preserves the checked toolchain and protocol without changing upstream
binaries or silently upgrading Lean/Mathlib. Setup is currently Apple Silicon
macOS-specific. All downloads and caches remain repository-local and ignored.

The shadow generator rejects cycles between section modules, even when a block
graph could be linearized by splitting sections. This conservative M2 limitation
avoids invalid import cycles; section splitting is not inferred silently.
