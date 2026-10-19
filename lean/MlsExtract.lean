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

private partial def isAllowedAdmissionSyntax (stx : Syntax) : Bool :=
  if stx.isAtom || stx.isIdent || stx.isMissing then
    let value := if stx.isAtom then stx.getAtomVal else ""
    !["run_tac", "eval%", "unsafeIO", "do", "unsafe", "partial",
      "implemented_by", "macro", "syntax", "elab"].contains value
  else
  let kind := stx.getKind.toString
  let allowedKind := [
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
    "«term_≥_»", "«term_∈_»", "«term_∧_»", "«term_∨_»"
  ].contains kind
  allowedKind && stx.getArgs.all isAllowedAdmissionSyntax

private partial def firstRejectedSyntax? (stx : Syntax) : Option String := do
  if !isAllowedAdmissionSyntax stx then
    for child in stx.getArgs do
      match firstRejectedSyntax? child with
      | some rejected => return rejected
      | none => pure ()
    return stx.getKind.toString
  none

private partial def containsAtom (stx : Syntax) (value : String) : Bool :=
  (stx.isAtom && stx.getAtomVal == value) || stx.getArgs.any (containsAtom · value)

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

private def parseHeader (env : Environment) (source : String) (admission := false) : Except String ParsedHeader := do
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
  if admission && !isAllowedAdmissionSyntax command then
    throw s!"declaration contains unsupported syntax '{(firstRejectedSyntax? command).getD "unknown"}'"
  if admission && kind == "theorem" then
    let value := decl.getArg 3
    unless isExactSorryBody value do
      throw "theorem bodies must be the engine-injected 'by sorry' placeholder"
    unless countKind command ``Lean.Parser.Term.byTactic == 1 do
      throw "tactics in a theorem signature are not supported"
  else if admission && (containsAtom decl "by" || containsAtom decl "sorry") then
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
  | `(#mls_extract $id:ident from_source $source:str) => do
      let payload ← extract id.getId source.getString
      logInfo m!"MLS_EXTRACT:{payload.compress}"

end MlsExtract
