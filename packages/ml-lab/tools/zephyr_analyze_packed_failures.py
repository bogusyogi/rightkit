#!/usr/bin/env python
"""Summarize failed packed conditioning rows for the Zephyr source-truth cleanup.

This is intentionally read-only. It scans packed manifest JSONL files, extracts
latest failed rows, groups them by source and error family, and writes a retry
worklist candidate for human approval.
"""

from __future__ import annotations

import argparse
import json
import time
from collections import Counter, defaultdict
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line_no, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                yield line_no, json.loads(line)
            except json.JSONDecodeError as exc:
                yield line_no, {"id": f"{path}:{line_no}", "ok": False, "error": f"bad_json:{exc}", "source_path": ""}


def classify_error(error: str) -> str:
    low = error.lower()
    if "timed out" in low:
        return "decode_timeout"
    if "no such file" in low or "cannot find" in low or "not found" in low:
        return "missing_source"
    if "invalid data" in low or "could not find codec" in low:
        return "decode_invalid_or_codec"
    if "row_not_ok" in low:
        return "row_not_ok"
    if "bad_json" in low:
        return "bad_json"
    return "other"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, action="append", required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--failed-jsonl", type=Path, required=True)
    parser.add_argument("--source-summary-jsonl", type=Path, required=True)
    args = parser.parse_args()

    latest: dict[str, dict] = {}
    input_stats = []
    started = time.time()

    for index, manifest in enumerate(args.manifest):
        counters: Counter = Counter()
        for _, row in iter_jsonl(manifest):
            row_id = str(row.get("id") or "")
            if not row_id:
                counters["missing_id"] += 1
                continue
            row["_input_index"] = index
            row["_input_manifest"] = str(manifest)
            latest[row_id] = row
            counters["rows"] += 1
            counters["ok" if row.get("ok") else "error"] += 1
        input_stats.append({"manifest": str(manifest), "counters": dict(counters)})

    failed = [row for row in latest.values() if not row.get("ok")]
    by_error_family: Counter = Counter()
    by_manifest: Counter = Counter()
    by_partition: Counter = Counter()
    by_semantic: Counter = Counter()
    by_source: dict[str, list[dict]] = defaultdict(list)

    args.failed_jsonl.parent.mkdir(parents=True, exist_ok=True)
    with args.failed_jsonl.open("w", encoding="utf-8", newline="\n") as f:
        for row in failed:
            error_family = classify_error(str(row.get("error", "")))
            row["error_family"] = error_family
            source_path = str(row.get("source_path") or "")
            by_error_family[error_family] += 1
            by_manifest[Path(str(row.get("_input_manifest", ""))).name] += 1
            by_partition[str(row.get("partition_hint", ""))] += 1
            by_semantic[str(row.get("semantic_bucket", ""))] += 1
            by_source[source_path].append(row)
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    source_rows = []
    for source_path, rows in by_source.items():
        families = Counter(str(row.get("error_family", "")) for row in rows)
        first = rows[0] if rows else {}
        source_rows.append(
            {
                "source_path": source_path,
                "failed_rows": len(rows),
                "error_families": dict(families),
                "partition_hint": first.get("partition_hint", ""),
                "semantic_bucket": first.get("semantic_bucket", ""),
                "source_exists": Path(source_path).exists() if source_path else False,
                "first_error": str(first.get("error", ""))[:500],
            }
        )
    source_rows.sort(key=lambda row: (-int(row["failed_rows"]), str(row["source_path"]).lower()))

    args.source_summary_jsonl.parent.mkdir(parents=True, exist_ok=True)
    with args.source_summary_jsonl.open("w", encoding="utf-8", newline="\n") as f:
        for row in source_rows:
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "input_stats": input_stats,
        "unique_latest_rows": len(latest),
        "failed_rows": len(failed),
        "failed_sources": len(source_rows),
        "by_error_family": dict(by_error_family),
        "by_manifest": dict(by_manifest),
        "by_partition": dict(by_partition),
        "by_semantic": dict(by_semantic),
        "top_sources": source_rows[:50],
        "failed_jsonl": str(args.failed_jsonl),
        "source_summary_jsonl": str(args.source_summary_jsonl),
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
