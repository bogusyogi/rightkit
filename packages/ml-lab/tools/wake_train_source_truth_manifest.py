#!/usr/bin/env python
"""Build a raw source-of-truth manifest from D:\\Claude\\heardright\\train."""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

AUDIO_EXTS = {
    ".aac",
    ".aif",
    ".aiff",
    ".flac",
    ".m4a",
    ".mkv",
    ".mov",
    ".mp3",
    ".mp4",
    ".ogg",
    ".opus",
    ".wav",
    ".webm",
}


def stable_id(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()


def iter_audio(root: Path):
    if not root.exists():
        return
    for path in root.rglob("*"):
        if path.is_file() and path.suffix.lower() in AUDIO_EXTS:
            yield path


def youtube_semantic(path: Path) -> str:
    lower = str(path).lower()
    if "podcast" in lower or "rogan" in lower or "huberman" in lower or "diary_of_a_ceo" in lower:
        return "podcast_interview"
    if "lincoln_zephyr" in lower or "zephyr_title" in lower:
        return "near_phonetic"
    if "comedy" in lower or "stand_up" in lower:
        return "comedy_standup"
    if "movie" in lower:
        return "movie_drama"
    if "tv_show" in lower:
        return "movie_drama"
    if "tech" in lower or "tutorial" in lower or "coding" in lower:
        return "tech_tutorial"
    if "news" in lower:
        return "news_broadcast"
    if "conversation" in lower:
        return "natural_conversation"
    return "natural_conversation"


def rows_for_root(train_root: Path, only_folders: set[str] | None = None):
    roots = [
        ("positives", "zephyr_positive", 1, "zephyr"),
        ("youtube", None, 0, ""),
        ("music", "music_instrumental", 0, ""),
        ("music-lyric", "music_lyrics", 0, ""),
        ("music_review_needed", "music_review_needed", 0, ""),
        ("audiobooks", "read_speech", 0, ""),
        ("masterclass", "lecture_instruction", 0, ""),
        ("tv", "movie_drama", 0, ""),
    ]
    for folder, fixed_semantic, label, word in roots:
        if only_folders and folder not in only_folders:
            continue
        root = train_root / folder
        for path in iter_audio(root):
            rel = path.relative_to(train_root)
            semantic = fixed_semantic or youtube_semantic(path)
            if folder == "youtube":
                source_bucket = "youtube_negative"
                provenance = "youtube_downloaded_for_internal_training"
            elif folder == "positives":
                source_bucket = "unified_zephyr_positive"
                provenance = "legacy_and_new_zephyr_positive"
            else:
                source_bucket = folder.replace("-", "_")
                provenance = f"train_source_truth_{folder}"
            stat = path.stat()
            yield {
                "schema_version": 1,
                "id": stable_id(str(rel).replace("\\", "/")),
                "path": str(path),
                "rel_path": str(rel),
                "label": label,
                "word": word,
                "source_bucket": source_bucket,
                "semantic_bucket": semantic,
                "source_kind": folder,
                "old_split": "source_truth_raw",
                "eligible_for_train": True,
                "eligible_for_eval": True,
                "eligible_for_mine": True,
                "partition_hint": "unified_candidate",
                "false_fire_type": "real_wake" if label == 1 else "not_applicable",
                "license_or_provenance": provenance,
                "bytes": stat.st_size,
                "extension": path.suffix.lower(),
            }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--train-root", type=Path, default=Path(r"D:\Claude\heardright\train"))
    parser.add_argument("--output-jsonl", type=Path, default=Path(r"D:\Claude\heardright\train\manifests\source_truth_raw_manifest.jsonl"))
    parser.add_argument("--summary-json", type=Path, default=Path(r"D:\Claude\heardright\train\reports\source_truth_raw_manifest_summary.json"))
    parser.add_argument("--only-folder", action="append", default=[], help="Limit manifest rows to one source-truth top folder; repeatable.")
    args = parser.parse_args()

    args.output_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    counters = Counter()
    bytes_by_kind = Counter()
    only_folders = set(args.only_folder) if args.only_folder else None
    with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows_for_root(args.train_root, only_folders):
            counters["rows"] += 1
            counters[f"source_kind:{row['source_kind']}"] += 1
            counters[f"semantic_bucket:{row['semantic_bucket']}"] += 1
            counters[f"label:{row['label']}"] += 1
            bytes_by_kind[row["source_kind"]] += int(row["bytes"])
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
    summary = {
        "manifest": str(args.output_jsonl),
        "counters": dict(sorted(counters.items())),
        "bytes_by_source_kind": dict(sorted(bytes_by_kind.items())),
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
