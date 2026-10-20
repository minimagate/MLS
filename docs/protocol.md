# MLS engine ↔ client protocol

The engine owns all state; the client renders what the engine returns. M3 is
batch-triggered: the TeXmacs plugin runs one short `mls` command per user action
and reads one JSON document from `--output`. M4 replaces the per-command
transport with a persistent daemon that carries the same messages.

All commands take the absolute path of a saved `.tm` document. State is stored
next to it in the document-adjacent `.mls/` project:

- `.mls/review/<name>.store.json` — the human review store (confirmed, edited
  and rejected SIRs, Lean cache), keyed by persistent block ID.
- `.mls/review/<name>.report.json` — the last batch report for the document.

`<name>` is the document file name. Commands never call a model except
`review-run`, which follows the M2 rules: exactly one of `--replay DIR` or
`--live`, and `--live` also requires `MLS_LIVE=1`.

## Commands

```
mls review-run    FILE.tm (--replay DIR | --live) --output OUT.json [--max-attempts N]
mls review-action FILE.tm --block UUID --action confirm|reject|edit [--sir SIR.json] --output OUT.json
mls review-status FILE.tm --output OUT.json
```

- `review-run` requires every formal block to carry a persisted `mls-id`. If any
  does not, it fails with exit status 3 and the error `missing_ids`; the client
  then runs `mls --write-ids FILE.tm`, reloads the buffer and retries.
- `review-action` changes only the review store and recomputes the view from the
  stored last report. It never calls a model or Lean. `edit` reads a complete
  SIR JSON object from `--sir`.
- `review-status` recomputes the view from stored state.

Exit status 0 means `OUT.json` holds a view. Any other status means it holds an
error object (when it could be written) and stderr explains the failure.

## View (`OUT.json`)

```json
{
  "protocol": 1,
  "document": "/abs/path/notes.tm",
  "counts": {"not_formalized": 0, "sir_drafted": 2, "statement_confirmed": 1,
             "formalization_error": 1},
  "proof_counts": {"verified": 1, "unproven": 0, "refuted": 0},
  "needs_rerun": false,
  "blocks": [
    {
      "block_id": "UUID",
      "label": "thm:main",
      "kind": "theorem",
      "status": "statement_confirmed",
      "proof_status": "verified",
      "review": "confirmed",
      "needs_rerun": false,
      "message": "short human-readable summary for the marker tooltip",
      "sir": { "...": "the SIR, or null" },
      "lean_statement": "theorem … := by sorry",
      "checks": [{"field": "goal.outer_head", "state": "matched", "scope": "structure", "detail": "…"}],
      "errors": ["…"],
      "proof": {"status": {"status": "verified"}, "candidate": {"proof_sir": {"steps": ["…"], "uses": [], "assumptions_made": []}, "lean_proof": "by exact?"}, "attempts": 1, "diagnostics": []},
      "cosmetic_reconfirmation": false,
      "superseded_confirmation": null,
      "previously_rejected": false
    }
  ]
}
```

- `status` remains the formalization state and is one of the four values in
  `counts`. `proof_status` is absent until a proof attempt has completed, then
  is `verified`, `unproven` or `refuted`. The optional `proof_counts` summarizes
  completed proof outcomes. `proof` carries the bounded candidate and
  block-local Lean diagnostics; its `status` is a tagged status object, for
  example `{"status":"verified"}` or
  `{"status":"refuted","evidence":"…"}`. `review` is `none`, `confirmed`,
  `edited` or `rejected`.
- `statement_confirmed` appears only when a human confirmation (or edit) is in
  effect for the exact SIR shown, its Lean statement was accepted, and every
  formal dependency is itself `statement_confirmed`. A confirmed block whose
  dependencies are not yet confirmed is shown as `sir_drafted` with a
  "waiting for dependency confirmation" message.
- The view compares each block's current text, SIR identity (kind, label,
  uses) and §4 dependency key with the stored report. Any drift sets
  `needs_rerun` and downgrades the block (identity drift to `not_formalized`).
- `needs_rerun` marks blocks whose stored decision cannot be reflected without a
  new run (an edited SIR has no Lean statement yet, or a rejection awaits a new
  draft). The top-level flag is true when any block needs a rerun.
- `review confirm` queues a proof-only job for the exact accepted Lean
  statement and confirmed SIR. The statement stays visible while proof search
  runs. A failed search is `unproven`; it does not imply that the statement is
  false. `refuted` requires separate positive Lean-checked negation or
  counterexample evidence. The current proof search generates and checks
  proofs; it does not search for refutations.
