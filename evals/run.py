#!/usr/bin/env python3
"""Run and summarize the small M2 statement evaluation corpus.

The batch process receives only ``statements.tm``.  ``cases.json`` is private to
this process and is used solely for evaluation.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import uuid
from typing import Any, Mapping, Sequence


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CASES = ROOT / "evals" / "cases.json"
DEFAULT_SOURCE = ROOT / "evals" / "statements.tm"
DEFAULT_RESULTS = ROOT / "evals" / "results"
BLOCKED_DEPENDENCY = "a formal statement dependency was not accepted speculatively"
FAILURE_TAXONOMY_VERSION = "mls-m2-failure-categories-v1"
# Order is also the deterministic precedence when a report has several signals.
FAILURE_CATEGORIES = (
    "sir_wrong",
    "structure_check_mismatch",
    "unknown_or_wrong_mathlib_name",
    "type_error",
    "other",
)


def failure_category(block: Mapping[str, Any]) -> str:
    """Classify final report evidence, never reference-SIR wording or guessed semantics.

    Name errors are Lean name-resolution diagnostics; the intended Mathlib symbol
    is not independently known. SIR wrong requires an explicit contract/rejection
    error. Missing SIRs, provider failures and unsupported admission syntax alone
    do not establish that a SIR is wrong.
    """
    errors = [error for error in block.get("errors", []) if isinstance(error, str)]
    sir_errors = {
        "SIR kind differs from tagged source kind",
        "SIR label differs from source label",
        "SIR uses differ from deterministically linked dependency labels",
        "the regenerated SIR equals the rejected SIR",
    }
    if any(error in sir_errors or re.fullmatch(
        r"SIR variable (?:name .+ cannot be preserved as a generated Lean binder"
        r"|h\d+ conflicts with the canonical hypothesis binder)", error
    ) for error in errors):
        return "sir_wrong"
    if any(check.get("hard") and check.get("state") == "mismatch"
           and check.get("scope", "structure") == "structure"
           for check in block.get("checks", [])):
        return "structure_check_mismatch"

    # Structured diagnostics are authoritative. Older reports also retain the
    # diagnostic in a Lean-prefixed error; unrelated provider/source prose is excluded.
    diagnostics = [item["message"] for item in block.get("lean_diagnostics", [])
                   if isinstance(item, dict) and isinstance(item.get("message"), str)]
    diagnostics.extend(error.removeprefix("Lean:").strip()
                       for error in errors if error.startswith("Lean:"))
    location = r"(?:^|\n)\s*(?:line \d+, column \d+:\s*)?"
    if any(re.search(location + r"(?:unknown (?:constant|identifier|namespace)\b"
                     r"|invalid field\b)", message, re.IGNORECASE)
           for message in diagnostics):
        return "unknown_or_wrong_mathlib_name"
    if any(re.search(location + r"(?:type mismatch\b|application (?:argument )?type mismatch\b"
                     r"|function expected\b|failed to synthesize\b)", message, re.IGNORECASE)
           for message in diagnostics):
        return "type_error"
    return "other"


def canonical_json(value: Any) -> str:
    """Return the exact JSON representation used by the string-level oracle."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


def ratio(count: int, denominator: int) -> dict[str, Any]:
    return {
        "count": count,
        "denominator": denominator,
        "rate": count / denominator if denominator else None,
    }


