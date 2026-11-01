#!/usr/bin/env python
"""Report raw sources that are covered by conditioned rows and raw sources not yet covered."""

from __future__ import annotations

import argparse
import csv
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
    parser.add_argument("--raw-manifest", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\manifests\raw_expansion_manifest.jsonl"))
    parser.add_argument("--conditioned-manifest", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\manifests\raw_expansion_conditioned_manifest_merged.jsonl"))
    parser.add_argument("--out-csv", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\reports\raw_delete_candidate_report.csv"))
    parser.add_argument("--summary-json", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\reports\raw_delete_candidate_summary.json"))
    args = parser.parse_args()

    conditioned_sources: set[str] = set()
    for row in iter_jsonl(args.conditioned_manifest):
        if row.get("ok") and row.get("source_path"):
            conditioned_sources.add(str(Path(str(row["source_path"]))).replace("/", "\\").lower())

    rows = []
    summary: dict[str, dict[str, int]] = {}
    for row in iter_jsonl(args.raw_manifest):
        raw_path = Path(str(row["path"]))
        key = str(raw_path).replace("/", "\\").lower()
        covered = key in conditioned_sources
        source_kind = str(row.get("source_kind", "unknown"))
        stat_bytes = int(row.get("bytes") or (raw_path.stat().st_size if raw_path.exists() else 0))
        bucket = summary.setdefault(source_kind, {"raw_files": 0, "covered_files": 0, "uncovered_files": 0, "covered_bytes": 0, "uncovered_bytes": 0})
        bucket["raw_files"] += 1
        if covered:
            bucket["covered_files"] += 1
            bucket["covered_bytes"] += stat_bytes
        else:
            bucket["uncovered_files"] += 1
            bucket["uncovered_bytes"] += stat_bytes
        rows.append(
            {
                "source_kind": source_kind,
                "semantic_bucket": row.get("semantic_bucket", ""),
                "covered_by_conditioned_manifest": covered,
                "bytes": stat_bytes,
                "path": str(raw_path),
            }
        )

    args.out_csv.parent.mkdir(parents=True, exist_ok=True)
    with args.out_csv.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["source_kind", "semantic_bucket", "covered_by_conditioned_manifest", "bytes", "path"])
        writer.writeheader()
        writer.writerows(rows)

    output = {
        "conditioned_unique_sources": len(conditioned_sources),
        "csv": str(args.out_csv),
        "by_source_kind": {
            k: {
                **v,
                "covered_gb": round(v["covered_bytes"] / (1024**3), 4),
                "uncovered_gb": round(v["uncovered_bytes"] / (1024**3), 4),
            }
            for k, v in sorted(summary.items())
        },
    }
    args.summary_json.write_text(json.dumps(output, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
