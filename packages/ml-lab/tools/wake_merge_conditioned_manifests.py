#!/usr/bin/env python
"""Merge conditioned manifests, preferring later successful retry rows."""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("inputs", type=Path, nargs="+")
    args = parser.parse_args()

    rows_by_id: dict[str, dict] = {}
    source_counts = Counter()
    replaced_errors = 0
    for input_path in args.inputs:
        for row in iter_jsonl(input_path):
            row_id = str(row.get("id", ""))
            if not row_id:
                continue
            prev = rows_by_id.get(row_id)
            if prev and not prev.get("ok") and row.get("ok"):
                replaced_errors += 1
            rows_by_id[row_id] = row
            source_counts[str(input_path)] += 1

    counters = Counter()
    args.output_jsonl.parent.mkdir(parents=True, exist_ok=True)
    with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as out:
        for row_id in sorted(rows_by_id):
            row = rows_by_id[row_id]
            out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            counters["rows"] += 1
            counters["ok_rows" if row.get("ok") else "error_rows"] += 1
            counters[f"semantic:{row.get('semantic_bucket', '')}"] += 1
            counters[f"kind:{row.get('output_kind', '')}"] += 1

    summary = {
        "output_jsonl": str(args.output_jsonl),
        "input_rows": dict(source_counts),
        "replaced_errors": replaced_errors,
        "counters": dict(sorted(counters.items())),
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
