#!/usr/bin/env python3
"""Real persistent-REPL tests for the trusted MLS Lean extractor."""

from __future__ import annotations

import json
import os
from pathlib import Path
import select
import subprocess
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
SHADOW = ROOT / ".mls"
REPL = ROOT / "scripts/run-lean-repl.sh"
TOOLCHAIN = ROOT / ".toolchains/lean-4.19.0-darwin_aarch64/bin"
FIXTURES = json.loads((ROOT / "tests/fixtures/lean/declarations.json").read_text())
PREFIX = "MLS_EXTRACT:"
VALIDATE_PREFIX = "MLS_VALIDATE:"


class Repl:
    def __init__(self) -> None:
        env = os.environ.copy()
        env["PATH"] = f"{TOOLCHAIN}:{env.get('PATH', '')}"
        env["MATHLIB_CACHE_DIR"] = str(ROOT / ".toolchains/mathlib-cache")
        self.proc = subprocess.Popen(
            [str(REPL)], cwd=SHADOW, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            bufsize=0,
        )
        self.buffer = bytearray()

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            self.proc.wait(timeout=10)

    def request(self, request: dict, timeout: float = 45.0) -> dict:
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write((json.dumps(request) + "\n\n").encode())
        self.proc.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            boundary = self.buffer.find(b"\n\n")
            if boundary >= 0:
                frame = bytes(self.buffer[:boundary])
                del self.buffer[:boundary + 2]
                return json.loads(frame)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"REPL timed out waiting for {request!r}")
            ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
            if not ready:
                raise TimeoutError(f"REPL timed out waiting for {request!r}")
            chunk = os.read(self.proc.stdout.fileno(), 65536)
            if not chunk:
                stderr = self.proc.stderr.read().decode(errors="replace") if self.proc.stderr else ""
                raise EOFError(f"REPL exited unexpectedly: {stderr}")
            self.buffer.extend(chunk)


def lean_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def extraction_command(name: str, source: str) -> str:
    return f"{source}\n#mls_extract {name} from_source {lean_string(source)}"


def payloads(response: dict) -> list[dict]:
    found = []
    for message in response.get("messages", []):
        data = message.get("data", "")
        if data.startswith(PREFIX):
            found.append(json.loads(data[len(PREFIX):]))
    return found


def validation_payloads(response: dict) -> list[dict]:
    found = []
    for message in response.get("messages", []):
        data = message.get("data", "")
        if data.startswith(VALIDATE_PREFIX):
            found.append(json.loads(data[len(VALIDATE_PREFIX):]))
    return found


def errors(response: dict) -> list[dict]:
    return [m for m in response.get("messages", []) if m.get("severity") == "error"]


class ExtractorIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not REPL.is_file() or not (SHADOW / ".lake/build/lib/lean/MlsExtract.olean").is_file():
            raise RuntimeError("run scripts/setup-lean.sh before the required native Lean tests")
        cls.repl = Repl()
        init = cls.repl.request(
            {"cmd": "import Mathlib\nimport MlsExtract\nset_option autoImplicit false"},
            timeout=180.0,
        )
        if errors(init) or "env" not in init:
            raise AssertionError(init)
        cls.base_env = init["env"]

    @classmethod
    def tearDownClass(cls) -> None:
        cls.repl.close()

    def extract(self, source: str, name: str, env: int | None = None) -> tuple[dict, dict]:
        response = self.repl.request({
            "env": self.base_env if env is None else env,
            "cmd": extraction_command(name, source),
        })
        self.assertEqual(errors(response), [], response)
        found = payloads(response)
        self.assertEqual(len(found), 1, response)
        self.assertEqual(found[0]["declaration"], name)
        return response, found[0]

    def validate(self, source: str, name: str, env: int | None = None) -> dict:
        response = self.repl.request({
            "env": self.base_env if env is None else env,
            "cmd": f"#mls_validate_source {name} from_source {lean_string(source)}",
        })
        self.assertEqual(errors(response), [], response)
        found = validation_payloads(response)
        self.assertEqual(len(found), 1, response)
        self.assertEqual(found[0]["declaration"], name)
        return response

    def validate_proof(self, source: str, name: str) -> dict:
        response = self.repl.request({
            "env": self.base_env,
            "cmd": f"#mls_validate_proof_source {name} from_source {lean_string(source)}",
        })
        self.assertEqual(errors(response), [], response)
        found = [
            json.loads(m["data"][len("MLS_VALIDATE_PROOF:"):])
            for m in response.get("messages", [])
            if m.get("data", "").startswith("MLS_VALIDATE_PROOF:")
        ]
        self.assertEqual(len(found), 1, response)
        self.assertEqual(found[0]["declaration"], name)
        return response

    def reject_proof(self, source: str, name: str, fragment: str) -> None:
        response = self.repl.request({
            "env": self.base_env,
            "cmd": f"#mls_validate_proof_source {name} from_source {lean_string(source)}",
        })
        self.assertTrue(errors(response), response)
        self.assertFalse(any(
            m.get("data", "").startswith("MLS_VALIDATE_PROOF:")
            for m in response.get("messages", [])
        ), response)
        messages = "\n".join(m.get("data", "") for m in errors(response))
        self.assertIn(fragment, messages, response)

    def test_persistent_dependencies_and_transitive_sorry(self) -> None:
        first, base = self.extract(FIXTURES["sorry_base"], "sorry_base")
        self.assertTrue(base["has_sorry"])
        self.assertIn("sorryAx", base["axioms"])
        second, dependent = self.extract(FIXTURES["dependent"], "dependent", first["env"])
        self.assertNotEqual(first["env"], second["env"])
        self.assertTrue(dependent["has_sorry"])
        _, trusted = self.extract(FIXTURES["trusted"], "trusted", second["env"])
        self.assertFalse(trusted["has_sorry"])

    def test_header_boundary_and_binder_classes(self) -> None:
        real_source = "theorem real_reflexive (x : ℝ) : x = x := by sorry"
        self.validate(real_source, "real_reflexive")
        square_source = "theorem mls_square (x : Real) : 0 ≤ x ^ 2 := by sorry"
        self.validate(square_source, "mls_square")
        bounded_source = (
            "def mls_bounded (f : Real → Real) (D : Set Real) : Prop := "
            "∃ M : Real, ∀ x : Real, x ∈ D → abs (f x) ≤ M"
        )
        self.validate(bounded_source, "mls_bounded")
        validation = self.validate(FIXTURES["shape"], "shape")
        self.assertNotEqual(validation["env"], self.base_env)
        _, data = self.extract(FIXTURES["shape"], "shape")
        self.assertEqual(data["kind"], "theorem")
        self.assertEqual(data["goal_outer_head"], "forall")
        self.assertEqual([b["name"] for b in data["binders"]], ["α", "f", "P", "h", "inst"])
        self.assertEqual([b["binder_info"] for b in data["binders"]],
                         ["implicit", "explicit", "explicit", "explicit", "instance"])
        self.assertEqual([b["is_proposition"] for b in data["binders"]],
                         [False, False, False, True, True])

    def test_definition_body_and_function_result_are_separate(self) -> None:
        numeric = "def numeric (n : Nat) : Nat := n + 1"
        self.validate(numeric, "numeric")
        self.validate(FIXTURES["function_definition"], "function_definition")
        _, data = self.extract(FIXTURES["function_definition"], "function_definition")
        self.assertEqual(data["kind"], "definition")
        self.assertEqual(data["goal_outer_head"], "forall")
        self.assertEqual(data["body_outer_head"], "lambda")
        self.assertIn("n +", data["body"])

    def test_source_admission_and_repair_from_prior_environment(self) -> None:
        bad_source = "theorem repaired (n : Nat) : n = n := by exact missing_name"
        failed = self.repl.request({
            "env": self.base_env,
            "cmd": extraction_command("repaired", bad_source),
        })
        self.assertTrue(errors(failed), failed)
        good_source = "theorem repaired (n : Nat) : n = n := by rfl"
        _, repaired = self.extract(good_source, "repaired", self.base_env)
        self.assertFalse(repaired["has_sorry"])

        appended = good_source + "\naxiom spoof : False"
        rejected = self.repl.request({
            "env": self.base_env,
            "cmd": f"{good_source}\n#mls_extract repaired from_source {lean_string(appended)}",
        })
        self.assertTrue(errors(rejected), rejected)
        self.assertEqual(payloads(rejected), [])

        wrong_kind = "axiom forbidden : False"
        rejected_kind = self.repl.request({
            "env": self.base_env,
            "cmd": f"{wrong_kind}\n#mls_extract forbidden from_source {lean_string(wrong_kind)}",
        })
        self.assertTrue(errors(rejected_kind), rejected_kind)
        self.assertEqual(payloads(rejected_kind), [])

        executable_type = (
            "theorem effectful (n : Nat) "
            ": (by run_tac Lean.logInfo \"not executed\"; exact Nat) := by sorry"
        )
        parse_only = self.repl.request({
            "env": self.base_env,
            "cmd": f"#mls_validate_source effectful from_source {lean_string(executable_type)}",
        })
        self.assertTrue(errors(parse_only), parse_only)
        self.assertEqual(validation_payloads(parse_only), [])

        eval_definition = "def evil : Nat := eval% unsafeIO (pure 0)"
        parse_only_eval = self.repl.request({
            "env": self.base_env,
            "cmd": f"#mls_validate_source evil from_source {lean_string(eval_definition)}",
        })
        self.assertTrue(errors(parse_only_eval), parse_only_eval)
        self.assertEqual(validation_payloads(parse_only_eval), [])

        admission_regressions = [
            good_source + "\n#eval 1",
            "theorem wrapped : True := (fun h => h) (by sorry)",
            "theorem duplicate : True := by sorry; sorry",
        ]
        for source in admission_regressions:
            name = "repaired" if source.startswith("theorem repaired") else (
                "wrapped" if source.startswith("theorem wrapped") else "duplicate"
            )
            response = self.repl.request({
                "env": self.base_env,
                "cmd": f"#mls_validate_source {name} from_source {lean_string(source)}",
            })
            self.assertTrue(errors(response), response)
            self.assertEqual(validation_payloads(response), [])

    def test_proof_admission_blocks_network_search_before_execution(self) -> None:
        # The validator only parses candidate source. These pinned Mathlib tactic
        # forms must be rejected here, before their tactic elaborators can run.
        cases = [
            ("proof_loogle", "#loogle Nat.add_comm",
             "proof declaration contains blocked syntax '#loogle'"),
            ("proof_leansearch", '#leansearch "Commutativity of addition."',
             "proof declaration contains blocked syntax '#leansearch'"),
            ("proof_moogle", '#moogle "Commutativity of addition."',
             "proof declaration contains blocked syntax '#moogle'"),
            ("proof_statesearch", "#statesearch",
             "proof declaration contains blocked syntax '#statesearch'"),
            ("proof_search", '#search "Commutativity of addition."',
             "proof declaration contains blocked syntax '#search'"),
            ("proof_polyrith", "polyrith",
             "proof declaration contains blocked syntax 'polyrith'"),
        ]
        for name, tactic, blocked in cases:
            with self.subTest(tactic=tactic):
                source = f"theorem {name} (n m : ℕ) : n + m = m + n := by {tactic}"
                self.reject_proof(source, name, blocked)

        # Local library search remains an allowed proof tactic.
        self.validate_proof(
            "theorem proof_exact_search (n m : ℕ) : n + m = m + n := by exact?",
            "proof_exact_search",
        )
        self.validate_proof(
            "theorem proof_apply_search (n m : ℕ) : n + m = m + n := by apply?",
            "proof_apply_search",
        )

    def reject(self, name: str, source: str, fragment: str | None = None) -> None:
        response = self.repl.request({
            "env": self.base_env,
            "cmd": f"#mls_validate_source {name} from_source {lean_string(source)}",
        })
        self.assertTrue(errors(response), response)
        self.assertEqual(validation_payloads(response), [], response)
        if fragment is not None:
            messages = "\n".join(m.get("data", "") for m in errors(response))
            self.assertIn(fragment, messages, response)

    def test_notation_admitted_and_extracted_with_goal_heads(self) -> None:
        # Each case: (case, declaration name, source, expected goal_outer_head).
        for case, name, source, head in NOTATION_CASES:
            with self.subTest(case=case):
                self.validate(source, name)
                _, data = self.extract(source, name)
                self.assertEqual(data["kind"], "theorem")
                self.assertEqual(data["goal_outer_head"], head)
                self.assertTrue(data["has_sorry"])

    def test_definition_body_with_absolute_value_is_admitted(self) -> None:
        source = "def mls_def_abs (x : ℝ) : ℝ := |x|"
        self.validate(source, "mls_def_abs")
        _, data = self.extract(source, "mls_def_abs")
        self.assertEqual(data["kind"], "definition")
        self.assertEqual(data["goal_outer_head"], "constant")
        self.assertEqual(data["body_outer_head"], "app")

    def test_group_is_rejected_outside_abs_and_typed_set_builder(self) -> None:
        # `⋃ x : ℕ, S x` produces a `group` around its type.  Only `|a|` and typed
        # set-builder binders may contain one, so this must fail as `group`.
        self.reject(
            "mls_group_union",
            "theorem mls_group_union (S : ℕ → Set ℝ) (T : Set ℝ) : "
            "(⋃ x : ℕ, S x) = T := by sorry",
            "unsupported syntax 'group'",
        )
        # The typed set-builder itself is admitted (see NOTATION_CASES), but a
        # tactic inside its binder type stays rejected.
        self.reject(
            "mls_setb_tac",
            "theorem mls_setb_tac (S : Set ℝ) : {x : ℝ | by exact True} = S := by sorry",
            "unsupported syntax 'Lean.Parser.Tactic.exact'",
        )

    def test_effectful_and_out_of_context_syntax_stays_rejected(self) -> None:
        cases = [
            ("mls_run", "theorem mls_run (n : ℕ) : n = n := by simp", None),
            ("mls_macro", 'macro "mls_x" : term => `(0)', "expected exactly one"),
            ("mls_tac_sig", "theorem mls_tac_sig : (by exact 0 : ℕ) = 0 := by sorry",
             "unsupported syntax 'Lean.Parser.Tactic.exact'"),
            ("mls_show", "theorem mls_show : (show (0:ℕ) = 0 from by rfl) := by sorry", None),
            ("mls_default", "theorem mls_default (d : Bool := by exact true) : True := by sorry",
             "unsupported syntax 'Lean.Parser.Tactic.exact'"),
            ("mls_abs_tac", "theorem mls_abs_tac (x : ℝ) : |(by exact x : ℝ)| = x := by sorry",
             "unsupported syntax 'Lean.Parser.Tactic.exact'"),
            ("mls_def_tac", "def mls_def_tac (x : ℝ) : ℝ := (by exact x)",
             "unsupported syntax 'Lean.Parser.Tactic.exact'"),
            # Named projections on a parenthesised term are not admitted; numeric ones are.
            ("mls_named", "theorem mls_named (p : ℝ × ℝ) : (p).fst = (p).snd := by sorry",
             "unsupported syntax 'Lean.Parser.Term.proj'"),
            ("mls_subtype", "theorem mls_subtype (P : ℝ → Prop) (S : Set ℝ) : "
             "{x : ℝ // P x} = S := by sorry", "unsupported syntax '«term{_:_//_}»'"),
        ]
        for name, source, fragment in cases:
            with self.subTest(case=name):
                self.reject(name, source, fragment)

    def test_deriving_clause_is_rejected(self) -> None:
        # A deriving handler is an executable elaborator, so the clause must be
        # rejected at validation, before the source is elaborated.
        self.reject("e3", "def e3 : Nat := 0 deriving Repr, BEq", "unsupported syntax 'deriving'")
        self.reject("e4", "def e4 : Nat := 0 deriving Repr", "unsupported syntax 'deriving'")

    def test_unsafe_identifiers_are_rejected(self) -> None:
        # Identifiers are checked component by component; each case below is a
        # reproducer that was admitted before the identifier denylist existed.
        cases = [
            ("m9", "def m9 : Nat := sorryAx Nat true", "unsupported syntax 'sorryAx'"),
            ("m9_at", "def m9_at : Nat := @sorryAx Nat true", "unsupported syntax 'sorryAx'"),
            ("m9_qualified", "def m9_q : Nat := Lean.sorryAx Nat true", "unsupported syntax 'sorryAx'"),
            ("mls_nd", "def mls_nd : Bool := native_decide (2 = 2)",
             "unsupported syntax 'native_decide'"),
            ("mls_rt", "def mls_rt : Unit := run_tac pure ()", "unsupported syntax 'run_tac'"),
            ("mls_ofrb", "def mls_ofrb (a b : Bool) (h : a = b) : Bool := Lean.ofReduceBool a b h",
             "unsupported syntax 'ofReduceBool'"),
            ("mls_ofrn", "def mls_ofrn (a b : Nat) (h : a = b) : Nat := Lean.ofReduceNat a b h",
             "unsupported syntax 'ofReduceNat'"),
            ("mls_uc", "def mls_uc : Nat := unsafeCast 0", "unsupported syntax 'unsafeCast'"),
            ("mls_io", "def mls_io : Nat := IO.println", "unsupported syntax 'IO'"),
            ("mls_dbg", 'def mls_dbg : Nat := dbgTrace "x" fun _ => 0', "unsupported syntax 'dbgTrace'"),
            ("mls_tc", "def mls_tc : Nat := Lean.trustCompiler 0", "unsupported syntax 'trustCompiler'"),
            ("mls_panic", 'def mls_panic : Nat := panic! "x"', "unsupported syntax 'panic!'"),
        ]
        for name, source, fragment in cases:
            with self.subTest(case=name):
                self.reject(name, source, fragment)

    def test_classical_choice_remains_admitted(self) -> None:
        # A theorem signature (not a computable body, which would need `noncomputable`).
        source = ("theorem mls_choice (α : Type) (h : Nonempty α) : "
                  "Classical.choice h = Classical.choice h := by sorry")
        self.validate(source, "mls_choice")
        _, data = self.extract(source, "mls_choice")
        self.assertEqual(data["kind"], "theorem")
        self.assertIn("Classical.choice", data["goal"])
        self.assertIn("Classical.choice", data["axioms"])


