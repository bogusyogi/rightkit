#!/usr/bin/env python
"""Shard a conditioning worklist by source_id, excluding already manifested rows.

This keeps each raw source owned by one shard so delete-on-success remains safe.
Rows already present as ok in one or more manifests are omitted.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if line:
                yield json.loads(line)


def load_done_ids(paths: list[Path]) -> set[str]:
    done: set[str] = set()
    for path in paths:
        if not path.exists():
            continue
        for row in iter_jsonl(path):
            if row.get("ok") is True and row.get("id") and not row.get("error"):
                done.add(str(row["id"]))
    return done


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--worklist-jsonl", type=Path, required=True)
    parser.add_argument("--done-manifest-jsonl", type=Path, action="append", default=[])
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--shards", type=int, required=True)
    args = parser.parse_args()

    if args.shards < 1:
        raise SystemExit("--shards must be >= 1")

    done = load_done_ids(args.done_manifest_jsonl)
    source_counts: dict[str, int] = {}
    source_order: list[str] = []
    source_paths: dict[str, str] = {}
    total_rows = 0
    omitted_done = 0

    for row in iter_jsonl(args.worklist_jsonl):
        row_id = str(row["id"])
        if row_id in done:
            omitted_done += 1
            continue
        source_id = str(row.get("source_id") or row.get("source_path"))
        if source_id not in source_counts:
            source_counts[source_id] = 0
            source_order.append(source_id)
            source_paths[source_id] = str(row.get("source_path", ""))
        source_counts[source_id] += 1
        total_rows += 1

    shard_sources: list[list[str]] = [[] for _ in range(args.shards)]
    shard_rows = [0 for _ in range(args.shards)]
    for source_id in sorted(source_order, key=lambda sid: source_counts[sid], reverse=True):
        index = min(range(args.shards), key=lambda i: shard_rows[i])
        shard_sources[index].append(source_id)
        shard_rows[index] += source_counts[source_id]

    source_to_shard = {
        source_id: index
        for index, sources in enumerate(shard_sources)
        for source_id in sources
    }

    args.out_dir.mkdir(parents=True, exist_ok=True)
    shard_paths = [
        args.out_dir / f"{args.tag}_shard_{index:02d}_worklist.jsonl"
        for index in range(args.shards)
    ]
    handles = [path.open("w", encoding="utf-8", newline="\n") for path in shard_paths]
    written = [0 for _ in range(args.shards)]
    try:
        for row in iter_jsonl(args.worklist_jsonl):
            row_id = str(row["id"])
            if row_id in done:
                continue
            source_id = str(row.get("source_id") or row.get("source_path"))
            index = source_to_shard[source_id]
            handles[index].write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            written[index] += 1
    finally:
        for handle in handles:
            handle.close()

    summary = {
        "worklist_jsonl": str(args.worklist_jsonl),
        "done_manifest_jsonl": [str(path) for path in args.done_manifest_jsonl],
        "tag": args.tag,
        "shards": args.shards,
        "done_ids_loaded": len(done),
        "omitted_done_rows": omitted_done,
        "remaining_rows_materialized": total_rows,
        "remaining_sources": len(source_counts),
        "shard_rows": written,
        "shard_sources": [len(sources) for sources in shard_sources],
        "largest_sources": [
            {
                "source_id": source_id,
                "rows": source_counts[source_id],
                "source_path": source_paths[source_id],
            }
            for source_id in sorted(source_counts, key=source_counts.get, reverse=True)[:20]
        ],
        "shard_paths": [str(path) for path in shard_paths],
    }
    summary_path = args.out_dir / f"{args.tag}_shard_summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