- Blocks appear in dependency order. Unknown fields must be ignored by clients.

Error object: `{"protocol": 1, "error": "code", "message": "…"}`.

Exit status 2 marks a refusal and 3 marks `missing_ids`; 1 is any other
failure. Error codes: `missing_ids`, `invalid_ids`, `invalid_document`,
`invalid_store`, `invalid_report`, `no_report`, `unknown_block`,
`stale_report`, `no_usable_sir`, `invalid_sir`, `recording_incomplete`,
`refused`, `failed`.

Additional rules:

- After a cosmetic reconfirmation, `review-run` re-anchors the confirmation to
  the block's current text, so it does not stay `needs_rerun`.
- Formal blocks missing from the stored report appear as `not_formalized` with
  `needs_rerun`; a stored report that lists removed blocks also sets the
  top-level `needs_rerun`.
- Rejected blocks show `sir: null` until a new draft exists.
- `review-run` writes the report, the store and the shadow project before the
  view. If a later step fails, `OUT.json` holds the error and the saved state
  stays readable through `review-status`.

## Persistent daemon (`mls serve`)

`mls serve (--replay DIR | --live)` reads one JSON-RPC 2.0 request per UTF-8
line from stdin and writes one ASCII-escaped JSON-RPC message per line to
stdout. Every request has a non-null string or number `id`; notifications are
not accepted. Stderr carries diagnostics. The input line limit is 4 MiB. Live
mode also requires `MLS_LIVE=1`. A session owns one saved `.tm` document and
one Lean worker until `shutdown` or disconnect.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"didOpen","params":{"document":"/abs/notes.tm","revision":1,"source":"...full TeXmacs source...","active":null}}
{"jsonrpc":"2.0","id":3,"method":"didChange","params":{"document":"/abs/notes.tm","revision":2,"source":"...full replacement source...","active":"UUID"}}
{"jsonrpc":"2.0","id":4,"method":"cursor","params":{"document":"/abs/notes.tm","revision":2,"active":null}}
{"jsonrpc":"2.0","id":5,"method":"review","params":{"document":"/abs/notes.tm","revision":2,"block":"UUID","action":"confirm"}}
{"jsonrpc":"2.0","id":6,"method":"retry","params":{"document":"/abs/notes.tm","revision":2,"block":"UUID"}}
{"jsonrpc":"2.0","id":7,"method":"shutdown","params":{}}
```

`didOpen` takes the initial complete source snapshot. `didChange` replaces the
whole snapshot and requires a strictly increasing revision. `cursor`, `review`
and `retry` require the current revision; stale requests return `-32001` and
leave the document unchanged. `review` accepts `confirm`, `reject`, or `edit`;
`edit` includes a complete `sir` object. `shutdown` persists the review store
and returns formalization usage, decision usage and any fatal session error.
The TeXmacs client waits for a status view at the acknowledged revision before
accepting live review actions.

The TeXmacs live client assigns stable `mls-id` carriers to uncarried formal
blocks and nonblank direct children of the body document, which represent
paragraph-level blocks. Existing carriers are left intact. Startup IDs are
persisted through the guarded `--write-ids` save/reload path; IDs assigned to
blocks added during a session stay in memory until the user saves the document.
Existing persisted untagged prose is classified in the live path. New untagged
prose typed during a session is a v1 limitation: it becomes eligible for
classification after the document is saved and the live session is restarted,
so startup can assign and persist its ID. The client sends no cursor or review
action against a source revision while a document change is awaiting
acknowledgement.

After a successful document operation, the server may send these unnumbered
JSON-RPC notifications, each tagged with the document and current revision:

- `blockStatus`: the current formalization and proof view, dirty block IDs,
  formalization, proof and decision usage, plus Jev's field-level review
  annotations.
- `reviewRequest`: draft SIRs that are ready for a person's review.
- `diagnostics`: parser diagnostics, block errors and fatal session state.

The daemon asks the configured pinned decision model to classify persisted,
untagged prose, decide whether blocks are finished, judge whether a text-only
edit without a SIR is cosmetic, and identify implicit reliance on an earlier
labeled statement. A confident prose `claim` is treated as a theorem and enters
the ordinary formalization, dependency, cache and review paths. `proof_step`,
`commentary`, and low-confidence or failed classifications stay non-formal.
Classification annotations, fallback evidence and aggregate decision usage are
included in `blockStatus`. The daemon also reports semantic-difference and
field-level SIR hints after combined generation. These hints only affect
scheduling and review presentation: generated SIRs stay drafts, and only an
explicit `review` request can confirm one. No Jev decision confirms a block.
