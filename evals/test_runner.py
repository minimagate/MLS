"""Offline metric, invocation, and immutable-history checks for the evaluator."""

import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as runner


def sir(label="thm:a", conclusion="A"):
    return {
        "kind": "theorem", "label": label, "variables": [], "hypotheses": [],
        "conclusion": conclusion, "uses": [], "assumptions_made": [],
    }


def realistic_inputs():
    expected_a = sir()
    expected_b = sir("thm:b", "B")
    expected_c = sir("thm:c", "C")
    corpus = {"cases": [
        {"id": "a", "block_id": "a-id", "expected_sir": expected_a},
        {"id": "b", "block_id": "b-id", "expected_sir": expected_b},
        {"id": "c", "block_id": "c-id", "expected_sir": expected_c},
        {"id": "d", "block_id": "d-id", "expected_sir": sir("thm:d", "D")},
    ]}
    report = {
        "diagnostics": [],
        "blocks": [
            {"block_id": "a-id", "label": "thm:a", "status": "sir_drafted",
             "sir": expected_a, "elaborated": True, "complete_structure": True,
             "accepted_speculatively": True, "checks": [{"hard": True, "state": "matched"}], "errors": []},
            {"block_id": "b-id", "label": "thm:b", "status": "sir_drafted",
             "sir": sir("thm:b", "B reworded"), "elaborated": True, "complete_structure": False,
             "accepted_speculatively": True, "checks": [{"hard": False, "state": "unchecked"}], "errors": []},
            {"block_id": "c-id", "label": "thm:c", "status": "formalization_error",
             "sir": expected_c, "elaborated": True, "complete_structure": False,
             "accepted_speculatively": False, "checks": [{"hard": True, "state": "mismatch"}],
             "errors": ["conclusion mismatch"]},
            {"block_id": "d-id", "label": "thm:d", "status": "not_formalized",
             "sir": None, "elaborated": False, "complete_structure": False,
             "accepted_speculatively": False, "checks": [],
             "errors": [runner.BLOCKED_DEPENDENCY]},
        ],
        "usage": {"calls": 5, "input_tokens": 100, "output_tokens": 50,
                  "known_cost": 0.12, "unknown_cost_calls": 2,
                  "unknown_input_token_calls": 1, "unknown_output_token_calls": 2},
    }
    return corpus, report


def leaf_values(value):
    """Yield every scalar inside a JSON-like value, however deeply it is nested."""
    if isinstance(value, dict):
        for item in value.values():
            yield from leaf_values(item)
    elif isinstance(value, list):
        for item in value:
            yield from leaf_values(item)
    else:
        yield value


def run_with_report(corpus, report):
    """Run the real evaluator path against a fake CLI; return (cases bytes, history)."""
    temporary = tempfile.TemporaryDirectory()
    root = Path(temporary.name)
    try:
        source = root / "notes.tm"
        source.write_text("source")
        cases = root / "cases.json"
        cases.write_text(json.dumps(corpus), encoding="utf-8")
        results = root / "results"

        def fake_batch(command, **kwargs):
            Path(command[command.index("--output") + 1]).write_text(json.dumps(report))
            return subprocess.CompletedProcess(command, 0)

        arguments = ["--replay", str(root / "recordings"), "--mls", sys.executable,
                     "--source", str(source), "--cases", str(cases),
                     "--output-directory", str(results)]
        with patch.object(runner.subprocess, "run", side_effect=fake_batch):
            with contextlib.redirect_stdout(io.StringIO()):
                _, summary_path = runner.run(arguments)
        return cases.read_bytes(), json.loads(summary_path.read_text(encoding="utf-8"))
    finally:
        temporary.cleanup()


