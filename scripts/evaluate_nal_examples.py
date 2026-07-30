#!/usr/bin/env python3
"""Run ONA's Narsese example expectations against the Rust shell."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


EXPECTED_PREFIX = "Comment: expected: "
ANSWER_PREFIX = EXPECTED_PREFIX + "Answer: "
FLOAT_PATTERN = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"


def previous_segment(lines: list[str], index: int) -> list[str]:
    start = 0
    for candidate in range(index - 1, -1, -1):
        if lines[candidate].startswith(EXPECTED_PREFIX):
            start = candidate + 1
            break
    return lines[start:index]


def parse_truth(line: str) -> tuple[float, float] | None:
    match = re.search(
        rf"Truth:\s*frequency=({FLOAT_PATTERN}),?\s*confidence=({FLOAT_PATTERN})",
        line,
    )
    if match is None:
        return None
    return float(match.group(1)), float(match.group(2))


def evaluate_output(
    name: str, output: str, baseline_output: str | None = None
) -> tuple[int, int, list[str]]:
    lines = [line.strip() for line in output.splitlines()]
    baseline_lines = (
        [line.strip() for line in baseline_output.splitlines()]
        if baseline_output is not None
        else []
    )
    baseline_expected_indices = [
        index
        for index, line in enumerate(baseline_lines)
        if line.startswith(EXPECTED_PREFIX)
    ]
    total = 0
    passed = 0
    failures: list[str] = []
    expectation_ordinal = 0
    for index, expected_line in enumerate(lines):
        if not expected_line.startswith(EXPECTED_PREFIX):
            continue
        total += 1
        segment = previous_segment(lines, index)
        baseline_segment = []
        if expectation_ordinal < len(baseline_expected_indices):
            baseline_index = baseline_expected_indices[expectation_ordinal]
            baseline_segment = previous_segment(baseline_lines, baseline_index)
        expectation_ordinal += 1
        expected_payload = expected_line[len(EXPECTED_PREFIX) :].strip()
        answer_payload = (
            expected_payload[len("Answer:") :].strip()
            if expected_payload.startswith("Answer:")
            else expected_payload
        )
        expected_term = answer_payload.split("Truth:", 1)[0].strip()
        baseline_matching = [
            line
            for line in baseline_segment
            if line.startswith("Answer:") and expected_term in line
        ]
        answer_expectation = expected_line.startswith(ANSWER_PREFIX) or (
            expected_payload.startswith("<") and bool(baseline_matching)
        )
        if answer_expectation:
            expected = answer_payload
            expected_term = expected.split("Truth:", 1)[0].strip()
            expected_truth = parse_truth(expected_line)
            matching = [
                line
                for line in segment
                if line.startswith("Answer:") and expected_term in line
            ]
            if not matching:
                failures.append(f"{name}: missing {expected_line}")
                continue
            baseline_truth = (
                parse_truth(baseline_matching[-1]) if baseline_matching else None
            )
            target_truth = baseline_truth or expected_truth
            if target_truth is not None:
                truth_matches = []
                for answer in matching:
                    actual_truth = parse_truth(answer)
                    if actual_truth is None:
                        continue
                    frequency_ok = abs(actual_truth[0] - target_truth[0]) <= 1.0e-6
                    confidence_ok = (
                        abs(actual_truth[1] - target_truth[1]) <= 1.0e-6
                        if baseline_truth is not None
                        else actual_truth[1] + 1.0e-6 >= target_truth[1]
                    )
                    if frequency_ok and confidence_ok:
                        truth_matches.append(answer)
                if not truth_matches:
                    failures.append(
                        f"{name}: truth mismatch for {expected_line}; got {matching[-1]}"
                    )
                    continue
            passed += 1
            continue

        expected_execution = expected_line[len(EXPECTED_PREFIX) :].strip()
        if expected_execution in segment:
            passed += 1
        else:
            failures.append(f"{name}: missing execution {expected_execution}")
    return total, passed, failures


def main() -> int:
    workspace = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--binary",
        type=Path,
        default=workspace / "target/release/nars-cli",
    )
    parser.add_argument(
        "--examples",
        type=Path,
        default=workspace / "upstream/ona/examples/nal",
    )
    parser.add_argument(
        "--upstream-binary",
        type=Path,
        default=workspace / "upstream/ona/NAR",
        help="optional ONA binary used as the authoritative truth baseline",
    )
    parser.add_argument("names", nargs="*", help="optional example stems")
    args = parser.parse_args()

    if not args.binary.is_file():
        print(f"missing Rust shell binary: {args.binary}", file=sys.stderr)
        print("run: cargo build --release -p nars-cli", file=sys.stderr)
        return 2

    selected = set(args.names)
    examples = [
        path
        for path in sorted(args.examples.glob("*.nal"))
        if not selected or path.stem in selected
    ]
    total = 0
    passed = 0
    failures: list[str] = []
    for example in examples:
        result = subprocess.run(
            [str(args.binary), "shell"],
            input=example.read_text(),
            text=True,
            capture_output=True,
            timeout=180,
            check=False,
        )
        baseline = None
        if args.upstream_binary.is_file():
            baseline = subprocess.run(
                [str(args.upstream_binary), "shell"],
                input=example.read_text(),
                text=True,
                capture_output=True,
                timeout=180,
                check=False,
            )
        if result.returncode != 0:
            if baseline is not None and baseline.returncode != 0:
                print(f"{example.name}: upstream-equivalent rejection")
                continue
            failures.append(
                f"{example.name}: shell exited {result.returncode}: {result.stderr.strip()}"
            )
            continue
        example_total, example_passed, example_failures = evaluate_output(
            example.name,
            result.stdout,
            baseline.stdout if baseline is not None and baseline.returncode == 0 else None,
        )
        total += example_total
        passed += example_passed
        failures.extend(example_failures)
        print(f"{example.name}: {example_passed}/{example_total}")

    print(f"NAL expectations: {passed}/{total} across {len(examples)} examples")
    if failures:
        print("Failures:")
        for failure in failures:
            print(f"- {failure}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
