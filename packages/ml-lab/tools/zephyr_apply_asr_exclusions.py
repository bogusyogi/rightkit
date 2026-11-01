#!/usr/bin/env python
"""Apply ASR verification exclusions to the canonical Zephyr manifest."""

from __future__ import annotations

import argparse
import csv
import json
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterable


def iter_jsonl(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def write_json(path: Path, data: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(path)


def bucket(row: dict) -> str:
    if int(row.get("label") or 0) == 1:
        return "positive"
    return str(row.get("negative_bucket") or row.get("source_family") or "negative")


def summarize_row(counter: Counter, row: dict, prefix: str) -> None:
    split = str(row.get("split") or "")
    label = int(row.get("label") or 0)
    family = str(row.get("source_family") or "")
    b = bucket(row)
    seconds = float(row.get("duration_s") or 0.0)
    counter[f"{prefix}:rows"] += 1
    counter[f"{prefix}:seconds"] += seconds
    counter[f"{prefix}:split:{split}:rows"] += 1
    counter[f"{prefix}:split:{split}:seconds"] += seconds
    counter[f"{prefix}:label:{label}:rows"] += 1
    counter[f"{prefix}:label:{label}:seconds"] += seconds
    counter[f"{prefix}:family:{family}:rows"] += 1
    counter[f"{prefix}:family:{family}:seconds"] += seconds
    counter[f"{prefix}:bucket:{b}:rows"] += 1
    counter[f"{prefix}:bucket:{b}:seconds"] += seconds
    counter[f"{prefix}:split_bucket:{split}|{b}:rows"] += 1
    counter[f"{prefix}:split_bucket:{split}|{b}:seconds"] += seconds


def load_exclusions(path: Path) -> tuple[set[str], list[dict], Counter]:
    excluded: set[str] = set()
    rows: list[dict] = []
    counts: Counter = Counter()
    for row in iter_jsonl(path):
        uid = str(row.get("uid") or "")
        if not uid:
            continue
        excluded.add(uid)
        rows.append(row)
        summarize_row(counts, row, "excluded")
    return excluded, rows, counts


def filter_manifest(input_path: Path, output_path: Path, excluded: set[str], counts: Counter) -> None:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8", newline="\n") as out:
        for row in iter_jsonl(input_path):
            summarize_row(counts, row, "before")
            uid = str(row.get("uid") or "")
            if uid in excluded:
                summarize_row(counts, row, "matched_excluded")
                continue
            summarize_row(counts, row, "after")
            out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def write_exclusion_ledger(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as out:
        for row in rows:
            out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def write_bucket_csv(path: Path, counts: Counter) -> None:
    rows = []
    prefixes = ["before", "matched_excluded", "after"]
    keys = set()
    for key in counts:
        for prefix in prefixes:
            marker = f"{prefix}:split_bucket:"
            if key.startswith(marker) and key.endswith(":rows"):
                keys.add(key[len(marker) : -len(":rows")])
    for split_bucket in sorted(keys):
        split, b = split_bucket.split("|", 1)
        row = {"split": split, "bucket": b}
        for prefix in prefixes:
            base = f"{prefix}:split_bucket:{split_bucket}"
            row[f"{prefix}_rows"] = int(counts.get(f"{base}:rows", 0))
            row[f"{prefix}_hours"] = round(float(counts.get(f"{base}:seconds", 0.0)) / 3600.0, 6)
        rows.append(row)
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "split",
                "bucket",
                "before_rows",
                "before_hours",
                "matched_excluded_rows",
                "matched_excluded_hours",
                "after_rows",
                "after_hours",
            ],
        )
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument(
        "--input-jsonl",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_20260609.jsonl"),
    )
    parser.add_argument(
        "--input-vast-jsonl",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_vast_20260609.jsonl"),
    )
    parser.add_argument(
        "--drop-jsonl",
        type=Path,
        default=Path(
            r"D:\Claude\heardright\train\reports\asr_verify_whisper_flags_20260609\whisper_flags_shard00-of-01_drop_candidate.jsonl"
        ),
    )
    parser.add_argument(
        "--output-jsonl",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_asr_clean_20260610.jsonl"),
    )
    parser.add_argument(
        "--output-vast-jsonl",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_asr_clean_vast_20260610.jsonl"),
    )
    parser.add_argument(
        "--exclusion-ledger",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_asr_exclusions_20260610.jsonl"),
    )
    parser.add_argument(
        "--summary-json",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\reports\zephyr_canonical_training_split_asr_clean_20260610_summary.json"),
    )
    parser.add_argument(
        "--bucket-csv",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\reports\zephyr_canonical_training_split_asr_clean_20260610_bucket_hours.csv"),
    )
    args = parser.parse_args()

    started = time.time()
    excluded, excluded_rows, counts = load_exclusions(args.drop_jsonl)
    filter_manifest(args.input_jsonl, args.output_jsonl, excluded, counts)
    # Vast manifest has same uid set but different transfer paths; use separate
    # counters only to verify the same number of rows is removed.
    vast_counts: Counter = Counter()
    filter_manifest(args.input_vast_jsonl, args.output_vast_jsonl, excluded, vast_counts)
    write_exclusion_ledger(args.exclusion_ledger, excluded_rows)
    write_bucket_csv(args.bucket_csv, counts)

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "input_jsonl": str(args.input_jsonl),
        "input_vast_jsonl": str(args.input_vast_jsonl),
        "output_jsonl": str(args.output_jsonl),
        "output_vast_jsonl": str(args.output_vast_jsonl),
        "drop_jsonl": str(args.drop_jsonl),
        "exclusion_ledger": str(args.exclusion_ledger),
        "bucket_csv": str(args.bucket_csv),
        "unique_excluded_uids": len(excluded),
        "counts": dict(counts),
        "vast_counts": dict(vast_counts),
    }
    write_json(args.summary_json, summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