class SummaryTests(unittest.TestCase):
    def test_failure_categories_are_exclusive_and_account_for_every_nonaccepted_case(self):
        corpus, report = realistic_inputs()
        template = report["blocks"][2]
        samples = [
            ("sir_wrong", {"errors": ["SIR kind differs from tagged source kind"]}),
            ("structure_check_mismatch", {}),
            ("unknown_or_wrong_mathlib_name", {
                "checks": [], "errors": ["Lean: unknown identifier 'Mathlib.Missing'"]}),
            ("type_error", {
                "checks": [], "errors": ["Lean: application type mismatch\nexpected Nat"]}),
            ("other", {"checks": [], "errors": [runner.BLOCKED_DEPENDENCY]}),
            ("other", {"checks": [], "errors": []}),
        ]
        corpus["cases"] = [corpus["cases"][0]]
        report["blocks"] = [report["blocks"][0]]
        for index, (category, fields) in enumerate(samples):
            block_id = f"failure-{index}"
            label = f"thm:failure-{index}"
            corpus["cases"].append({"id": block_id, "block_id": block_id,
                                    "expected_sir": sir(label)})
            block = dict(template, block_id=block_id, label=label, **fields)
            report["blocks"].append(block)
        result = runner.summarize(corpus, report)
        failures = result["failures"]
        self.assertEqual(failures["taxonomy_version"], runner.FAILURE_TAXONOMY_VERSION)
        self.assertEqual([case["category"] for case in failures["cases"]],
                         [category for category, _ in samples])
        self.assertEqual(failures["category_counts"], {
            "sir_wrong": 1, "structure_check_mismatch": 1,
            "unknown_or_wrong_mathlib_name": 1, "type_error": 1, "other": 2,
        })
        self.assertEqual(sum(failures["category_counts"].values()), failures["count"])
        self.assertEqual(failures["count"] + result["accepted_unconfirmed"]["count"],
                         len(corpus["cases"]))
        self.assertEqual(len({case["block_id"] for case in failures["cases"]}),
                         failures["count"])
        self.assertEqual(result, runner.summarize(corpus, report))

    def test_failure_category_precedence_and_explicit_sir_evidence(self):
        block = {
            "errors": ["Lean: unknown identifier 'Real.Missing'"],
            "lean_diagnostics": [{"message": "application type mismatch\nexpected Nat"}],
            "checks": [{"hard": True, "scope": "structure", "state": "mismatch"}],
        }
        self.assertEqual(runner.failure_category(block), "structure_check_mismatch")
        for error in (
            "SIR label differs from source label",
            "SIR uses differ from deterministically linked dependency labels",
            'SIR variable name "match" cannot be preserved as a generated Lean binder',
            "SIR variable h0 conflicts with the canonical hypothesis binder",
            "the regenerated SIR equals the rejected SIR",
        ):
            with self.subTest(error=error):
                self.assertEqual(runner.failure_category(dict(block, errors=[error])), "sir_wrong")
        block["checks"] = []
        self.assertEqual(runner.failure_category(block), "unknown_or_wrong_mathlib_name")
        block["errors"] = []
        self.assertEqual(runner.failure_category(block), "type_error")

    def test_lean_diagnostics_classify_names_and_types_without_matching_arbitrary_prose(self):
        samples = [
            ("Unknown constant `Real.notInMathlib`", "unknown_or_wrong_mathlib_name"),
            ("unknown namespace 'NotMathlib'", "unknown_or_wrong_mathlib_name"),
            ("invalid field 'missing'", "unknown_or_wrong_mathlib_name"),
            ("type mismatch\n  x\nhas type Nat but is expected to have type Bool", "type_error"),
            ("application argument type mismatch\n  x", "type_error"),
            ("function expected at\n  x", "type_error"),
            ("failed to synthesize\n  Add Foo", "type_error"),
            ("invalid MLS declaration source: declaration contains unsupported syntax 'group'",
             "other"),
        ]
        for message, category in samples:
            with self.subTest(message=message):
                for block in ({"lean_diagnostics": [{"message": message}]},
                              {"errors": [f"Lean: line 1, column 0: {message}"]}):
                    self.assertEqual(runner.failure_category(block), category)
        for message in ("unknown identifier in provider response", "type mismatch in source",
                        "invalid provider response: completion did not finish normally: length"):
            self.assertEqual(runner.failure_category({"errors": [message]}), "other")
        self.assertEqual(runner.failure_category({"checks": [
            {"scope": "semantics", "hard": True, "state": "mismatch"}]}), "other")

    def test_reference_wording_and_missing_sir_never_imply_sir_wrong(self):
        corpus, report = realistic_inputs()
        # The same observed failure keeps its category when the private oracle changes.
        baseline = runner.summarize(corpus, report)["failures"]
        changed = copy.deepcopy(corpus)
        for case in changed["cases"]:
            case["expected_sir"]["conclusion"] = "A differently worded reference"
        self.assertEqual(runner.summarize(changed, report)["failures"], baseline)
        self.assertEqual(runner.failure_category({"sir": None, "errors": [
            "invalid provider response: completion did not finish normally: length"]}), "other")

    def test_no_failures_keeps_all_zero_categories(self):
        corpus, report = realistic_inputs()
        corpus["cases"] = corpus["cases"][:1]
        report["blocks"] = report["blocks"][:1]
        failures = runner.summarize(corpus, report)["failures"]
        self.assertEqual(failures["count"], 0)
        self.assertEqual(failures["cases"], [])
        self.assertEqual(failures["category_counts"], dict.fromkeys(runner.FAILURE_CATEGORIES, 0))

    def test_exact_details_omit_reference_hashes_and_withhold_generated_hash_on_match(self):
        corpus, report = realistic_inputs()
        details = {item["case_id"]: item for item in
                   runner.summarize(corpus, report)["exact_canonical_sir_details"]}
        for item in details.values():
            self.assertNotIn("reference_sha256", item)
            self.assertIn("match", item)
        self.assertTrue(details["a"]["match"])
        self.assertNotIn("generated_sha256", details["a"])
        self.assertFalse(details["b"]["match"])
        self.assertEqual(details["b"]["generated_sha256"],
                         runner.canonical_hash(sir("thm:b", "B reworded")))

    def test_history_records_cases_file_hash_and_no_reference_hashes(self):
        corpus, report = realistic_inputs()
        cases_bytes, history = run_with_report(corpus, report)
        self.assertEqual(history["cases_sha256"], hashlib.sha256(cases_bytes).hexdigest())
        self.assertNotIn("reference_sha256", json.dumps(history))
        self.assertTrue(history["complete"])

    def test_no_summary_value_equals_a_reference_canonical_hash(self):
        corpus, report = realistic_inputs()
        # Case e's reference differs from case a's, but its draft is identical to a's
        # reference; that generated hash must not reveal a's reference hash either.
        corpus["cases"].append({"id": "e", "block_id": "e-id", "expected_sir": sir("thm:e", "E")})
        report["blocks"].append({
            "block_id": "e-id", "label": "thm:e", "status": "sir_drafted", "sir": sir(),
            "elaborated": True, "complete_structure": False, "accepted_speculatively": False,
            "checks": [], "errors": [],
        })
        _, history = run_with_report(corpus, report)
        reference_hashes = {runner.canonical_hash(case["expected_sir"]) for case in corpus["cases"]}
        for value in leaf_values(history):
            self.assertNotIn(value, reference_hashes)
        details = {item["case_id"]: item for item in history["summary"]["exact_canonical_sir_details"]}
        self.assertNotIn("generated_sha256", details["e"])
        self.assertFalse(details["e"]["match"])

    def test_unmapped_successful_block_does_not_change_any_numerator(self):
        corpus, report = realistic_inputs()
        baseline = runner.summarize(corpus, report)
        extra = {"block_id": "extra-id", "label": "thm:extra", "status": "sir_drafted",
                 "sir": sir("thm:extra", "E"), "elaborated": True, "complete_structure": True,
                 "accepted_speculatively": True, "checks": [{"hard": False, "state": "matched"}],
                 "errors": []}
        extended = dict(report, blocks=report["blocks"] + [extra])
        result = runner.summarize(corpus, extended)
        self.assertEqual(result["corpus_integrity"]["unexpected_report_block_ids"], ["extra-id"])
        self.assertFalse(result["corpus_integrity"]["full_corpus_and_label_mapping"])
        for key in ("sir_drafts", "exact_canonical_sir_match", "exact_canonical_sir_details",
                    "elaboration_success", "structure", "accepted_unconfirmed", "failures",
                    "usage"):
            self.assertEqual(result[key], baseline[key], key)
        self.assertEqual(result["structure"]["complete"]["count"], 1)
        self.assertEqual(result["accepted_unconfirmed"]["count"], 2)
        self.assertEqual(result["sir_drafts"]["count"], 3)

    def test_duplicate_report_block_counts_only_first_occurrence(self):
        corpus = {"cases": [{"id": "a", "block_id": "a-id", "expected_sir": sir()}]}
        first = {"block_id": "a-id", "label": "thm:a", "status": "formalization_error",
                 "sir": None, "elaborated": False, "complete_structure": False,
                 "accepted_speculatively": False, "checks": [], "errors": ["first"]}
        second = dict(first, status="sir_drafted", sir=sir(), elaborated=True,
                      complete_structure=True, accepted_speculatively=True, errors=[])
        result = runner.summarize(corpus, {"blocks": [first, second], "usage": {}})
        self.assertEqual(result["corpus_integrity"]["duplicate_report_block_ids"], ["a-id"])
        self.assertFalse(result["corpus_integrity"]["full_corpus_and_label_mapping"])
        self.assertEqual(result["sir_drafts"]["count"], 0)
        self.assertEqual(result["accepted_unconfirmed"]["count"], 0)
        self.assertEqual(result["structure"]["complete"]["count"], 0)
        self.assertEqual(result["failures"]["cases"][0]["errors"], ["first"])

    def test_realistic_success_failure_cost_and_structure_metrics(self):
        corpus, report = realistic_inputs()
        result = runner.summarize(corpus, report)
        self.assertTrue(result["corpus_integrity"]["full_corpus_and_label_mapping"])
        self.assertEqual(result["sir_drafts"], {"count": 3, "denominator": 4, "rate": .75})
        self.assertEqual(result["exact_canonical_sir_match"]["count"], 2)
        self.assertEqual(result["exact_canonical_sir_match"]["denominator"], 3)
        self.assertEqual(result["elaboration_success"]["count"], 3)
        self.assertEqual(result["structure"]["complete"]["count"], 1)
        self.assertEqual(result["structure"]["partial"]["count"], 1)
        self.assertEqual(result["structure"]["hard_mismatch"]["count"], 1)
        self.assertEqual(result["failures"]["blocked_dependency_count"], 1)
        self.assertEqual(
            sorted(case["status"] for case in result["failures"]["cases"]),
            ["formalization_error", "not_formalized"],
        )
        self.assertEqual(result["accepted_unconfirmed"]["count"], 2)
        self.assertEqual(result["usage"]["known_cost_calls"], 3)
        self.assertEqual(result["usage"]["unknown_cost_count"], 2)
        self.assertEqual(result["usage"]["unknown_input_token_calls"], 1)
        self.assertEqual(result["usage"]["unknown_output_token_calls"], 2)
        self.assertIsNone(result["usage"]["latency_ms"])
        self.assertTrue(result["batch_report_contract"]["valid"])

    def test_label_or_membership_mismatch_fails_integrity(self):
        corpus = {"cases": [{"id": "a", "block_id": "a-id", "expected_sir": sir()}]}
        report = {"blocks": [{"block_id": "a-id", "label": "wrong", "status": "formalization_error",
                              "sir": None, "elaborated": False, "complete_structure": False,
                              "accepted_speculatively": False, "checks": [], "errors": []}], "usage": {}}
        result = runner.summarize(corpus, report)
        self.assertFalse(result["corpus_integrity"]["full_corpus_and_label_mapping"])
        self.assertEqual(len(result["corpus_integrity"]["label_mismatches"]), 1)

    def test_missing_usage_and_contradictory_state_are_explicitly_unknown_or_invalid(self):
        corpus = {"cases": [{"id": "a", "block_id": "a-id", "expected_sir": sir()}]}
        report = {"blocks": [{"block_id": "a-id", "label": "thm:a", "status": "sir_drafted",
                              "sir": None, "elaborated": False, "complete_structure": True,
                              "accepted_speculatively": False, "checks": [], "errors": []}]}
        result = runner.summarize(corpus, report)
        self.assertFalse(result["batch_report_contract"]["valid"])
        self.assertIsNone(result["usage"]["calls"])
        self.assertIsNone(result["usage"]["known_cost"])
        self.assertIsNone(result["usage"]["known_cost_calls"])
        issues = {item["issue"] for item in result["batch_report_contract"]["issues"]}
        self.assertEqual(issues, {"sir_drafted_without_sir", "complete_without_acceptance"})


