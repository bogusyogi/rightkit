from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def parse_csv(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def should_drop(row: dict, positive_splits: set[str], near_bucket: str) -> str | None:
    if str(row.get("negative_bucket") or "") == near_bucket:
        return "drop_near_bucket"
    if row.get("label") == 1 and str(row.get("split") or "") in positive_splits:
        return "drop_eval_positive"
    return None


def summarize_key(row: dict) -> tuple[str, int, str, str]:
    return (
        str(row.get("split") or ""),
        int(row.get("label") or 0),
        str(row.get("source_bucket") or ""),
        str(row.get("semantic_bucket") or ""),
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--summary-out", type=Path, default=None)
    parser.add_argument("--drop-positive-splits", default="eval_primary")
    parser.add_argument("--near-bucket", default="near_negative")
    args = parser.parse_args()

    positive_splits = set(parse_csv(args.drop_positive_splits))
    kept = 0
    dropped = 0
    drop_reason_counts: Counter[str] = Counter()
    kept_counts: Counter[tuple[str, int, str, str]] = Counter()
    dropped_counts: Counter[tuple[str, int, str, str]] = Counter()

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.manifest.open("r", encoding="utf-8") as src, args.out.open("w", encoding="utf-8") as dst:
        for line in src:
            row = json.loads(line)
            reason = should_drop(row, positive_splits, args.near_bucket)
            if reason is None:
                dst.write(json.dumps(row, separators=(",", ":")) + "\n")
                kept += 1
                kept_counts[summarize_key(row)] += 1
                continue
            dropped += 1
            drop_reason_counts[reason] += 1
            dropped_counts[summarize_key(row)] += 1

    summary = {
        "manifest": str(args.manifest),
        "out": str(args.out),
        "drop_positive_splits": sorted(positive_splits),
        "near_bucket": args.near_bucket,
        "kept_rows": kept,
        "dropped_rows": dropped,
        "drop_reasons": dict(sorted(drop_reason_counts.items())),
        "kept_counts": [
            {
                "split": split,
                "label": label,
                "source_bucket": source_bucket,
                "semantic_bucket": semantic_bucket,
                "rows": rows,
            }
            for (split, label, source_bucket, semantic_bucket), rows in sorted(kept_counts.items())
        ],
        "dropped_counts": [
            {
                "split": split,
                "label": label,
                "source_bucket": source_bucket,
                "semantic_bucket": semantic_bucket,
                "rows": rows,
            }
            for (split, label, source_bucket, semantic_bucket), rows in sorted(dropped_counts.items())
        ],
    }

    summary_out = args.summary_out or args.out.with_suffix(args.out.suffix + ".summary.json")
    summary_out.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({"out": str(args.out), "summary": str(summary_out), "kept_rows": kept, "dropped_rows": dropped}, sort_keys=True))


if __name__ == "__main__":
    main()
