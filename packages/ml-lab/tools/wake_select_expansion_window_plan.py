#!/usr/bin/env python
"""Select a capped subset of expansion source rows before materialization."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from collections import Counter
from pathlib import Path


DEFAULT_CAPS = {
    "music_lyrics": 100_000,
    "music_instrumental": 15_000,
    "read_speech": 100_000,
    "unknown_negative": 50_000,
}


def stable_float(text: str) -> float:
    raw = hashlib.sha256(text.encode("utf-8", errors="replace")).digest()
    return int.from_bytes(raw[:8], "big") / float(2**64 - 1)


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def write_csv(path: Path, rows: list[dict[str, str]], fieldnames: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-plan-csv", type=Path, required=True)
    parser.add_argument("--output-csv", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--seed", default="wake_expansion_select_20260607_v1")
    for semantic, cap in DEFAULT_CAPS.items():
        parser.add_argument(f"--cap-{semantic.replace('_', '-')}", type=int, default=cap)
    args = parser.parse_args()

    caps = {
        "music_lyrics": args.cap_music_lyrics,
        "music_instrumental": args.cap_music_instrumental,
        "read_speech": args.cap_read_speech,
        "unknown_negative": args.cap_unknown_negative,
    }
    rows = read_csv(args.source_plan_csv)
    selected: list[dict[str, str]] = []
    selected_windows = Counter()
    seen_windows = Counter()
    selected_sources = Counter()
    seen_sources = Counter()

    ordered = sorted(
        rows,
        key=lambda row: stable_float(f"{args.seed}|{row.get('semantic_bucket')}|{row.get('source_id')}"),
    )
    for row in ordered:
        semantic = row.get("semantic_bucket", "unknown_negative")
        windows = int(row.get("planned_windows") or 0)
        seen_sources[semantic] += 1
        seen_windows[semantic] += windows
        cap = caps.get(semantic, 0)
        if windows <= 0 or cap <= 0:
            continue
        if selected_windows[semantic] + windows > cap:
            continue
        selected.append(row)
        selected_windows[semantic] += windows
        selected_sources[semantic] += 1

    fieldnames = list(rows[0].keys()) if rows else []
    selected = sorted(selected, key=lambda row: row.get("source_path", ""))
    write_csv(args.output_csv, selected, fieldnames)
    summary = {
        "source_plan_csv": str(args.source_plan_csv),
        "selected_plan_csv": str(args.output_csv),
        "seed": args.seed,
        "caps": caps,
        "seen_sources_by_semantic": dict(sorted(seen_sources.items())),
        "seen_windows_by_semantic": dict(sorted(seen_windows.items())),
        "selected_sources_by_semantic": dict(sorted(selected_sources.items())),
        "selected_windows_by_semantic": dict(sorted(selected_windows.items())),
        "selected_source_rows": len(selected),
        "selected_windows": sum(selected_windows.values()),
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