def summarize(corpus: Mapping[str, Any], report: Mapping[str, Any]) -> dict[str, Any]:
    """Compute transparent metrics from a BatchReport-shaped JSON object."""
    cases = corpus.get("cases", [])
    blocks = report.get("blocks", [])
    if not isinstance(cases, list) or not isinstance(blocks, list):
        raise ValueError("corpus cases and report blocks must be arrays")

    case_ids = [case.get("block_id") for case in cases]
    report_ids = [block.get("block_id") for block in blocks]
    duplicate_case_ids = sorted({item for item in case_ids if case_ids.count(item) > 1})
    duplicate_report_ids = sorted({item for item in report_ids if report_ids.count(item) > 1})
    case_id_set = set(case_ids)
    # Only blocks mapped to a corpus case feed metric numerators; the first occurrence
    # wins and later duplicates are reported solely as integrity issues.
    by_id = {}
    for block in blocks:
        block_id = block.get("block_id")
        if block_id in case_id_set and block_id not in by_id:
            by_id[block_id] = block
    mapped_blocks = list(by_id.values())
    missing = [item for item in case_ids if item not in by_id]
    unexpected = [item for item in report_ids if item not in case_id_set]

    label_mismatches = []
    exact = 0
    exact_details = []
    sir_drafts = 0
    report_contract_issues = []
    required_block_fields = {
        "block_id", "label", "status", "sir", "elaborated", "complete_structure",
        "accepted_speculatively", "checks", "errors",
    }
    for index, block in enumerate(blocks):
        missing_fields = sorted(required_block_fields - set(block))
        if missing_fields:
            report_contract_issues.append({
                "block_index": index, "block_id": block.get("block_id"),
                "issue": "missing_fields", "fields": missing_fields,
            })
        if block.get("status") == "sir_drafted" and not isinstance(block.get("sir"), dict):
            report_contract_issues.append({
                "block_index": index, "block_id": block.get("block_id"),
                "issue": "sir_drafted_without_sir",
            })
        if block.get("accepted_speculatively") and not block.get("elaborated"):
            report_contract_issues.append({
                "block_index": index, "block_id": block.get("block_id"),
                "issue": "accepted_without_elaboration",
            })
        if block.get("complete_structure") and not block.get("accepted_speculatively"):
            report_contract_issues.append({
                "block_index": index, "block_id": block.get("block_id"),
                "issue": "complete_without_acceptance",
            })
    # Reference hashes are oracle-derived. A generated hash equal to any reference hash
    # (an exact match, or a draft identical to another case's reference) is withheld so
    # the summary never discloses a reference canonical hash.
    reference_hashes = {canonical_hash(case.get("expected_sir")) for case in cases}
    counted_case_block_ids = set()
    for case in cases:
        block_id = case.get("block_id")
        block = by_id.get(block_id)
        expected = case.get("expected_sir")
        # A duplicated corpus block_id is already an integrity failure; count it once.
        if block is None or block_id in counted_case_block_ids:
            continue
        counted_case_block_ids.add(block_id)
        expected_label = expected.get("label") if isinstance(expected, dict) else None
        if block.get("label") != expected_label:
            label_mismatches.append({
                "block_id": case.get("block_id"),
                "expected": expected_label,
                "actual": block.get("label"),
            })
        generated = block.get("sir")
        # A valid draft remains present when later Lean attempts fail and the final
        # block status is formalization_error.
        drafted = isinstance(generated, dict)
        if drafted:
            sir_drafts += 1
            matches = canonical_json(generated) == canonical_json(expected)
            exact += int(matches)
            detail = {
                "case_id": case.get("id"),
                "block_id": case.get("block_id"),
                "match": matches,
            }
            generated_hash = canonical_hash(generated)
            if generated_hash not in reference_hashes:
                detail["generated_sha256"] = generated_hash
            exact_details.append(detail)

    elaborated = sum(bool(block.get("elaborated")) for block in mapped_blocks)
    accepted = sum(bool(block.get("accepted_speculatively")) for block in mapped_blocks)
    complete = sum(bool(block.get("complete_structure")) for block in mapped_blocks)
    partial = sum(
        bool(block.get("accepted_speculatively")) and not bool(block.get("complete_structure"))
        for block in mapped_blocks
    )
    hard_mismatch = sum(
        any(check.get("hard") and check.get("state") == "mismatch" for check in block.get("checks", []))
        for block in mapped_blocks
    )
    failures = []
    category_counts = dict.fromkeys(FAILURE_CATEGORIES, 0)
    blocked = 0
    for block in mapped_blocks:
        if block.get("accepted_speculatively"):
            continue
        errors = list(block.get("errors", []))
        is_blocked = any(BLOCKED_DEPENDENCY in str(error) for error in errors)
        blocked += int(is_blocked)
        category = failure_category(block)
        category_counts[category] += 1
        failures.append({
            "block_id": block.get("block_id"),
            "label": block.get("label"),
            "status": block.get("status"),
            "blocked_dependency": is_blocked,
            "category": category,
            "errors": errors,
        })

    usage = report.get("usage") if isinstance(report.get("usage"), dict) else {}
    calls = usage.get("calls")
    unknown_cost = usage.get("unknown_cost_calls")
    known_cost_calls = (
        max(0, int(calls) - int(unknown_cost))
        if isinstance(calls, int) and isinstance(unknown_cost, int) else None
    )
    full_integrity = not (
        duplicate_case_ids or duplicate_report_ids or missing or unexpected or label_mismatches
    ) and len(cases) == len(blocks)

    return {
        "metric_semantics": {
            "exact_canonical_sir_match": (
                "Equality of canonical JSON strings. It is reproducible but does not establish "
                "mathematical correctness; a different rewording can be mathematically correct."
            ),
            "accepted_speculatively": (
                "Engine acceptance after elaboration and no hard supported mismatch; it remains "
                "an unconfirmed draft."
            ),
            "structure_denominator": "Cases whose candidate Lean statement elaborated.",
            "failure_categories": (
                "One category per nonaccepted mapped case, using final report evidence. "
                "Precedence: SIR contract/rejection error, hard structure mismatch, Lean name "
                "resolution, Lean type error, other. Name resolution does not establish which "
                "Mathlib name was intended. Exact reference-SIR inequality is not SIR wrong; "
                "semantic faithfulness still requires independent review."
            ),
        },
        "source_case_count": len(cases),
        "report_block_count": len(blocks),
        "corpus_integrity": {
            "full_corpus_and_label_mapping": full_integrity,
            "duplicate_corpus_block_ids": duplicate_case_ids,
            "duplicate_report_block_ids": duplicate_report_ids,
            "missing_report_block_ids": missing,
            "unexpected_report_block_ids": unexpected,
            "label_mismatches": label_mismatches,
        },
        "batch_report_contract": {
            "valid": not report_contract_issues,
            "issues": report_contract_issues,
        },
        "sir_drafts": ratio(sir_drafts, len(cases)),
        "exact_canonical_sir_match": ratio(exact, sir_drafts),
        "exact_canonical_sir_details": exact_details,
        "elaboration_success": ratio(elaborated, len(cases)),
        "structure": {
            "hard_mismatch": ratio(hard_mismatch, elaborated),
            "partial": ratio(partial, elaborated),
            "complete": ratio(complete, elaborated),
        },
        "accepted_unconfirmed": ratio(accepted, len(cases)),
        "failures": {
            "taxonomy_version": FAILURE_TAXONOMY_VERSION,
            "count": len(failures),
            "denominator": len(cases),
            "blocked_dependency_count": blocked,
            "category_counts": category_counts,
            "cases": failures,
        },
        "diagnostics": report.get("diagnostics", []),
        "usage": {
            "calls": calls,
            "known_cost_calls": known_cost_calls,
            "unknown_cost_count": unknown_cost,
            "known_cost": usage.get("known_cost"),
            "input_tokens_reported_total": usage.get("input_tokens"),
            "output_tokens_reported_total": usage.get("output_tokens"),
            "unknown_input_token_calls": usage.get("unknown_input_token_calls"),
            "unknown_output_token_calls": usage.get("unknown_output_token_calls"),
            "tokens_note": (
                "Totals include only reported tokens. Separate counters identify calls with "
                "missing input/output usage; null means an older report omitted the counter."
            ),
            "latency_ms": None,
            "latency_note": "BatchReport does not currently expose per-call or aggregate latency.",
        },
    }


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    transport = parser.add_mutually_exclusive_group(required=True)
    transport.add_argument("--replay", type=Path, help="directory of replay recordings")
    transport.add_argument("--live", action="store_true", help="make explicitly gated live calls")
    parser.add_argument("--mls", type=Path, default=ROOT / "target" / "debug" / "mls")
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--cases", type=Path, default=DEFAULT_CASES)
    parser.add_argument("--output-directory", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--model-config", type=Path)
    parser.add_argument("--max-attempts", type=int)
    return parser.parse_args(argv)


_NOT_JSON = object()


def parse_json(data: bytes | None) -> Any:
    """Return the parsed JSON value, or _NOT_JSON when data is absent or invalid."""
    if data is None:
        return _NOT_JSON
    try:
        return json.loads(data)
    except ValueError:
        return _NOT_JSON


def select_report(file_bytes: bytes | None, captured: bytes) -> tuple[Any, bytes | None, str | None]:
    """Choose the batch report: the --output file if it parses, else a JSON object from stdout.

    Returns (report, raw_bytes, report_source), or (None, None, None) when neither is usable.
    """
    value = parse_json(file_bytes)
    if value is not _NOT_JSON:
        return value, file_bytes, "output_file"
    value = parse_json(captured)
    if isinstance(value, dict):
        return value, captured, "stdout"
    return None, None, None


def run(argv: Sequence[str] | None = None) -> tuple[Path, Path]:
    args = parse_args(argv)
    # The CLI runs with cwd=ROOT, so every path must be absolute before it is used.
    for name in ("replay", "mls", "source", "cases", "output_directory", "model_config"):
        value = getattr(args, name)
        if value is not None:
            setattr(args, name, value.resolve())
    if args.live and os.environ.get("MLS_LIVE") != "1":
        raise SystemExit("--live requires MLS_LIVE=1; no live request was made")
    if args.max_attempts is not None and args.max_attempts <= 0:
        raise SystemExit("--max-attempts must be positive")
    if not args.mls.is_file():
        raise SystemExit(f"MLS CLI not found at {args.mls}; build it before running the evaluation")

    args.output_directory.mkdir(parents=True, exist_ok=True)
    now = dt.datetime.now(dt.timezone.utc)
    run_id = f"{now.strftime('%Y%m%dT%H%M%SZ')}-{uuid.uuid4().hex[:10]}"
    raw_path = args.output_directory / f"{run_id}.batch-report.json"
    summary_path = args.output_directory / f"{run_id}.summary.json"
    command = [str(args.mls), "batch", str(args.source)]
    command += ["--live"] if args.live else ["--replay", str(args.replay)]
    if args.model_config:
        command += ["--model-config", str(args.model_config)]
    if args.max_attempts is not None:
        command += ["--max-attempts", str(args.max_attempts)]

    staging = tempfile.TemporaryDirectory(prefix="mls-eval-", dir=args.output_directory)
    temporary = Path(staging.name) / "batch-report.json"
    try:
        # Validate the private oracle before spending work on the batch. Its contents
        # are never included in the child process arguments or environment.
        corpus_bytes = args.cases.read_bytes()
        corpus = json.loads(corpus_bytes.decode("utf-8"))
        if not isinstance(corpus.get("cases"), list):
            raise ValueError("evaluation corpus must contain a cases array")
        command += ["--output", str(temporary)]
        # A nonzero exit can follow a complete report after paid model calls. The status
        # is recorded rather than raised, so any parseable report is still preserved.
        # stdout is captured so a report printed instead of written to --output is not lost;
        # stderr still streams to the terminal (it carries the model-call record directory).
        completed = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE)
        exit_code = completed.returncode
        captured = completed.stdout or b""
        file_bytes = temporary.read_bytes() if temporary.is_file() else None
        report, report_bytes, report_source = select_report(file_bytes, captured)
        if captured and report_source != "stdout":
            # A normal run leaves stdout empty; any output that is not the report is shown.
            sys.stdout.write(captured.decode("utf-8", errors="replace"))
            sys.stdout.flush()
        if report_bytes is None:
            if file_bytes is None:
                raise SystemExit(
                    f"MLS batch command failed with exit code {exit_code} and wrote no report; "
                    "nothing was preserved"
                )
            raise SystemExit(
                f"MLS batch report is not valid JSON (exit code {exit_code}); "
                "nothing was preserved"
            )
        summary_error = None
        try:
            summary = summarize(corpus, report)
        except (AttributeError, TypeError, ValueError) as error:
            summary = None
            summary_error = f"{type(error).__name__}: {error}"
        history = {
            "schema_version": 1,
            "run_id": run_id,
            "run_at_utc": now.isoformat().replace("+00:00", "Z"),
            "mode": "live" if args.live else "replay",
            "source": str(args.source),
            "cases": str(args.cases),
            "cases_sha256": hashlib.sha256(corpus_bytes).hexdigest(),
            "raw_batch_report": raw_path.name,
            "report_source": report_source,
            "cli_exit_code": exit_code,
            "complete": exit_code == 0 and summary_error is None,
            "summary": summary,
        }
        if summary_error is not None:
            history["summary_error"] = summary_error
        # Exclusive creation makes immutable run history safe against accidental overwrite.
        with raw_path.open("xb") as output:
            output.write(report_bytes)
        with summary_path.open("x", encoding="utf-8") as output:
            json.dump(history, output, ensure_ascii=False, indent=2)
            output.write("\n")
    finally:
        staging.cleanup()

    print(json.dumps(history, ensure_ascii=False, indent=2))
    if exit_code != 0 or summary_error is not None:
        problems = []
        if exit_code != 0:
            problems.append(f"MLS batch command failed with exit code {exit_code}")
        if summary_error is not None:
            problems.append(f"report could not be summarized ({summary_error})")
        raise SystemExit(
            f"{'; '.join(problems)}; preserved {raw_path} and {summary_path}"
        )
    return raw_path, summary_path


if __name__ == "__main__":
    run()
