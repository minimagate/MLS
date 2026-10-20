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

## 2026-10-19 — Conservative outer-head recognition and check scopes
Context: the final pipeline review reproduced false hard results from the SIR
outer-head guesser: `x = 0 and P` matched Lean `eq`, while prose quantifiers
(`For each x, …`, `There is x with …`) and `¬ x = 0` produced false mismatches.
A second review found that a blocklist kept leaking (TeX/TeXmacs spellings such
as `<rightarrow>`, curly quotes, "there exists exactly one").
Choice: every character must be in an explicit allowlist (ASCII alphanumerics,
basic operators, a fixed set of Unicode mathematical symbols, Greek letters);
TeXmacs tags and TeX commands are rejected. Then recognize only a leading `∀`,
a leading `∃` (not `∃!` and without counting words such as "exactly" or
"unique"), or a purely symbolic top-level `↔ → ∨ ∧ =` scan with no
natural-language connective, top-level comma/semicolon/colon or unspaced
connective. Prose quantifier prefixes are not recognized. For definitions an
equality head stays unchecked, because `=` in a defining expression may separate
the defined name from the body. A third review, probing about 1,300 generated
conclusions against real Lean heads, found prose-led text ("No real x satisfies
…") and relation chains inside quantifier bodies (Lean ends the body before a
second relation or `↔`). Outside a quantifier prefix, any purely alphabetic
ASCII word of two or more letters now vetoes recognition unless it is a listed
function name (`sin`, `exp`, `ker`, …). Any depth-0 `↔` in a quantifier body,
and any segment between connectives holding two relations, also leaves the
head unchecked. A final review found that convergence arrows (`a_n → 0`) were
read as implication, although Mathlib encodes limits with `Tendsto`. Implication
is now recognized only when every top-level operand contains a relation or
logical connective; nested arrows and `lim` leave the head unchecked, and the
prose-word veto also applies inside quantifier bodies. Why: a hard check must
never guess; unchecked is the honest result for unsupported prose.

Every structure check now carries `scope`: `structure` (mechanical) or
`semantics` (variable types, hypothesis/conclusion/body content, always
unchecked in M2). `complete_structure` means accepted with every
structure-scoped check implemented and matched. Why: the previous definition
could never be true, because semantic content is unchecked by design.

## 2026-10-19 — No lost paid results
Context: recording I/O failure discarded successful paid replies, and late
filesystem errors (missing output directory, stale shadow manifest) discarded
the batch report after all model calls. Choice: `RecordingClient` returns the
provider result unchanged and exposes `persistence_failure()`; the CLI preflights
the output location, writes the report before the shadow project, and exits
nonzero afterwards for incomplete recordings or shadow failures. After the first
unpersisted record, the recording client makes no further live calls. Replay misses
no longer count as provider calls. Why: a retry must not silently pay twice, and
an unreplayable recording must be visible.

## 2026-10-20 — M2 iteration 2: SIR contract and admission notations
Context: live run 1 showed typing facts restated as SIR hypotheses (padded in
Lean with vacuous `True` binders), bound variables listed as SIR variables, TeX
or prose conclusions, and our admission grammar rejecting `|x|` and binder
predicates such as `∃ c ∈ S`. Choice: the SIR instructions now define variables
as the free parameters only, hypotheses as propositional assumptions only, and
conclusions in Unicode notation without TeX. The Lean instructions fix binder
order and forbid padding. A hard structure check rejects any hypothesis binder
whose extracted type is literally `True`; it is deliberately literal, so other
vacuous forms (`1 = 1`, `True ∧ True`) remain undetected and are a stated
limitation. Admission accepts additional pure term notations (binder
predicates, `∃!`, `≠ ¬ ↔ ∉ ⊆ ∣ • ∘ ⁻¹ ×`, unary minus, set-builder, absolute
value, numeric projections). `group` nodes are admitted only inside the
absolute-value and typed set-builder shapes. `deriving` clauses and
proof-escape identifiers (`sorryAx`, `native_decide`, `ofReduceBool`, …) are
rejected. Why: these notations expand to ordinary applications and execute
nothing at admission; the restriction keeps generic parser nodes from opening
other constructs.

## 2026-10-20 — Keep the evaluated prompt
Context: the final review noted that the Lean instructions mention
"autoImplicit false semantics" in the same sentence that forbids `set_option`.
Choice: leave the wording unchanged in the M2 commit, so the committed prompts
are exactly those that produced, and replay, evaluation run 3. Why: changing
prompts invalidates the recorded evidence; the wording is revisited with the
next prompt change.

