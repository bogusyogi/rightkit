#!/usr/bin/env python
"""Summarize the top-level train source-of-truth layout."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

AUDIO_EXTS = {".wav", ".mp3", ".flac", ".m4a", ".ogg", ".opus", ".aac", ".mp4", ".mkv"}


def summarize_dir(path: Path) -> dict:
    files = 0
    audio = 0
    bytes_total = 0
    truncated = False
    if path.exists():
        for item in path.rglob("*"):
            if not item.is_file():
                continue
            files += 1
            if item.suffix.lower() in AUDIO_EXTS:
                audio += 1
            try:
                bytes_total += item.stat().st_size
            except OSError:
                pass
            if files >= 200_000:
                truncated = True
                break
    return {
        "path": str(path),
        "exists": path.exists(),
        "files": files,
        "audio_files": audio,
        "gb": round(bytes_total / (1024**3), 4),
        "truncated_at_200k_files": truncated,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--train-root", type=Path, default=Path(r"D:\Claude\heardright\train"))
    parser.add_argument("--output-json", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\reports\train_source_truth_layout.json"))
    args = parser.parse_args()

    names = [
        "positives",
        "youtube",
        "music",
        "music-lyric",
        "music_review_needed",
        "audiobooks",
        "tv",
        "pico",
        "campaigns",
        "corpus_expansion_20260607",
    ]
    summary = {name: summarize_dir(args.train_root / name) for name in names}
    args.output_json.parent.mkdir(parents=True, exist_ok=True)
    args.output_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
