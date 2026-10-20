import Lean
import Lean.Util.CollectAxioms

/-!
Trusted structure extraction for MLS-generated theorem and definition declarations.

The extractor reparses the exact declaration source and obtains the header boundary
from its syntax.  It never accepts a model-provided binder count.
-/

open Lean Meta Elab Command

namespace MlsExtract

private def binderCount (stx : Syntax) : Except String Nat := do
  let kind := stx.getKind
  if kind == ``Lean.Parser.Term.explicitBinder ||
      kind == ``Lean.Parser.Term.implicitBinder ||
      kind == ``Lean.Parser.Term.strictImplicitBinder then
    let names := stx.getArg 1
    if names.getNumArgs == 0 then
      throw "a declaration-header binder has no name"
    return names.getNumArgs
  if kind == ``Lean.Parser.Term.instBinder then
    -- `[C a]` and `[inst : C a]` each elaborate to one binder.
    return 1
  throw s!"unsupported declaration-header syntax: {kind}"

private structure ParsedHeader where
  name : Name
  kind : String
  binderCount : Nat

/-- Keyword atoms that can never appear in admitted source.  `sorry` is deliberately
absent: the engine-injected placeholder body `by sorry` contains that atom.
`deriving` is the keyword of the optional deriving clause: Lean stores it in a
plain `null` node after the definition body, so only this atom rejects it. -/
private def blockedAtoms : List String :=
  ["run_tac", "eval%", "unsafeIO", "do", "unsafe", "partial",
    "implemented_by", "macro", "syntax", "elab", "deriving", "dbg_trace", "panic!"]

/-- Search syntax that can launch an external request in the pinned Mathlib environment.
These are proof-only because `firstBlockedProofSyntax?` runs only during proof admission. -/
private def blockedProofExternalAtoms : List String :=
  ["#loogle", "#leansearch", "#moogle", "#statesearch", "#search", "polyrith"]

/-- Identifier components that can never appear in admitted source.  Each
component of a dotted name is checked, so `Lean.ofReduceBool` is rejected through
its `ofReduceBool` component.  A leading `@` is ignored.  Identifiers are otherwise
unrestricted: `Classical.choice` is admitted because the mathematics needs it. -/
private def blockedIdentifiers : List String :=
  ["sorryAx", "sorry", "native_decide", "ofReduceBool", "ofReduceNat", "run_tac",
    "implemented_by", "extern", "unsafeCast", "unsafeIO", "IO", "dbgTrace", "dbg_trace",
    "panic", "panic!", "trustCompiler"]

private def nameComponents : Name → List String
  | .anonymous => []
  | .str pre s => s :: nameComponents pre
  | .num pre _ => nameComponents pre

/-- The first blocked component of an identifier, if any. -/
private def blockedIdentifier? (name : Name) : Option String :=
  (nameComponents name).findSome? fun component =>
    let bare := if component.startsWith "@" then component.drop 1 else component
    if blockedIdentifiers.contains bare then some bare else none