class GatingTests(unittest.TestCase):
    def test_batch_output_starts_absent_and_history_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "notes.tm"
            source.write_text("source stays intact")
            cases = root / "cases.json"
            cases.write_text(json.dumps({"cases": []}))
            report = {"blocks": [], "diagnostics": [], "usage": {}}

            def fake_batch(command, **kwargs):
                self.assertIn(str(source.resolve()), command)
                self.assertNotIn(str(cases.resolve()), command)
                output = Path(command[command.index("--output") + 1])
                self.assertFalse(output.exists())
                output.write_text(json.dumps(report))
                return subprocess.CompletedProcess(command, 0)

            arguments = ["--replay", str(root / "recordings"), "--mls", sys.executable,
                         "--source", str(source), "--cases", str(cases),
                         "--output-directory", str(root / "results")]
            with patch.object(runner.subprocess, "run", side_effect=fake_batch):
                with contextlib.redirect_stdout(io.StringIO()):
                    first = runner.run(arguments)
                    second = runner.run(arguments)
            self.assertEqual(len(set(first + second)), 4)
            self.assertTrue(all(path.exists() for path in first + second))
            self.assertEqual(json.loads(first[0].read_text()), report)
            for summary_path in (first[1], second[1]):
                history = json.loads(summary_path.read_text())
                self.assertEqual(history["report_source"], "output_file")
                self.assertEqual(history["cli_exit_code"], 0)
                self.assertTrue(history["complete"])
            self.assertEqual(source.read_text(), "source stays intact")

    def _run_with_fake_cli(self, write_output, exit_code, stdout=b"", output_text=None):
        """Run the evaluator with a fake CLI.

        Returns (result, exception, results dir, source, terminal stdout text). The fake
        writes ``output_text`` (or a default empty report) to --output when write_output is set,
        and returns ``stdout`` from the child process as the CLI would print it.
        """
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        source = root / "notes.tm"
        source.write_text("source stays intact")
        cases = root / "cases.json"
        cases.write_text(json.dumps({"cases": []}))
        results = root / "results"

        def fake_batch(command, **kwargs):
            if write_output:
                output = Path(command[command.index("--output") + 1])
                output.write_text(output_text if output_text is not None
                                  else json.dumps({"blocks": [], "diagnostics": [], "usage": {}}))
            return subprocess.CompletedProcess(command, exit_code, stdout=stdout)

        arguments = ["--replay", str(root / "recordings"), "--mls", sys.executable,
                     "--source", str(source), "--cases", str(cases),
                     "--output-directory", str(results)]
        printed = io.StringIO()
        with patch.object(runner.subprocess, "run", side_effect=fake_batch):
            with contextlib.redirect_stdout(printed):
                try:
                    return runner.run(arguments), None, results, source, printed.getvalue()
                except SystemExit as error:
                    return None, error, results, source, printed.getvalue()

    def test_stdout_report_is_preserved_when_output_file_is_missing(self):
        report = {"blocks": [], "diagnostics": [], "usage": {}}
        _, error, results, source, printed = self._run_with_fake_cli(
            write_output=False, exit_code=1, stdout=json.dumps(report).encode("utf-8"))
        self.assertIsNotNone(error)
        self.assertNotEqual(error.code, 0)
        self.assertIn("exit code 1", str(error.code))
        raw_files = sorted(results.glob("*.batch-report.json"))
        summary_files = sorted(results.glob("*.summary.json"))
        self.assertEqual(len(raw_files), 1)
        self.assertEqual(len(summary_files), 1)
        self.assertEqual(json.loads(raw_files[0].read_text()), report)
        history = json.loads(summary_files[0].read_text())
        self.assertEqual(history["report_source"], "stdout")
        self.assertEqual(history["cli_exit_code"], 1)
        self.assertFalse(history["complete"])
        self.assertEqual(history["raw_batch_report"], raw_files[0].name)
        self.assertEqual(sorted(p.name for p in results.iterdir()),
                         sorted([raw_files[0].name, summary_files[0].name]))
        self.assertEqual(source.read_text(), "source stays intact")

    def test_stdout_report_replaces_unparseable_output_file(self):
        report = {"blocks": [], "diagnostics": [], "usage": {}}
        result, error, results, _, _ = self._run_with_fake_cli(
            write_output=True, exit_code=0, stdout=json.dumps(report).encode("utf-8"),
            output_text="{truncated")
        self.assertIsNone(error)
        raw_path, summary_path = result
        self.assertEqual(json.loads(raw_path.read_text()), report)
        history = json.loads(summary_path.read_text())
        self.assertEqual(history["report_source"], "stdout")
        self.assertEqual(history["cli_exit_code"], 0)
        self.assertTrue(history["complete"])

    def test_non_json_stdout_is_echoed_and_nothing_is_preserved(self):
        error_text = b"model call records: /tmp/records\nnot a report\n"
        _, error, results, _, printed = self._run_with_fake_cli(
            write_output=False, exit_code=1, stdout=error_text)
        self.assertIsNotNone(error)
        self.assertNotEqual(error.code, 0)
        self.assertIn("exit code 1", str(error.code))
        self.assertIn("not a report", printed)
        self.assertEqual(list(results.iterdir()), [])

    def test_stdout_on_success_with_output_file_is_echoed_and_report_is_from_file(self):
        result, error, results, _, printed = self._run_with_fake_cli(
            write_output=True, exit_code=0, stdout=b"unexpected stdout\n")
        self.assertIsNone(error)
        self.assertIn("unexpected stdout", printed)
        raw_path, summary_path = result
        self.assertEqual(json.loads(summary_path.read_text())["report_source"], "output_file")

    def test_nonzero_exit_after_complete_report_preserves_report_and_history(self):
        _, error, results, source, _ = self._run_with_fake_cli(write_output=True, exit_code=1)
        self.assertIsNotNone(error)
        self.assertNotEqual(error.code, 0)
        self.assertIn("exit code 1", str(error.code))
        raw_files = sorted(results.glob("*.batch-report.json"))
        summary_files = sorted(results.glob("*.summary.json"))
        self.assertEqual(len(raw_files), 1)
        self.assertEqual(len(summary_files), 1)
        self.assertEqual(json.loads(raw_files[0].read_text()),
                         {"blocks": [], "diagnostics": [], "usage": {}})
        history = json.loads(summary_files[0].read_text())
        self.assertEqual(history["report_source"], "output_file")
        self.assertEqual(history["cli_exit_code"], 1)
        self.assertFalse(history["complete"])
        self.assertEqual(history["raw_batch_report"], raw_files[0].name)
        # Only the two preserved files remain; the staging directory is cleaned up.
        self.assertEqual(sorted(p.name for p in results.iterdir()),
                         sorted([raw_files[0].name, summary_files[0].name]))
        self.assertEqual(source.read_text(), "source stays intact")

    def test_nonzero_exit_without_report_writes_nothing(self):
        _, error, results, _, _ = self._run_with_fake_cli(write_output=False, exit_code=1)
        self.assertIsNotNone(error)
        self.assertNotEqual(error.code, 0)
        self.assertIn("exit code 1", str(error.code))
        self.assertEqual(list(results.iterdir()), [])

    def test_nonzero_exit_with_invalid_json_report_writes_nothing(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        source = root / "notes.tm"
        source.write_text("source")
        cases = root / "cases.json"
        cases.write_text(json.dumps({"cases": []}))
        results = root / "results"

        def fake_batch(command, **kwargs):
            Path(command[command.index("--output") + 1]).write_text("{truncated")
            return subprocess.CompletedProcess(command, 1)

        arguments = ["--replay", str(root / "recordings"), "--mls", sys.executable,
                     "--source", str(source), "--cases", str(cases),
                     "--output-directory", str(results)]
        with patch.object(runner.subprocess, "run", side_effect=fake_batch):
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as raised:
                    runner.run(arguments)
        self.assertNotEqual(raised.exception.code, 0)
        self.assertIn("not valid JSON", str(raised.exception.code))
        self.assertEqual(list(results.iterdir()), [])

    def test_relative_paths_are_resolved_against_caller_cwd(self):
        original = os.getcwd()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            caller = base / "caller"
            caller.mkdir()
            (base / "recordings").mkdir()
            fake_mls = base / "mls"
            fake_mls.write_text("#!/bin/sh\nexit 0\n")
            fake_mls.chmod(0o755)
            (base / "notes.tm").write_text("source")
            (base / "cases.json").write_text(json.dumps({"cases": []}))
            (base / "model.json").write_text("{}")
            report = {"blocks": [], "diagnostics": [], "usage": {}}
            captured = []

            def fake_batch(command, **kwargs):
                captured.append(command)
                output = Path(command[command.index("--output") + 1])
                output.write_text(json.dumps(report))
                return subprocess.CompletedProcess(command, 0)

            arguments = ["--replay", "../recordings", "--mls", "../mls",
                         "--source", "../notes.tm", "--cases", "../cases.json",
                         "--model-config", "../model.json",
                         "--output-directory", "../results"]
            os.chdir(caller)
            try:
                with patch.object(runner.subprocess, "run", side_effect=fake_batch):
                    with contextlib.redirect_stdout(io.StringIO()):
                        raw_path, summary_path = runner.run(arguments)
            finally:
                os.chdir(original)

            command = captured[0]
            self.assertEqual(command[0], str(fake_mls))
            # The source is the positional argument of `mls batch`.
            values = {
                "source": command[2],
                "--replay": command[command.index("--replay") + 1],
                "--model-config": command[command.index("--model-config") + 1],
            }
            expected = {
                "source": base / "notes.tm",
                "--replay": base / "recordings",
                "--model-config": base / "model.json",
            }
            for name, path in expected.items():
                self.assertTrue(Path(values[name]).is_absolute(), name)
                self.assertEqual(Path(values[name]), path, name)
            output = Path(command[command.index("--output") + 1])
            self.assertTrue(output.is_absolute())
            self.assertIn(base / "results", output.parents)
            results = base / "results"
            self.assertEqual(raw_path.parent, results)
            self.assertEqual(summary_path.parent, results)
            self.assertTrue(raw_path.exists() and summary_path.exists())
            self.assertEqual(json.loads(raw_path.read_text()), report)
            history = json.loads(summary_path.read_text())
            self.assertEqual(history["cli_exit_code"], 0)
            self.assertTrue(history["complete"])
            self.assertFalse((caller / "results").exists())

    def test_transport_is_required(self):
        process = subprocess.run(
            [sys.executable, str(Path(runner.__file__))], capture_output=True, text=True
        )
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("one of the arguments --replay --live is required", process.stderr)

    def test_live_gate_stops_before_executable_or_network(self):
        environment = dict(os.environ)
        environment.pop("MLS_LIVE", None)
        process = subprocess.run(
            [sys.executable, str(Path(runner.__file__)), "--live", "--mls", "/does/not/exist"],
            capture_output=True, text=True, env=environment,
        )
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("--live requires MLS_LIVE=1", process.stderr)
        self.assertNotIn("CLI not found", process.stderr)


if __name__ == "__main__":
    unittest.main()
