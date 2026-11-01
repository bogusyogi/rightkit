#!/usr/bin/env python
"""Stream failed rows from packed manifests without building a full row map.

Read-only helper for source-truth cleanup. It scans JSONL manifests, parses
only rows that contain `"ok": false`, and writes incremental progress so the
chat can stay responsive while the scan runs in the background.
"""

from __future__ import annotations

import argparse
import json
import time
from collections import Counter, defaultdict
from pathlib import Path


def classify_error(error: str) -> str:
    low = error.lower()
    if "timed out" in low:
        return "decode_timeout"
    if "no such file" in low or "cannot find" in low or "not found" in low:
        return "missing_source"
    if "invalid data" in low or "could not find codec" in low:
        return "decode_invalid_or_codec"
    if "bad_json" in low:
        return "bad_json"
    return "other"


def write_json(path: Path, data: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, action="append", required=True)
    parser.add_argument("--failed-jsonl", type=Path, required=True)
    parser.add_argument("--source-summary-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--progress-json", type=Path, required=True)
    parser.add_argument("--progress-every-lines", type=int, default=100_000)
    args = parser.parse_args()

    started = time.time()
    args.failed_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.source_summary_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.progress_json.parent.mkdir(parents=True, exist_ok=True)

    totals = Counter()
    by_manifest = Counter()
    by_family = Counter()
    by_partition = Counter()
    by_semantic = Counter()
    source_rows: dict[str, dict] = {}

    with args.failed_jsonl.open("w", encoding="utf-8", newline="\n") as failed_out:
        for manifest_index, manifest in enumerate(args.manifest):
            manifest_rows = 0
            manifest_failed = 0
            with manifest.open("r", encoding="utf-8", errors="replace") as f:
                for line_no, line in enumerate(f, 1):
                    manifest_rows += 1
                    totals["rows_scanned"] += 1
                    if '"ok": false' not in line:
                        if totals["rows_scanned"] % args.progress_every_lines == 0:
                            write_json(
                                args.progress_json,
                                {
                                    "elapsed_s": round(time.time() - started, 3),
                                    "current_manifest": str(manifest),
                                    "line_no": line_no,
                                    "rows_scanned": totals["rows_scanned"],
                                    "failed_rows": totals["failed_rows"],
                                    "failed_sources": len(source_rows),
                                },
                            )
                        continue
                    try:
                        row = json.loads(line)
                    except json.JSONDecodeError as exc:
                        row = {"id": f"{manifest}:{line_no}", "ok": False, "error": f"bad_json:{exc}", "source_path": ""}
                    family = classify_error(str(row.get("error", "")))
                    source_path = str(row.get("source_path") or "")
                    row["error_family"] = family
                    row["_input_manifest"] = str(manifest)
                    row["_input_manifest_index"] = manifest_index
                    row["_input_line_no"] = line_no
                    failed_out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

                    manifest_failed += 1
                    totals["failed_rows"] += 1
                    by_manifest[manifest.name] += 1
                    by_family[family] += 1
                    by_partition[str(row.get("partition_hint", ""))] += 1
                    by_semantic[str(row.get("semantic_bucket", ""))] += 1
                    item = source_rows.setdefault(
                        source_path,
                        {
                            "source_path": source_path,
                            "failed_rows": 0,
                            "error_families": defaultdict(int),
                            "partition_hint": row.get("partition_hint", ""),
                            "semantic_bucket": row.get("semantic_bucket", ""),
                            "first_error": str(row.get("error", ""))[:500],
                        },
                    )
                    item["failed_rows"] += 1
                    item["error_families"][family] += 1
            totals[f"manifest_rows:{manifest.name}"] = manifest_rows
            totals[f"manifest_failed:{manifest.name}"] = manifest_failed
            write_json(
                args.progress_json,
                {
                    "elapsed_s": round(time.time() - started, 3),
                    "current_manifest": str(manifest),
                    "manifest_done": True,
                    "rows_scanned": totals["rows_scanned"],
                    "failed_rows": totals["failed_rows"],
                    "failed_sources": len(source_rows),
                },
            )

    source_list = []
    for item in source_rows.values():
        item = dict(item)
        item["error_families"] = dict(item["error_families"])
        source_path = item.get("source_path") or ""
        item["source_exists"] = Path(source_path).exists() if source_path else False
        source_list.append(item)
    source_list.sort(key=lambda row: (-int(row["failed_rows"]), str(row["source_path"]).lower()))

    with args.source_summary_jsonl.open("w", encoding="utf-8", newline="\n") as source_out:
        for row in source_list:
            source_out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "rows_scanned": totals["rows_scanned"],
        "failed_rows": totals["failed_rows"],
        "failed_sources": len(source_list),
        "by_manifest": dict(by_manifest),
        "by_error_family": dict(by_family),
        "by_partition": dict(by_partition),
        "by_semantic": dict(by_semantic),
        "top_sources": source_list[:50],
        "failed_jsonl": str(args.failed_jsonl),
        "source_summary_jsonl": str(args.source_summary_jsonl),
    }
    write_json(args.summary_json, summary)
    write_json(args.progress_json, {**summary, "done": True})
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