NOTATION_CASES = [
    ("abs_le", "mls_abs_le", "theorem mls_abs_le (x : ℝ) : |x| ≤ 1 := by sorry", "app"),
    ("abs_triangle", "mls_triangle",
     "theorem mls_triangle (x y : ℝ) : |x + y| ≤ |x| + |y| := by sorry", "app"),
    ("abs_bounded_sequence", "mls_bounded",
     "theorem mls_bounded (u : ℕ → ℝ) : ∃ M : ℝ, ∀ n : ℕ, |u n| ≤ M := by sorry", "exists"),
    ("abs_parenthesised", "mls_abs_paren",
     "theorem mls_abs_paren (x : ℝ) : |(x + 1)| ≤ 2 := by sorry", "app"),
    ("forall_membership", "mls_forall_mem",
     "theorem mls_forall_mem (S : Set ℝ) (P : ℝ → Prop) : ∀ x ∈ S, P x := by sorry", "forall"),
    ("exists_membership", "mls_exists_mem",
     "theorem mls_exists_mem (S : Set ℝ) (P : ℝ → Prop) : ∃ x ∈ S, P x := by sorry", "exists"),
    ("forall_positive", "mls_forall_pos",
     "theorem mls_forall_pos (P : ℕ → Prop) : ∀ ε > 0, P ε := by sorry", "forall"),
    ("exists_positive", "mls_exists_pos",
     "theorem mls_exists_pos (P : ℕ → Prop) : ∃ δ > 0, P δ := by sorry", "exists"),
    ("forall_at_least", "mls_forall_ge",
     "theorem mls_forall_ge (a : ℝ) (P : ℝ → Prop) : ∀ x ≥ a, P x := by sorry", "forall"),
    ("exists_less_than", "mls_exists_lt",
     "theorem mls_exists_lt (m : ℕ) (P : ℕ → Prop) : ∃ n < m, P n := by sorry", "exists"),
    ("extremum_in_interval", "mls_icc",
     "theorem mls_icc (f : ℝ → ℝ) (a b : ℝ) : "
     "∃ c ∈ Set.Icc a b, ∀ x ∈ Set.Icc a b, f x ≤ f c := by sorry", "exists"),
    ("exists_two_binders", "mls_exists_two",
     "theorem mls_exists_two (P : ℕ → ℕ → Prop) : ∃ x y, P x y := by sorry", "exists"),
    ("exists_typed_binder", "mls_exists_typed",
     "theorem mls_exists_typed (P : ℝ → Prop) : ∃ x : ℝ, P x := by sorry", "exists"),
    ("exists_unique", "mls_unique",
     "theorem mls_unique (P : ℝ → Prop) : ∃! x, P x := by sorry", "app"),
    ("not_equal", "mls_ne", "theorem mls_ne (a b : ℝ) : a ≠ b := by sorry", "app"),
    ("negation", "mls_not", "theorem mls_not (P : Prop) : ¬ P := by sorry", "app"),
    ("iff", "mls_iff", "theorem mls_iff (P Q : Prop) : P ↔ Q := by sorry", "iff"),
    ("arrow", "mls_arrow", "theorem mls_arrow (P Q : Prop) : P → Q := by sorry", "forall"),
    ("not_member", "mls_notin", "theorem mls_notin (x : ℝ) (S : Set ℝ) : x ∉ S := by sorry", "app"),
    ("subset", "mls_subset", "theorem mls_subset (S T : Set ℝ) : S ⊆ T := by sorry", "app"),
    ("divides", "mls_dvd", "theorem mls_dvd (d n : ℕ) : d ∣ n := by sorry", "app"),
    ("set_builder", "mls_setb",
     "theorem mls_setb (P : ℝ → Prop) (S : Set ℝ) : {x | P x} = S := by sorry", "eq"),
    ("typed_set_builder", "mls_setb_typed",
     "theorem mls_setb_typed (P : ℝ → Prop) (S : Set ℝ) : {x : ℝ | P x} = S := by sorry", "eq"),
    ("membership_set_builder", "mls_setb_mem",
     "theorem mls_setb_mem (S T : Set ℝ) (P : ℝ → Prop) : {x ∈ S | P x} = T := by sorry", "eq"),
    ("typed_set_builder_abs", "mls_setb_abs",
     "theorem mls_setb_abs (S : Set ℝ) : {x : ℝ | |x| ≤ 1} = S := by sorry", "eq"),
    ("negation_of_term", "mls_neg", "theorem mls_neg (x y : ℝ) : -x = y := by sorry", "eq"),
    ("inverse", "mls_inv", "theorem mls_inv (x y : ℝ) : x⁻¹ = y := by sorry", "eq"),
    ("scalar_multiplication", "mls_smul", "theorem mls_smul (c v w : ℝ) : c • v = w := by sorry", "eq"),
    ("composition", "mls_comp", "theorem mls_comp (f g h : ℝ → ℝ) : f ∘ g = h := by sorry", "eq"),
    ("numeric_projection", "mls_proj",
     "theorem mls_proj (p : ℝ × ℝ) : p.1 = p.2 := by sorry", "eq"),
    ("parenthesised_numeric_projection", "mls_paren_num",
     "theorem mls_paren_num (p : ℝ × ℝ) : (p).1 = (p).2 := by sorry", "eq"),
    ("product_type", "mls_prod",
     "theorem mls_prod (α β : Type) : (α × β) = (α × β) := by sorry", "eq"),
    ("tendsto", "mls_tendsto",
     "theorem mls_tendsto (u : ℕ → ℝ) : Filter.Tendsto u Filter.atTop (nhds 0) := by sorry", "app"),
    ("function_type", "mls_fin", "theorem mls_fin (v : Fin 2 → ℝ) : v = v := by sorry", "eq"),
]


if __name__ == "__main__":
    unittest.main(verbosity=2)
