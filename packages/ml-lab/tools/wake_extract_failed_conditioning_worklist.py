#!/usr/bin/env python
"""Extract failed conditioning rows back into a retry worklist."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--conditioned-manifest", type=Path, required=True)
    parser.add_argument("--source-worklist", type=Path, required=True)
    parser.add_argument("--output-worklist", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    args = parser.parse_args()

    failed_ids = {
        str(row["id"])
        for row in iter_jsonl(args.conditioned_manifest)
        if row.get("id") and not row.get("ok")
    }
    ok_ids = {
        str(row["id"])
        for row in iter_jsonl(args.conditioned_manifest)
        if row.get("id") and row.get("ok")
    }
    retry_ids = failed_ids - ok_ids

    args.output_worklist.parent.mkdir(parents=True, exist_ok=True)
    rows = 0
    source_paths: set[str] = set()
    with args.output_worklist.open("w", encoding="utf-8", newline="\n") as out:
        for row in iter_jsonl(args.source_worklist):
            if str(row.get("id")) not in retry_ids:
                continue
            rows += 1
            source_paths.add(str(row.get("source_path", "")))
            out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    summary = {
        "failed_ids": len(failed_ids),
        "ok_ids": len(ok_ids),
        "retry_ids": len(retry_ids),
        "retry_rows_written": rows,
        "retry_source_paths": len(source_paths),
        "output_worklist": str(args.output_worklist),
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