## 2026-10-20 — M3 human review semantics
Context: spec §8 requires human confirmation of each SIR before a statement
counts as confirmed, and pins confirmed SIRs until the block's meaning changes.
Choice: a document-scoped review store records `confirmed` (optionally
human-edited) and `rejected` SIRs, each bound to the block's text hash and SIR
semantic hash. A confirmed SIR is reused without a model call while the text is
unchanged. After a text change the SIR is regenerated; an identical semantic
hash keeps the confirmation (cosmetic edit, §8 step 6), a different one returns
the block to `sir_drafted` and shows the difference. Rejected SIRs are sent
back to the model as a negative example; an identical regeneration stays
unformalized. `statement_confirmed` requires a human decision in effect and an
accepted Lean statement for that exact SIR. Accepted Lean statements for
confirmed SIRs are cached by the §4 key (SIR hash plus dependency SIR hashes)
and re-checked locally before any regeneration. Why: confirmation must never be
inferred from model output, and unchanged meaning must not cost model calls.

## 2026-10-20 — M3 markers in a status panel; JSON over short commands
Context: the investigation in `research/m3-texmacs.md` found no GUI-visible
marker in TeXmacs 2.1.5 that leaves the saved document and its LaTeX export
unchanged; colored wrappers, balloons and style macros all alter the document.
Choice: the document carries only `mls-id` carriers. Per-block markers live in
an auxiliary "MLS status" buffer (colored status, tooltip message, go-to and
review actions); the badge is the footer message plus the panel header. The
plugin runs short `mls review-*` commands and parses their JSON with a pure
Scheme reader, never evaluating engine output (protocol in `protocol.md`). Live
model calls need an explicit menu toggle in addition to `MLS_LIVE=1`.
Why: this keeps documents byte-stable and the plugin a thin, replaceable client.
The spec's "marker per block" is met by the panel row, not an in-text mark.

## 2026-10-20 — Unformalized versus failed, and confirmation through dependencies
Context: M3 adds `not_formalized` to the status vocabulary. A failed SIR call,
an SIR whose kind, label or uses disagree with the source, a non-UTF-8 block
and a blocked dependency have no usable SIR, so calling them formalization
errors overstated progress. A review also showed that a confirmed block could
reach `statement_confirmed` on top of an unconfirmed dependency, and that the
status view trusted stored reports after the document changed.
Choice: those cases report `not_formalized`; `formalization_error` means a
usable SIR exists but no Lean statement was accepted. `statement_confirmed`
additionally requires every formal dependency to be `statement_confirmed`;
otherwise the confirmed block waits as `sir_drafted`. The view re-checks text,
SIR identity and the §4 dependency key against the current document, and any
drift downgrades the block and asks for a rerun. Why: a confirmed statement
must rest only on confirmed meaning, and stale state must never look current.

## 2026-10-20 — Per-document shadow modules
Context: spec §9 shows one flat `Shadow/SectionNN.lean` set per project, but the
project lives next to the document, so two documents in one directory claimed
the same files and the second was refused. Choice: each document writes
`.mls/Shadow/<Module>/SectionN.lean`, where `<Module>` is `Doc_`, the sanitized
ASCII file stem (at most 40 characters), `_` and the first eight hex digits of
the file name's SHA-256. Ownership markers and manifests are per module. An M2
flat layout is removed only when its owner marker names the same document, and
only its manifest-listed regular files. Why: documents sharing a directory are
common, and generated files must stay attributable to exactly one document.

## 2026-10-20 — Block identity from the TeXmacs tree, not source layout
Context: an end-to-end test through the plugin showed that TeXmacs 2.1.5
re-serializes every block on save (long forms, indentation) without changing
the document tree. Hashing raw bytes therefore made every save look like a text
edit: confirmations went stale, SIRs were regenerated at model cost, and
recorded requests stopped matching. Choice: supersede the M1 rule that block
hashes use raw bytes. Each block gets a canonical text derived from the parsed
tree under TeXmacs' own reader rules (layout whitespace ignored, content
whitespace and structure kept, identity carriers removed). `text_hash` hashes
that text, and model prompts use it. Source spans and every write remain
byte-exact. Validation: the canonical text of each block must be unchanged by a
real TeXmacs load-and-save, and texts TeXmacs reads as different trees must
differ (checked against TeXmacs' own snippet parser on random sources). Known
TeXmacs limitation: text after raw data inside a long-form argument is lost by
TeXmacs itself on load; the plugin refuses documents that do not round-trip. Why: identity must follow meaning-bearing content,
and TeXmacs, not MLS, owns the file's layout.

## 2026-10-20 — Refuse rather than reinterpret malformed separators
Context: TeXmacs never reads a bare `|` or `>` as text; inside a tag argument it
splits the argument, and at body level the body fails to load. Such bytes only
occur in hand-written, already malformed files, yet wrapping them in an
identity carrier, or treating a carrier with them as transparent, would give
MLS a different reading from TeXmacs. Choice: `--write-ids` refuses to wrap a
span with a bare separator at the carrier's level and leaves the file
untouched; canonicalization refuses such carriers (`canonicalization_failed`,
no model call). The plugin refuses its first command when the file on disk
differs from the buffer, so you save or revert in TeXmacs first. Why: MLS must
read every block exactly as TeXmacs does, and must never write over content it
did not create.
