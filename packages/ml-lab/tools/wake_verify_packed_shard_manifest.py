#!/usr/bin/env python
"""Verify packed PCM16 shard manifest rows point to readable shard byte ranges."""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line_no, line in enumerate(f, 1):
            line = line.strip()
            if line:
                yield line_no, json.loads(line)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--limit-rows", type=int, default=0)
    args = parser.parse_args()

    counters = Counter()
    examples = []
    for line_no, row in iter_jsonl(args.manifest_jsonl):
        if args.limit_rows and counters["rows"] >= args.limit_rows:
            break
        counters["rows"] += 1
        if not row.get("ok"):
            counters["error_rows"] += 1
            continue
        shard_path = Path(str(row.get("conditioned_shard_path") or row.get("conditioned_path") or ""))
        try:
            offset = int(row.get("conditioned_shard_offset_bytes"))
            nbytes = int(row.get("conditioned_shard_nbytes"))
        except Exception:
            counters["bad_range_rows"] += 1
            examples.append({"line": line_no, "id": row.get("id"), "reason": "bad offset/nbytes"})
            continue
        if not shard_path.exists():
            counters["missing_shard_rows"] += 1
            examples.append({"line": line_no, "id": row.get("id"), "reason": "missing shard", "path": str(shard_path)})
            continue
        size = shard_path.stat().st_size
        if offset < 0 or nbytes <= 0 or offset + nbytes > size:
            counters["bad_range_rows"] += 1
            examples.append({"line": line_no, "id": row.get("id"), "reason": "range outside shard", "path": str(shard_path), "size": size, "offset": offset, "nbytes": nbytes})
            continue
        with shard_path.open("rb") as f:
            f.seek(offset)
            payload = f.read(min(nbytes, 64))
        if not payload:
            counters["unreadable_rows"] += 1
            examples.append({"line": line_no, "id": row.get("id"), "reason": "empty read", "path": str(shard_path)})
            continue
        counters["verified_rows"] += 1

    summary = {
        "manifest_jsonl": str(args.manifest_jsonl),
        "counters": dict(sorted(counters.items())),
        "examples": examples[:25],
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if counters["rows"] == counters["verified_rows"] + counters["error_rows"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
