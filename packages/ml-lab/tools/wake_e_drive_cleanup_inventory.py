#!/usr/bin/env python
"""Inventory E-drive cleanup candidates after corpus expansion staging.

This script is deliberately report-only. Destructive cleanup should happen only
after the keep manifest has been verified against staged train/ data.
"""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


DEFAULT_ARCHIVE_ROOT = Path(r"E:\heardright_archive")
DEFAULT_EXPANSION_ROOT = Path(r"D:\Claude\heardright\train\corpus_expansion_20260607")

CACHE_NAME_EXACT = (
    "__pycache__",
    ".cache",
    "round3_cache",
    "round3_cache_smoke",
    "whisper_cache",
    "validate_chunks",
    "vast_xfer_chunks",
    "_chunks",
    "_repair_transcoded_for_vast",
)

CACHE_NAME_CONTAINS = (
    "cond_cache",
    "conditioned_cache",
    "canon_cache",
    "pending_chunks",
    "stream_cond",
    "repair_lists",
)

KEEP_HINTS = (
    "corpora",
    "tests\\music",
    "_yt_diverse_dl",
    "_yt_music_dl",
    "commonvoice-v24_en-au",
    "librispeech",
)


def dir_stats(path: Path) -> tuple[int, int]:
    files = 0
    bytes_total = 0
    for item in path.rglob("*"):
        if item.is_file():
            files += 1
            try:
                bytes_total += item.stat().st_size
            except OSError:
                pass
    return files, bytes_total


def is_cleanup_candidate(path: Path) -> bool:
    name = path.name.lower()
    return name in CACHE_NAME_EXACT or any(pattern in name for pattern in CACHE_NAME_CONTAINS)


def is_review_candidate(path: Path) -> bool:
    lower = str(path).lower()
    return any(token in lower for token in ("corpus_download", "eval_sets", "_negsrc", "tests\\music"))


def keep_reason(path: Path) -> str:
    lower = str(path).lower()
    for hint in KEEP_HINTS:
        if hint.lower() in lower:
            return f"raw_or_reacquirable_source:{hint}"
    return ""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive-root", type=Path, default=DEFAULT_ARCHIVE_ROOT)
    parser.add_argument("--expansion-root", type=Path, default=DEFAULT_EXPANSION_ROOT)
    args = parser.parse_args()

    report_dir = args.expansion_root / "reports"
    report_dir.mkdir(parents=True, exist_ok=True)
    candidates_csv = report_dir / "e_drive_cleanup_candidates.csv"
    summary_json = report_dir / "e_drive_cleanup_summary.json"

    candidate_paths: list[Path] = []
    for path in args.archive_root.rglob("*"):
        if not path.is_dir():
            continue
        if not is_cleanup_candidate(path):
            continue
        candidate_paths.append(path)

    # Keep only top-level candidates so size totals are not double-counted.
    candidate_paths = sorted(candidate_paths, key=lambda p: (len(p.parts), str(p).lower()))
    top_level: list[Path] = []
    for path in candidate_paths:
        if any(parent in path.parents for parent in top_level):
            continue
        top_level.append(path)

    rows = []
    for path in top_level:
        reason = keep_reason(path)
        files, bytes_total = dir_stats(path)
        if reason:
            recommendation = "keep_for_now"
        elif is_review_candidate(path):
            recommendation = "review_before_delete_raw_like"
        else:
            recommendation = "delete_candidate_cache_or_generated"
        rows.append(
            {
                "path": str(path),
                "files": files,
                "bytes": bytes_total,
                "gb": round(bytes_total / (1024**3), 4),
                "cleanup_recommendation": recommendation,
                "keep_reason": reason,
            }
        )

    rows.sort(key=lambda r: (-float(r["gb"]), r["path"]))
    with candidates_csv.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "path",
                "files",
                "bytes",
                "gb",
                "cleanup_recommendation",
                "keep_reason",
            ],
        )
        writer.writeheader()
        writer.writerows(rows)

    summary = {
        "archive_root": str(args.archive_root),
        "candidates_csv": str(candidates_csv),
        "candidate_dirs": len(rows),
        "delete_candidate_dirs": sum(1 for r in rows if r["cleanup_recommendation"].startswith("delete")),
        "delete_candidate_gb": round(
            sum(float(r["gb"]) for r in rows if r["cleanup_recommendation"].startswith("delete")),
            4,
        ),
        "keep_for_now_dirs": sum(1 for r in rows if r["cleanup_recommendation"] == "keep_for_now"),
        "review_before_delete_dirs": sum(1 for r in rows if r["cleanup_recommendation"].startswith("review")),
    }
    summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
