#!/usr/bin/env python
"""Report E-drive wake-word data that belongs to old/non-Zephyr words."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

OLD_WORDS = ("shunya", "cipher", "cypher", "decipher", "quasar", "equinox")
AUDIO_EXTS = {".wav", ".mp3", ".flac", ".m4a", ".ogg", ".opus", ".aac"}


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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive-root", type=Path, default=Path(r"E:\heardright_archive"))
    parser.add_argument("--report-dir", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\reports"))
    args = parser.parse_args()

    rows = []
    candidate_dirs: list[Path] = []
    for path in args.archive_root.rglob("*"):
        lower = str(path).lower()
        matched = [word for word in OLD_WORDS if word in lower]
        if not matched:
            continue
        if path.is_dir():
            candidate_dirs.append(path)
        elif path.is_file() and path.suffix.lower() in AUDIO_EXTS:
            try:
                size = path.stat().st_size
            except OSError:
                size = 0
            rows.append(
                {
                    "kind": "file",
                    "path": str(path),
                    "matched_words": ",".join(matched),
                    "files": 1,
                    "bytes": size,
                    "gb": round(size / (1024**3), 6),
                    "recommendation": "review_old_word_audio",
                }
            )

    candidate_dirs = sorted(candidate_dirs, key=lambda p: (len(p.parts), str(p).lower()))
    top_dirs: list[Path] = []
    for path in candidate_dirs:
        if any(parent in path.parents for parent in top_dirs):
            continue
        top_dirs.append(path)

    for path in top_dirs:
        matched = [word for word in OLD_WORDS if word in str(path).lower()]
        files, bytes_total = dir_stats(path)
        rows.append(
            {
                "kind": "dir",
                "path": str(path),
                "matched_words": ",".join(matched),
                "files": files,
                "bytes": bytes_total,
                "gb": round(bytes_total / (1024**3), 6),
                "recommendation": "review_old_word_tree",
            }
        )

    rows.sort(key=lambda r: (-float(r["gb"]), r["kind"], r["path"]))
    args.report_dir.mkdir(parents=True, exist_ok=True)
    csv_path = args.report_dir / "e_drive_old_word_candidates.csv"
    json_path = args.report_dir / "e_drive_old_word_summary.json"
    with csv_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=["kind", "path", "matched_words", "files", "bytes", "gb", "recommendation"],
        )
        writer.writeheader()
        writer.writerows(rows)

    summary = {
        "archive_root": str(args.archive_root),
        "candidate_rows": len(rows),
        "candidate_dirs": sum(1 for r in rows if r["kind"] == "dir"),
        "candidate_files": sum(1 for r in rows if r["kind"] == "file"),
        "candidate_gb": round(sum(float(r["gb"]) for r in rows), 4),
        "csv": str(csv_path),
    }
    json_path.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
