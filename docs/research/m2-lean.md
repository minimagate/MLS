# M2 Lean worker investigation

Investigated 2026-10-19. Host confirmed by `uname`: Darwin, arm64. No installation or implementation was performed for this investigation; the metaprogram sketch below is source-checked against the pinned APIs but **not compiled**. A real compilation/integration run remains an implementation acceptance gate.

## Reproducible compatible pins

Use the known stable 4.19.0 family, rather than independently selecting latest Lean, Mathlib and REPL versions. Their official tags and files were fetched directly from GitHub during this investigation:

| Component | Tag | Exact commit |
|---|---|---|
| Lean | v4.19.0 | `6caaee842e9495688c1567e78c0e68dbb96942aa` |
| Mathlib | v4.19.0 | `c44e0c8ee63ca166450922a373c7409c5d26b00b` |
| community REPL | v4.19.0 | `3b27c8543a48444ffab4077f47526d91849cca3d` |

Both [Mathlib's toolchain](https://github.com/leanprover-community/mathlib4/blob/c44e0c8ee63ca166450922a373c7409c5d26b00b/lean-toolchain) and [REPL's toolchain](https://github.com/leanprover-community/repl/blob/3b27c8543a48444ffab4077f47526d91849cca3d/lean-toolchain) specify `leanprover/lean4:v4.19.0`. Matching this exact toolchain avoids binary `.olean` incompatibilities.

Official ARM macOS assets from the [Lean release](https://github.com/leanprover/lean4/releases/tag/v4.19.0):

- [ZIP, 460659437 bytes](https://github.com/leanprover/lean4/releases/download/v4.19.0/lean-4.19.0-darwin_aarch64.zip)
- [tar.zst](https://github.com/leanprover/lean4/releases/download/v4.19.0/lean-4.19.0-darwin_aarch64.tar.zst)

The release API returned no asset digest. A local download hash can detect subsequent changes to the same local archive, but is not independent upstream integrity evidence. Prefer ZIP here because it avoids introducing a zstd dependency.

## Local-only setup plan

`.toolchains/` and `.mls/` are already ignored. Download/unzip the official release beneath `.toolchains/`, then invoke its `bin/lake` and `bin/lean` directly. No Elan installation or shell startup-file changes are necessary. Export the release `bin` on the child process PATH, using a task-specific variable such as `MLS_TOOLCHAIN`; do not overwrite HOME. Set `MATHLIB_CACHE_DIR` to an absolute path within `.toolchains/mathlib-cache` for every Lake process. The pinned [Mathlib cache source](https://github.com/leanprover-community/mathlib4/blob/c44e0c8ee63ca166450922a373c7409c5d26b00b/Cache/IO.lean#L42) explicitly supports that override, otherwise falling back to XDG/HOME caches.

Suggested layout:

```text
.toolchains/lean-4.19.0-darwin_aarch64/
.toolchains/repl/                 # checkout exact REPL commit
.toolchains/mathlib-cache/
.mls/lakefile.lean
.mls/lean-toolchain
.mls/MlsExtract.lean              # generated/copied from versioned lean/ source
.mls/Shadow/Section01.lean
```

After extracting Lean and checking out the REPL pin:

```sh
# Run from the repository root; resolve absolute paths in the setup script.
export MLS_TOOLCHAIN="$PWD/.toolchains/lean-4.19.0-darwin_aarch64"
export PATH="$MLS_TOOLCHAIN/bin:$PATH"
export MATHLIB_CACHE_DIR="$PWD/.toolchains/mathlib-cache"
lake --version
```

Run `lake build` with working directory `.toolchains/repl`. For `.mls/lakefile.lean` use:

```lean
import Lake
open Lake DSL
package mlsShadow
require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @
  "c44e0c8ee63ca166450922a373c7409c5d26b00b"
lean_lib MlsExtract
lean_lib Shadow
```

Write `leanprover/lean4:v4.19.0` into `.mls/lean-toolchain`. In `.mls`, run `lake update`, `lake exe cache get`, and `lake build MlsExtract`. Keep the generated `lake-manifest.json` for exact transitive dependency resolution; never silently update during normal checks. `lake update` can itself invoke the Mathlib cache post-update hook, so its cache environment must already be local. Mathlib's [dependency documentation](https://github.com/leanprover-community/mathlib4/wiki/Using-mathlib4-as-a-dependency) confirms downloading compiled caches is essential to avoid compiling the entire library.

Start the long-running worker in `.mls` with `lake env <absolute-repo>/.toolchains/repl/.lake/build/bin/repl`, as prescribed by the [REPL README](https://github.com/leanprover-community/repl/blob/3b27c8543a48444ffab4077f47526d91849cca3d/README.md). Do not use a fresh `lake env lean` process for each declaration.

## Exact command protocol and ownership

Read the pinned [JSON structures](https://github.com/leanprover-community/repl/blob/3b27c8543a48444ffab4077f47526d91849cca3d/REPL/JSON.lean) and [main loop](https://github.com/leanprover-community/repl/blob/3b27c8543a48444ffab4077f47526d91849cca3d/REPL/Main.lean). Send one JSON object followed by **two newlines**, flush stdin, read response until the blank line. Responses can span multiple lines. Consume stderr separately to avoid deadlock. Empty optional arrays such as `messages` and `sorries` are omitted; serde fields need defaults.

Initialize exactly once per worker:

```json
{"cmd":"import Mathlib\nimport MlsExtract\nset_option autoImplicit false"}
```

Save the returned numeric `env`, e.g. `0`. Only calls without `env` may contain imports. Then:

```json
{"env":0,"cmd":"-- block:example-id\ntheorem mls_example (x : ℝ) (h : 0 ≤ x) : x ≤ x := by sorry\n#mls_extract mls_example"}
```

Successful responses have `env`, optionally `messages`, `sorries`, `tactics`, and `infotree`. Messages contain `severity` (`info`, `warning`, `error`, or `trace`), `pos: {line,column}`, optional `endPos`, and `data`. Outer protocol failures use `{ "message": "..." }` instead. Treat both error forms as failures; presence of an `env` is not proof of success.

Save a response environment only after validating the entire command, extraction payload, and hard structural checks. A failed statement can still create declarations via Lean error recovery. Repair from the last accepted environment, not the failed one. Retain an import-only base environment for independent evaluation cases. For a document, advance through dependency order; on an edit, replay from the last valid predecessor snapshot. Restart/replay periodically because REPL stores all snapshots and offers no deletion command.

Lean lines are 1-based and columns 0-based in this protocol. Use per-request generated source spans, not the previous request's line offset. Preserve Unicode column semantics when converting to editor positions; add a test with multibyte mathematics before an error. The source-map builder should associate exact declaration ranges with IDs and emitted `-- block:<id>` comments. An extraction-command error outside the declaration range should be explicitly associated with its requested block, never guessed from unrelated earlier comments.

## Extraction through a trusted Lean command

Compile a small versioned `MlsExtract` module and import it with Mathlib once. It should expose `#mls_extract ident` implemented with `elab ... : command`, `Lean.Elab.Command.liftTermElabM`, `getConstInfo`, `Lean.Meta.forallTelescope`, `Expr.fvarId!.getDecl`, `Lean.Meta.isProp`, and `Lean.Meta.ppExpr`. These exact APIs exist in the pinned [Command](https://github.com/leanprover/lean4/blob/6caaee842e9495688c1567e78c0e68dbb96942aa/src/Lean/Elab/Command.lean), [Meta.Basic](https://github.com/leanprover/lean4/blob/6caaee842e9495688c1567e78c0e68dbb96942aa/src/Lean/Meta/Basic.lean), and [Print](https://github.com/leanprover/lean4/blob/6caaee842e9495688c1567e78c0e68dbb96942aa/src/Lean/Elab/Print.lean) sources.

Emit a reserved prefix plus `Json.compress` through `logInfo`, so the payload remains inside REPL JSON message `data`. Do not `IO.println` raw JSON from the extractor: it would corrupt framing. Require exactly one expected extraction payload per declaration, matching the declaration name, and reject absent/duplicate payloads.

Implementation sketch (not compiled):

```lean
import Lean
import Lean.Util.CollectAxioms
open Lean Meta Elab Command

elab "#mls_extract " id:ident : command => do
  let name := id.getId  -- generated names are fully qualified
  let ci ← getConstInfo name
  let axioms ← collectAxioms name
  let payload ← liftTermElabM do
    forallTelescope ci.type fun xs goal => do
      let binders ← xs.mapM fun x => do
        let d ← x.fvarId!.getDecl
        let ty ← ppExpr d.type
        let prop ← isProp d.type
        let bi := match d.binderInfo with
          | .default => "explicit"
          | .implicit => "implicit"
          | .strictImplicit => "strictImplicit"
          | .instImplicit => "instance"
        pure <| Json.mkObj [
          ("name", toJson d.userName.toString),
          ("type", toJson ty.pretty),
          ("is_proposition", toJson prop),
          ("binder_info", toJson bi)]
      let g ← ppExpr goal
      pure <| Json.mkObj [
        ("declaration", toJson name.toString),
        ("binders", toJson binders),
        ("goal", toJson g.pretty),
        ("axioms", toJson (axioms.map Name.toString))]
  logInfo m!"MLS_EXTRACT:{payload.compress}"
```

Add declaration kind from `ConstantInfo` (`thmInfo` versus `defnInfo`, rejecting axiom/opaque/unsafe substitutes), conclusion head kind from the elaborated expression (`Exists`, `And`, `Or`, `Iff`, `Eq`, `False`, or atom), plus definition-body inspection as needed. Use AST constructors/names, never pretty-printed text regexes, for Lean-side structure. Keep `ppExpr` for human diagnostics only.

### Structural comparison limits that require explicit decisions

- All `forallE` binders look alike after elaboration. A source header variable and a universal quantifier in the conclusion cannot be distinguished from the declaration type alone. An unrestricted telescope consumes both, including function arrows in definition result types. Do not falsely compare its total length with only `SIR.variables.length`.
- Choose a documented canonical generated format: explicit SIR variables first, named proposition hypotheses next, then the conclusion. Preserve/validate the declaration-header boundary from syntax, and use it to split the elaborated type. Alternatively, classify all outer universal variables into SIR variables consistently. The first route preserves the given SIR example and richer conclusions. `forallBoundedTelescope` can open a validated prefix, but passing an unvalidated expected count alone is insufficient to detect dropped or moved binders.
- Keep binder classes separate. `(P : Prop)` is a variable because `P`'s type is `Prop`, not a proposition. `(h : P)` is a hypothesis because its type is proposition-valued. `[Nonempty α]` is an instance binder even though `Nonempty α` is a proposition. Preserve implicit type parameters and typeclass instances; do not silently count them as prose hypotheses. Use `autoImplicit false` to prevent accidental undeclared variables. Define a policy for permitted implicit infrastructure versus explicit mathematical assumptions and test it.
- The SIR contains arbitrary semi-formal text, so a deterministic program cannot certify its full semantic equivalence to Lean. It can check header variable/hypothesis counts, supported type categories, declaration kind, and a small explicitly supported outer conclusion grammar. Unknown prose shape must be `unchecked`/unsupported with an honest explanation, not a structural success. A hard mismatch blocks acceptance. Model-supplied expected metadata is not independent evidence of correctness.
- For a definition, checking only its result type misses the defined expression entirely (`def f : ℝ := 0` versus `:= 1`). Include actual definition-body structure and report unsupported body comparisons. A definition with body `sorry` is only a placeholder; never claim the defining content has been faithfully checked merely because the type elaborates.
- Keep normal forms conservative. `forallTelescopeReducing` unfolds definitions and can erase useful syntactic structure. Avoid arbitrary reduction of predicates when the intended check is their visible outer shape.

## Sorry, kernel trust and status honesty

Do not infer absence of sorry from a clean error list or an empty `sorries` field. Inspect transitive axioms with `Lean.collectAxioms`: its [implementation](https://github.com/leanprover/lean4/blob/6caaee842e9495688c1567e78c0e68dbb96942aa/src/Lean/Util/CollectAxioms.lean) traverses declaration types and bodies through the checked kernel environment. `sorryAx` on this list includes dependencies hidden behind other constants; direct `Expr.hasSorry` does not establish that.

Use named declarations, not anonymous `example`, so the exact constant can be inspected. The M2 expected state is successful elaboration **with** `sorryAx`; report elaborated placeholder separately from user confirmation and never verified. For later M5, no `sorryAx` is necessary but still insufficient if generated text can introduce arbitrary axioms or unsafe assumptions. Permit only intended declaration syntax, keep the trusted Mathlib/extractor baseline fixed, and reject unknown new axioms; the usual baseline may include `propext`, `Classical.choice`, and `Quot.sound`. A theorem proved using another theorem with `sorry` remains unverified even if the submitted proof text has no `sorry` token.

The pinned REPL tactic-mode `proofStatus` checks direct `pf.hasSorry`; moreover completed tactic proofs are not substituted back into the original declaration environment by that protocol. M5 must re-elaborate the complete named theorem and recheck transitive trust. Do not make the M2 worker depend on experimental tactic mode.

Generated Lean is executable metaprogramming if arbitrary commands are allowed. The model-output boundary should admit only the intended declaration, reject appended imports/commands/axioms, and keep extractor invocation under engine control. This also prevents fake extraction messages from generated `#eval` or `#print` commands. Test the actual admission parser rather than relying on instruction text to the model.

## Required real integration cases

1. One persistent process imports Mathlib/extractor, then checks two dependent declarations with successive `env` values; second uses the first.
2. A theorem with `by sorry` elaborates and extractor reports `sorryAx`; a separate `by rfl` theorem does not. A theorem whose proof references a sorry-containing dependency still reports `sorryAx`.
3. A missing hypothesis and wrong declaration kind are hard mismatches. Function-valued variables, implicit type variables, proposition variables, proposition hypotheses, and proposition-valued class instances are classified correctly.
4. Universal/existential conclusions and function-valued definitions preserve the header/conclusion boundary; definition body changes cannot pass by result-type equality alone.
5. Unknown names/type errors map to the correct block and original positions, including Unicode. An error-recovery declaration is never committed to the accepted environment.
6. A failed attempt followed by a repaired same-name declaration succeeds by branching from the pre-error environment. Malformed JSON, worker EOF, timeout, missing/duplicate extraction messages, and absent default arrays are handled without hanging.
7. Adversarial appended commands/axioms cannot spoof the extractor or bypass trust checks. Unsupported semi-formal structure is reported as unchecked rather than passed.

These are real-REPL tests, distinct from Rust unit tests of recorded protocol responses. Keep them explicitly gated by toolchain availability, but run them for M2 acceptance on this machine; a skipped suite is not completion evidence.