/-- Node kinds admitted by the generic walk.  `group` is deliberately absent:
Lean's `group` node is produced by `|a|` and by typed set-builder binders, and
only those two exact shapes are admitted (see `absBarsShape` and
`setBuilderRejected?`).  A `group` anywhere else is rejected. -/
private def admittedKinds : List String := [
    "null",
    "Lean.Parser.Command.declaration", "Lean.Parser.Command.theorem",
    "Lean.Parser.Command.definition", "Lean.Parser.Command.declModifiers",
    "Lean.Parser.Command.declId", "Lean.Parser.Command.declSig",
    "Lean.Parser.Command.optDeclSig", "Lean.Parser.Command.declValSimple",
    "Lean.Parser.Term.explicitBinder", "Lean.Parser.Term.implicitBinder",
    "Lean.Parser.Term.strictImplicitBinder", "Lean.Parser.Term.instBinder",
    "Lean.Parser.Term.type", "Lean.Parser.Term.prop", "Lean.Parser.Term.app",
    "Lean.Parser.Term.typeSpec", "Lean.Parser.Term.forall", "Lean.Parser.Term.arrow",
    "Lean.Parser.Term.fun", "Lean.Parser.Term.basicFun", "Lean.Parser.Term.let",
    "Lean.Parser.Term.paren", "Lean.Parser.Term.tuple",
    "Lean.Parser.Term.byTactic", "Lean.Parser.Tactic.tacticSeq",
    "Lean.Parser.Tactic.tacticSeq1Indented", "Lean.Parser.Tactic.tacticSorry",
    "Lean.Parser.Termination.suffix", "Lean.explicitBinders",
    "Lean.unbracketedExplicitBinders", "Lean.binderIdent", "num",
    "termℕ", "termℤ", "termℚ", "termℝ", "termℂ",
    "«term∃_,_»", "«term_=_»", "«term_+_»", "«term_-_»", "«term_*_»",
    "«term_/_»", "«term_^_»", "«term_<_»", "«term_≤_»", "«term_>_»",
    "«term_≥_»", "«term_∈_»", "«term_∧_»", "«term_∨_»",
    -- quantifiers with binder predicates: `∀ x ∈ S, P`, `∃ δ > 0, P`, ...
    "Lean.«term∀__,_»", "Lean.«term∃__,_»", "Mathlib.Notation.«term∃!_,_»",
    "Lean.«binderTerm∈_»", "Lean.«binderTerm>_»", "Lean.«binderTerm≥_»",
    "Lean.«binderTerm<_»",
    -- relations, connectives and pure algebraic notation
    "«term_≠_»", "«term¬_»", "«term_↔_»", "«term_∉_»", "«term_⊆_»", "«term_∣_»",
    "«term-_»", "«term_⁻¹»", "«term_•_»", "«term_∘_»", "«term_×_»"
  ]

private def absNotationKind : String := "«term|___|»"
private def setBuilderKind : String := "Mathlib.Meta.setBuilder"
private def extBinderKind : String := "Batteries.ExtendedBinder.extBinder"
private def projectionKind : String := "Lean.Parser.Term.proj"
private def groupKind : String := "group"

private def isAtomValue (stx : Syntax) (value : String) : Bool :=
  stx.isAtom && stx.getAtomVal == value

private def isNumericAtom (stx : Syntax) : Bool :=
  stx.isAtom && !stx.getAtomVal.isEmpty && stx.getAtomVal.all Char.isDigit

/-- Mathlib's `|a|` (`term|___|`) has exactly the children
`group["|"]`, the inner term, an empty `group`, and a closing `"|"`. -/
private def absBarsShape (stx : Syntax) : Bool :=
  stx.getNumArgs == 4 &&
    (stx.getArg 0).getKind.toString == groupKind && (stx.getArg 0).getNumArgs == 1 &&
    isAtomValue ((stx.getArg 0).getArg 0) "|" &&
    (stx.getArg 2).getKind.toString == groupKind && (stx.getArg 2).getNumArgs == 0 &&
    isAtomValue (stx.getArg 3) "|"

/-- Only numeric projections of a parenthesised or bare term, `(p).1` or `p.1`, are
admitted (both are `Term.proj` with a `fieldIdx`); named projections `(p).foo` are
rejected.  A dotted identifier such as `p.fst` is a plain identifier, not a `Term.proj`,
and is checked only by `blockedIdentifier?`. -/
private def numericProjection (stx : Syntax) : Bool :=
  stx.getNumArgs == 3 &&
    (stx.getArg 2).getKind.toString == "fieldIdx" &&
    (stx.getArg 2).getNumArgs == 1 &&
    isNumericAtom ((stx.getArg 2).getArg 0)

