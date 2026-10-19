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


if __name__ == "__main__":
    unittest.main(verbosity=2)
