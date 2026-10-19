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