mutual
/-- The first syntax kind outside the admission grammar, if the tree is not admitted. -/
private partial def firstRejectedSyntax? (stx : Syntax) : Option String :=
  if stx.isAtom then
    if blockedAtoms.contains stx.getAtomVal then some stx.getKind.toString else none
  else if stx.isIdent then
    blockedIdentifier? stx.getId
  else if stx.isMissing then none
  else
    let kind := stx.getKind.toString
    if kind == absNotationKind then
      if absBarsShape stx then firstRejectedSyntax? (stx.getArg 1) else some kind
    else if kind == setBuilderKind then
      setBuilderRejected? stx
    else if kind == projectionKind then
      if numericProjection stx then firstRejectedSyntax? (stx.getArg 0) else some kind
    else if admittedKinds.contains kind then
      stx.getArgs.toList.findSome? firstRejectedSyntax?
    else
      some ((stx.getArgs.toList.findSome? firstRejectedSyntax?).getD kind)

/-- `{x | P}`, `{x ∈ S | P}` and typed `{x : T | P}`.  The typed form is the only
place a `group` may hold a type, and it is admitted solely as `group[":", T]`. -/
private partial def setBuilderRejected? (stx : Syntax) : Option String :=
  let binder := stx.getArg 1
  let wellFormed :=
    stx.getNumArgs == 5 && isAtomValue (stx.getArg 0) "{" &&
      isAtomValue (stx.getArg 2) "|" && isAtomValue (stx.getArg 4) "}" &&
      binder.getKind.toString == extBinderKind && binder.getNumArgs == 2
  if !wellFormed then
    some setBuilderKind
  else
    let ascription := binder.getArg 1
    let ascriptionRejected :=
      if ascription.getNumArgs == 1 && (ascription.getArg 0).getKind.toString == groupKind then
        let typed := ascription.getArg 0
        if typed.getNumArgs == 2 && isAtomValue (typed.getArg 0) ":" then
          firstRejectedSyntax? (typed.getArg 1)
        else
          some groupKind
      else
        firstRejectedSyntax? ascription
    firstRejectedSyntax? (binder.getArg 0) <|> ascriptionRejected <|>
      firstRejectedSyntax? (stx.getArg 3)
end

private partial def containsAtom (stx : Syntax) (value : String) : Bool :=
  (stx.isAtom && stx.getAtomVal == value) || stx.getArgs.any (containsAtom · value)

/-- Proof bodies may use Lean tactics outside the statement admission grammar, but they may not
introduce blocked identifiers or effects. The declaration still has to parse as one theorem. -/
private partial def firstBlockedProofSyntax? (stx : Syntax) : Option String :=
  if stx.isAtom then
    if blockedAtoms.contains stx.getAtomVal || blockedProofExternalAtoms.contains stx.getAtomVal then
      some stx.getKind.toString
    else none
  else if stx.isIdent then
    blockedIdentifier? stx.getId
  else if stx.isMissing then
    none
  else
    stx.getArgs.toList.findSome? firstBlockedProofSyntax?

private partial def countKind (stx : Syntax) (kind : SyntaxNodeKind) : Nat :=
  (if stx.getKind == kind then 1 else 0) +
    (stx.getArgs.foldl (fun count child => count + countKind child kind) 0)

private def isExactSorryBody (value : Syntax) : Bool :=
  value.getKind == ``Lean.Parser.Command.declValSimple &&
  value.getNumArgs >= 2 &&
  (value.getArg 1).getKind == ``Lean.Parser.Term.byTactic &&
  countKind value ``Lean.Parser.Term.byTactic == 1 &&
  countKind value ``Lean.Parser.Tactic.tacticSeq == 1 &&
  countKind value ``Lean.Parser.Tactic.tacticSeq1Indented == 1 &&
  countKind value ``Lean.Parser.Tactic.tacticSorry == 1 &&
  containsAtom value "by" && containsAtom value "sorry"

