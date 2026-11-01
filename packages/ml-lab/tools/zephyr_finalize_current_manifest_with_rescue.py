#!/usr/bin/env python
"""Stream-finalize current packed source-truth manifest with rescue rows."""

from __future__ import annotations

import argparse
import json
import time
from collections import Counter
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-jsonl", type=Path, required=True)
    parser.add_argument("--rescue-jsonl", type=Path, required=True)
    parser.add_argument("--output-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--progress-json", type=Path, required=True)
    parser.add_argument("--progress-every", type=int, default=100_000)
    args = parser.parse_args()

    started = time.time()
    args.output_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.progress_json.parent.mkdir(parents=True, exist_ok=True)

    counters = Counter()
    by_input = Counter()
    by_semantic = Counter()
    by_source_delete = Counter()
    seen_ids = set()
    duplicate_ids = 0
    path_exists_cache: dict[str, bool] = {}
    missing_outputs: list[dict] = []

    inputs = [args.base_jsonl, args.rescue_jsonl]
    with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as out:
        for input_path in inputs:
            for row in iter_jsonl(input_path):
                row_id = str(row.get("id") or "")
                if row_id:
                    if row_id in seen_ids:
                        duplicate_ids += 1
                    seen_ids.add(row_id)
                out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
                counters["rows"] += 1
                by_input[input_path.name] += 1
                if row.get("ok"):
                    counters["ok_rows"] += 1
                    shard_path = str(row.get("conditioned_shard_path") or row.get("conditioned_path") or "")
                    if shard_path:
                        exists = path_exists_cache.get(shard_path)
                        if exists is None:
                            exists = Path(shard_path).exists()
                            path_exists_cache[shard_path] = exists
                        if not exists and len(missing_outputs) < 100:
                            missing_outputs.append({"id": row_id, "path": shard_path})
                            counters["missing_output_count"] += 1
                    else:
                        counters["missing_output_count"] += 1
                else:
                    counters["error_rows"] += 1
                by_semantic[str(row.get("semantic_bucket", ""))] += 1
                by_source_delete[str(row.get("source_delete_status", ""))] += 1
                if counters["rows"] % args.progress_every == 0:
                    args.progress_json.write_text(
                        json.dumps(
                            {
                                "elapsed_s": round(time.time() - started, 3),
                                "rows": counters["rows"],
                                "ok_rows": counters["ok_rows"],
                                "error_rows": counters["error_rows"],
                                "current_input": str(input_path),
                            },
                            indent=2,
                            sort_keys=True,
                        ),
                        encoding="utf-8",
                    )

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "output_manifest": str(args.output_jsonl),
        "inputs": [str(path) for path in inputs],
        "by_input": dict(by_input),
        "unique_ids": len(seen_ids),
        "duplicate_ids": duplicate_ids,
        "rows": counters["rows"],
        "ok_rows": counters["ok_rows"],
        "error_rows": counters["error_rows"],
        "missing_output_count": counters["missing_output_count"],
        "missing_output_examples": missing_outputs,
        "by_semantic": dict(by_semantic),
        "source_delete_status_counts": dict(by_source_delete),
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    args.progress_json.write_text(json.dumps({**summary, "done": True}, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if duplicate_ids == 0 and counters["missing_output_count"] == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
