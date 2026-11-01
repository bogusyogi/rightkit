#!/usr/bin/env python
"""Validate final Zephyr source-truth audit artifacts.

This is intentionally small and artifact-only. Run it after
wake_source_truth_final_audit_when_idle.ps1 has produced the merged manifest,
audit summary, input audit, raw CSV, and bucket CSV.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from pathlib import Path


EXPECTED_MERGE_ORDER = "later input rows supersede earlier rows by deterministic id"


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def count_nonempty_lines(path: Path) -> int:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        return sum(1 for line in f if line.strip())


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def csv_data_rows(path: Path) -> int:
    with path.open("r", encoding="utf-8", newline="") as f:
        reader = csv.reader(f)
        rows = list(reader)
    if not rows:
        return 0
    return max(0, len(rows) - 1)


def nested_get(data: dict, keys: list[str], default=None):
    current = data
    for key in keys:
        if not isinstance(current, dict) or key not in current:
            return default
        current = current[key]
    return current


def canonical_path_text(value: str, repo_root: Path) -> str:
    path = Path(value)
    if not path.is_absolute():
        path = repo_root / path
    return str(path.resolve()).replace("/", "\\").lower()


def validate(args: argparse.Namespace) -> tuple[list[dict], dict]:
    failures: list[dict] = []

    required_paths = {
        "merged_manifest": args.merged_manifest,
        "summary_json": args.summary_json,
        "input_audit_json": args.input_audit_json,
        "residual_raws_csv": args.residual_raws_csv,
        "bucket_report_csv": args.bucket_report_csv,
        "expected_buckets_csv": args.expected_buckets_csv,
        "input_list": args.input_list,
    }
    for name, path in required_paths.items():
        if not path.exists():
            failures.append({"gate": name, "reason": "missing", "path": str(path)})

    if failures:
        return failures, {"checked_paths": {name: str(path) for name, path in required_paths.items()}}

    summary = load_json(args.summary_json)
    input_audit = load_json(args.input_audit_json)
    expected_input_count = count_nonempty_lines(args.input_list)
    residual_csv_rows = csv_data_rows(args.residual_raws_csv)
    bucket_csv_rows = csv_data_rows(args.bucket_report_csv)
    merged_rows = count_nonempty_lines(args.merged_manifest)

    zero_summary_fields = [
        "latest_unresolved_error_ids",
        "missing_output_count",
        "bad_policy_count",
        "bad_conditioned_root_count",
        "expected_bucket_failure_count",
        "music_split_failure_count",
        "positive_unification_failure_count",
        "residual_raw_blocking_count",
        "obsolete_wake_raw_blocking_count",
    ]
    for field in zero_summary_fields:
        value = int(summary.get(field, -1))
        if value != 0:
            failures.append({"gate": field, "reason": "expected_zero", "actual": value})

    raw_total = nested_get(summary, ["raw_scan", "total_audio_video_files"])
    if raw_total != 0:
        failures.append({"gate": "raw_scan.total_audio_video_files", "reason": "expected_zero", "actual": raw_total})
    if residual_csv_rows != 0:
        failures.append({"gate": "residual_raws_csv", "reason": "expected_header_only", "actual_data_rows": residual_csv_rows})

    if bucket_csv_rows <= 0:
        failures.append({"gate": "bucket_report_csv", "reason": "expected_data_rows", "actual_data_rows": bucket_csv_rows})
    if merged_rows <= 0:
        failures.append({"gate": "merged_manifest", "reason": "expected_data_rows", "actual_rows": merged_rows})

    music_lyrics = nested_get(summary, ["bucket_assertions", "music_split", "music_lyrics_label_0_rows"], 0)
    music_non_lyric = nested_get(summary, ["bucket_assertions", "music_split", "music_non_lyric_label_0_rows"], 0)
    if int(music_lyrics or 0) <= 0:
        failures.append({"gate": "music_lyrics_label_0_rows", "reason": "expected_positive", "actual": music_lyrics})
    if int(music_non_lyric or 0) <= 0:
        failures.append({"gate": "music_non_lyric_label_0_rows", "reason": "expected_positive", "actual": music_non_lyric})

    pos_rows = nested_get(summary, ["bucket_assertions", "positive_unification", "zephyr_positive_label_1_rows"], 0)
    bad_pos_rows = nested_get(summary, ["bucket_assertions", "positive_unification", "zephyr_positive_bad_label_rows"], -1)
    if int(pos_rows or 0) <= 0:
        failures.append({"gate": "zephyr_positive_label_1_rows", "reason": "expected_positive", "actual": pos_rows})
    if int(bad_pos_rows or 0) != 0:
        failures.append({"gate": "zephyr_positive_bad_label_rows", "reason": "expected_zero", "actual": bad_pos_rows})

    if bool(input_audit.get("allow_missing_music_split")):
        failures.append({"gate": "allow_missing_music_split", "reason": "must_be_false_for_normal_completion"})
    if input_audit.get("merge_order") != EXPECTED_MERGE_ORDER:
        failures.append({"gate": "merge_order", "reason": "unexpected", "actual": input_audit.get("merge_order")})
    if int(input_audit.get("input_count", -1)) != expected_input_count:
        failures.append(
            {
                "gate": "input_count",
                "reason": "does_not_match_input_list",
                "actual": input_audit.get("input_count"),
                "expected": expected_input_count,
            }
        )

    inputs = input_audit.get("inputs") or []
    if len(inputs) != expected_input_count:
        failures.append({"gate": "inputs", "reason": "length_mismatch", "actual": len(inputs), "expected": expected_input_count})
    input_audit_paths = []
    for index, row in enumerate(inputs):
        input_path = Path(str(row.get("path", "")))
        if not input_path.is_absolute():
            input_path = args.repo_root / input_path
        input_audit_paths.append(canonical_path_text(str(input_path), args.repo_root))
        if int(row.get("order", -1)) != index:
            failures.append({"gate": "inputs.order", "reason": "not_contiguous", "index": index, "actual": row.get("order")})
        sha256 = str(row.get("sha256", ""))
        if len(sha256) != 64 or any(ch not in "0123456789abcdef" for ch in sha256):
            failures.append({"gate": "inputs.sha256", "reason": "invalid", "index": index, "actual": sha256})
        try:
            audited_bytes = int(row.get("bytes", 0))
        except (TypeError, ValueError):
            audited_bytes = 0
        if audited_bytes <= 0:
            failures.append({"gate": "inputs.bytes", "reason": "expected_positive", "index": index, "actual": row.get("bytes")})
        if not input_path.exists():
            failures.append({"gate": "inputs.path", "reason": "missing", "index": index, "path": str(input_path)})
            continue
        actual_bytes = input_path.stat().st_size
        if actual_bytes != audited_bytes:
            failures.append(
                {
                    "gate": "inputs.bytes",
                    "reason": "does_not_match_file",
                    "index": index,
                    "actual": actual_bytes,
                    "expected": audited_bytes,
                    "path": str(input_path),
                }
            )
        if len(sha256) == 64 and not any(ch not in "0123456789abcdef" for ch in sha256):
            actual_sha256 = sha256_file(input_path)
            if actual_sha256 != sha256:
                failures.append(
                    {
                        "gate": "inputs.sha256",
                        "reason": "does_not_match_file",
                        "index": index,
                        "actual": actual_sha256,
                        "expected": sha256,
                        "path": str(input_path),
                    }
                )

    summary_manifest_paths = [canonical_path_text(str(path), args.repo_root) for path in summary.get("manifests", [])]
    if summary_manifest_paths != input_audit_paths:
        failures.append(
            {
                "gate": "summary.manifests",
                "reason": "does_not_match_input_audit_order",
                "summary_count": len(summary_manifest_paths),
                "input_audit_count": len(input_audit_paths),
                "first_summary_only": next((path for path in summary_manifest_paths if path not in input_audit_paths), ""),
                "first_input_only": next((path for path in input_audit_paths if path not in summary_manifest_paths), ""),
            }
        )
    summary_output = str(summary.get("output_jsonl") or "")
    if summary_output and canonical_path_text(summary_output, args.repo_root) != canonical_path_text(str(args.merged_manifest), args.repo_root):
        failures.append(
            {
                "gate": "summary.output_jsonl",
                "reason": "does_not_match_merged_manifest_argument",
                "actual": summary_output,
                "expected": str(args.merged_manifest),
            }
        )

    report = {
        "summary_json": str(args.summary_json),
        "input_audit_json": str(args.input_audit_json),
        "merged_manifest": str(args.merged_manifest),
        "input_count": expected_input_count,
        "summary_manifest_count": len(summary_manifest_paths),
        "merged_rows": merged_rows,
        "bucket_csv_rows": bucket_csv_rows,
        "residual_csv_rows": residual_csv_rows,
        "failure_count": len(failures),
        "failures": failures,
    }
    return failures, report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument("--merged-manifest", type=Path, default=Path(r"train\manifests\source_truth_final_latest_merged.jsonl"))
    parser.add_argument("--summary-json", type=Path, default=Path(r"train\reports\source_truth_final_latest_audit_summary.json"))
    parser.add_argument("--input-audit-json", type=Path, default=Path(r"train\reports\source_truth_final_manifest_inputs_audit.json"))
    parser.add_argument("--input-list", type=Path, default=Path(r"train\manifests\source_truth_final_manifest_inputs.txt"))
    parser.add_argument("--residual-raws-csv", type=Path, default=Path(r"train\reports\source_truth_final_residual_raws.csv"))
    parser.add_argument("--bucket-report-csv", type=Path, default=Path(r"train\reports\source_truth_final_bucket_report.csv"))
    parser.add_argument("--expected-buckets-csv", type=Path, default=Path(r"train\reports\source_truth_expected_buckets.csv"))
    parser.add_argument("--report-json", type=Path, default=Path(r"train\reports\source_truth_final_evidence_validation.json"))
    args = parser.parse_args()

    for attr in [
        "merged_manifest",
        "summary_json",
        "input_audit_json",
        "input_list",
        "residual_raws_csv",
        "bucket_report_csv",
        "expected_buckets_csv",
        "report_json",
    ]:
        path = getattr(args, attr)
        if not path.is_absolute():
            setattr(args, attr, args.repo_root / path)

    failures, report = validate(args)
    args.report_json.parent.mkdir(parents=True, exist_ok=True)
    args.report_json.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if not failures else 2


if __name__ == "__main__":
    raise SystemExit(main())