private def parseHeader (env : Environment) (source : String) (admission := false)
    (proofAdmission := false) : Except String ParsedHeader := do
  let command ← Parser.runParserCategory env `command source "<mls-declaration>"
  unless command.getKind == ``Lean.Parser.Command.declaration do
    throw "expected exactly one theorem or def declaration"
  let modifiers := command.getArg 0
  if modifiers.getArgs.any (fun arg => !arg.isNone) then
    throw "declaration modifiers are not supported"
  let decl := command.getArg 1
  let (kind, idIndex, sigIndex) ←
    if decl.getKind == ``Lean.Parser.Command.theorem then
      pure ("theorem", 1, 2)
    else if decl.getKind == ``Lean.Parser.Command.definition then
      pure ("definition", 1, 2)
    else
      throw "only theorem and def declarations are supported"
  let declId := decl.getArg idIndex
  let name := (declId.getArg 0).getId
  if name.isAnonymous then
    throw "the declaration must have a name"
  let sig := decl.getArg sigIndex
  let binderNode := sig.getArg 0
  let mut count := 0
  for binder in binderNode.getArgs do
    count := count + (← binderCount binder)
  if proofAdmission then
    unless kind == "theorem" do
      throw "proof checking requires one theorem declaration"
    let sig := decl.getArg sigIndex
    if let some rejected := firstRejectedSyntax? sig then
      throw s!"theorem statement contains unsupported syntax '{rejected}'"
    if let some rejected := firstBlockedProofSyntax? command then
      throw s!"proof declaration contains blocked syntax '{rejected}'"
    let value := decl.getArg 3
    unless value.getKind == ``Lean.Parser.Command.declValSimple &&
        value.getNumArgs >= 2 && (value.getArg 1).getKind == ``Lean.Parser.Term.byTactic do
      throw "proof must be a theorem with a tactic body"
    unless countKind command ``Lean.Parser.Term.byTactic == 1 do
      throw "tactics in a theorem signature are not supported"
  else if admission then
    if let some rejected := firstRejectedSyntax? command then
      throw s!"declaration contains unsupported syntax '{rejected}'"
  if admission && !proofAdmission && kind == "theorem" then
    let value := decl.getArg 3
    unless isExactSorryBody value do
      throw "theorem bodies must be the engine-injected 'by sorry' placeholder"
    unless countKind command ``Lean.Parser.Term.byTactic == 1 do
      throw "tactics in a theorem signature are not supported"
  else if admission && !proofAdmission && (containsAtom decl "by" || containsAtom decl "sorry") then
    -- The `sorry` keyword and `sorryAx` are both rejected: the keyword here, the
    -- constant by `blockedIdentifier?` in the walk above.
    throw "definition bodies cannot contain tactics or sorry"
  return { name, kind, binderCount := count }

private def binderInfoString : BinderInfo → String
  | .default => "explicit"
  | .implicit => "implicit"
  | .strictImplicit => "strictImplicit"
  | .instImplicit => "instance"

private def outerHead (expr : Expr) : String :=
  if expr.isForall then
    "forall"
  else
    let name? := expr.getAppFn.constName?
    if name? == some ``Exists then "exists"
    else if name? == some ``And then "and"
    else if name? == some ``Or then "or"
    else if name? == some ``Iff then "iff"
    else if name? == some ``Eq then "eq"
    else if name? == some ``False then "false"
    else if expr.isLambda then "lambda"
    else if expr.isLet then "let"
    else if expr.isApp then "app"
    else if expr.isConst then "constant"
    else if expr.isFVar then "free_variable"
    else if expr.isLit then "literal"
    else "atom"

private def sourceKindMatches (sourceKind : String) (ci : ConstantInfo) : Bool :=
  match sourceKind, ci with
  | "theorem", .thmInfo _ => true
  | "definition", .defnInfo _ => true
  | _, _ => false

private def declarationKind (ci : ConstantInfo) : String :=
  match ci with
  | .thmInfo _ => "theorem"
  | .defnInfo _ => "definition"
  | .axiomInfo _ => "axiom"
  | .opaqueInfo _ => "opaque"
  | _ => "unsupported"

private def extract (expectedName : Name) (source : String) : CommandElabM Json := do
  let env ← getEnv
  let header ← match parseHeader env source with
    | .ok header => pure header
    | .error message => throwError "invalid MLS declaration source: {message}"
  unless header.name == expectedName do
    throwError "source declares '{header.name}', but extraction requested '{expectedName}'"
  let ci ← getConstInfo expectedName
  unless sourceKindMatches header.kind ci do
    throwError "source kind '{header.kind}' does not match compiled declaration kind '{declarationKind ci}'"
  if ci.isUnsafe then
    throwError "unsafe declarations are not supported"
  let axioms ← collectAxioms expectedName
  liftTermElabM do
    forallBoundedTelescope ci.type header.binderCount fun xs goal => do
      unless xs.size == header.binderCount do
        throwError "compiled declaration has fewer binders than its parsed header"
      let binders ← xs.mapM fun x => do
        let decl ← x.fvarId!.getDecl
        let typeFmt ← ppExpr decl.type
        let isProposition ← isProp decl.type
        pure <| Json.mkObj [
          ("name", toJson decl.userName.eraseMacroScopes.toString),
          ("type", toJson typeFmt.pretty),
          ("is_proposition", toJson isProposition),
          ("binder_info", toJson (binderInfoString decl.binderInfo))]
      let goalFmt ← ppExpr goal
      let base := [
        ("declaration", toJson expectedName.toString),
        ("kind", toJson (declarationKind ci)),
        ("binders", toJson binders),
        ("goal", toJson goalFmt.pretty),
        ("goal_outer_head", toJson (outerHead goal)),
        ("axioms", toJson (axioms.map Name.toString)),
        ("has_sorry", toJson (axioms.contains ``sorryAx))]
      match ci with
      | .defnInfo info =>
          lambdaBoundedTelescope info.value header.binderCount fun bodyXs body => do
            unless bodyXs.size == header.binderCount do
              throwError "compiled definition body has fewer binders than its parsed header"
            let bodyFmt ← ppExpr body
            pure <| Json.mkObj (base ++ [
              ("body", toJson bodyFmt.pretty),
              ("body_outer_head", toJson (outerHead body))])
      | .thmInfo _ => pure <| Json.mkObj base
      | _ => throwError "unsupported compiled declaration kind"

syntax (name := mlsExtract) "#mls_extract " ident " from_source " str : command
syntax (name := mlsValidateSource) "#mls_validate_source " ident " from_source " str : command
syntax (name := mlsValidateProofSource) "#mls_validate_proof_source " ident " from_source " str : command

elab_rules : command
  | `(#mls_validate_source $id:ident from_source $source:str) => do
      let header ← match parseHeader (← getEnv) source.getString (admission := true) with
        | .ok header => pure header
        | .error message => throwError "invalid MLS declaration source: {message}"
      unless header.name == id.getId do
        throwError "source declares '{header.name}', but validation requested '{id.getId}'"
      let payload := Json.mkObj [
        ("declaration", toJson header.name.toString),
        ("kind", toJson header.kind),
        ("binder_count", toJson header.binderCount)]
      logInfo m!"MLS_VALIDATE:{payload.compress}"
  | `(#mls_validate_proof_source $id:ident from_source $source:str) => do
      let header ← match parseHeader (← getEnv) source.getString (admission := true) (proofAdmission := true) with
        | .ok header => pure header
        | .error message => throwError "invalid MLS proof source: {message}"
      unless header.name == id.getId do
        throwError "source declares '{header.name}', but validation requested '{id.getId}'"
      let payload := Json.mkObj [
        ("declaration", toJson header.name.toString),
        ("kind", toJson header.kind),
        ("binder_count", toJson header.binderCount)]
      logInfo m!"MLS_VALIDATE_PROOF:{payload.compress}"
  | `(#mls_extract $id:ident from_source $source:str) => do
      let payload ← extract id.getId source.getString
      logInfo m!"MLS_EXTRACT:{payload.compress}"

end MlsExtract
