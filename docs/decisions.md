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
