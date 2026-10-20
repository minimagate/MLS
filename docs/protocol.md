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
  "needs_rerun": false,
  "blocks": [
    {
      "block_id": "UUID",
      "label": "thm:main",
      "kind": "theorem",
      "status": "sir_drafted",
      "review": "none",
      "needs_rerun": false,
      "message": "short human-readable summary for the marker tooltip",
      "sir": { "...": "the SIR, or null" },
      "lean_statement": "theorem … := by sorry",
      "checks": [{"field": "goal.outer_head", "state": "matched", "scope": "structure", "detail": "…"}],
      "errors": ["…"],
      "cosmetic_reconfirmation": false,
      "superseded_confirmation": null,
      "previously_rejected": false
    }
  ]
}
```

- `status` is one of the four values in `counts` (spec §7, M3 subset; proof
  statuses arrive in M5). `review` is `none`, `confirmed`, `edited` or
  `rejected`.
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
