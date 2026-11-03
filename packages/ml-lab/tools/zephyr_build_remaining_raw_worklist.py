#!/usr/bin/env python
"""Build a small conditioning worklist for remaining non-pico raw audio."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import subprocess
from pathlib import Path

import soundfile as sf


AUDIO_EXTS = {".wav", ".flac", ".mp3", ".m4a", ".ogg", ".opus", ".aac", ".wma", ".webm"}
SAMPLE_RATE = 16000


def stable_id(*parts: object) -> str:
    h = hashlib.sha256()
    for part in parts:
        h.update(str(part).encode("utf-8", errors="replace"))
        h.update(b"\0")
    return h.hexdigest()


def ffprobe_duration(path: Path) -> tuple[float, int]:
    cmd = [
        "ffprobe",
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=sample_rate",
        "-of",
        "json",
        str(path),
    ]
    data = json.loads(subprocess.check_output(cmd, text=True, encoding="utf-8", errors="replace"))
    duration = float(data.get("format", {}).get("duration") or 0.0)
    sample_rate = 0
    for stream in data.get("streams", []):
        if stream.get("sample_rate"):
            sample_rate = int(stream["sample_rate"])
            break
    return duration, sample_rate


def audio_duration(path: Path) -> tuple[float, int]:
    try:
        info = sf.info(str(path))
        return float(info.frames) / float(info.samplerate), int(info.samplerate)
    except Exception:
        return ffprobe_duration(path)


def classify(path: Path) -> tuple[int, str, str, str, str]:
    low = str(path).lower().replace("/", "\\")
    if "\\mic_soak_musan_aug\\zephyr_positive_musan_aug\\" in low:
        return 1, "zephyr_positive", "mic_soak_musan_aug", "mic_soak_musan_aug", "positive_full_clip"
    if "\\youtube\\fresh_scrape\\" in low:
        if "lincoln_zephyr" in low:
            return 0, "near_phonetic", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
        if "movie" in low or "tv_show" in low:
            return 0, "movie_drama", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
        if "podcast" in low or "conversation" in low:
            return 0, "podcast_interview", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
        if "news" in low:
            return 0, "news_broadcast", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
        if "tech" in low:
            return 0, "tech_tutorial", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
        return 0, "natural_conversation", "youtube_negative", "youtube_fresh_scrape", "negative_1p5s_window"
    raise ValueError(f"Unclassified remaining raw path: {path}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw-root", type=Path, default=Path(r"D:\Claude\heardright\train\corpus_expansion_20260607\raw"))
    parser.add_argument("--worklist-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    args = parser.parse_args()

    files = [
        path
        for path in sorted(args.raw_root.rglob("*"))
        if path.is_file() and path.suffix.lower() in AUDIO_EXTS
    ]

    args.worklist_jsonl.parent.mkdir(parents=True, exist_ok=True)
    rows = 0
    sources = []
    with args.worklist_jsonl.open("w", encoding="utf-8", newline="\n") as out:
        for path in files:
            label, semantic_bucket, source_bucket, provenance, output_kind = classify(path)
            duration_s, sample_rate = audio_duration(path)
            if label == 1:
                windows = [(0.0, min(3.0, duration_s), 0)]
            else:
                count = max(1, int(math.floor(duration_s / 1.5)))
                windows = [(idx * 1.5, (idx + 1) * 1.5, idx) for idx in range(count)]
            source_id = stable_id("remaining_raw_source", path, path.stat().st_size)
            for start_s, end_s, chunk_index in windows:
                row = {
                    "schema_version": 1,
                    "id": stable_id("remaining_raw_window", path, start_s, end_s, semantic_bucket, label),
                    "source_id": source_id,
                    "source_path": str(path),
                    "source_top_folder": "corpus_expansion_20260607",
                    "source_bucket": source_bucket,
                    "source_sample_rate": sample_rate,
                    "source_duration_s": round(duration_s, 6),
                    "source_plan_status": "remaining_raw_rescue",
                    "start_offset_s": round(start_s, 6),
                    "end_offset_s": round(end_s, 6),
                    "chunk_index": chunk_index,
                    "label": label,
                    "semantic_bucket": semantic_bucket,
                    "partition_hint": "unified_candidate",
                    "conditioning_policy": "adaptive_hot_v2_5",
                    "conditioned_path": "",
                    "output_kind": output_kind,
                    "label_sealed": True if label == 1 else "",
                    "target_window_policy": "",
                    "license_or_provenance": provenance,
                    "old_split": "remaining_raw_rescue_20260609",
                    "eligible_for_train": True,
                    "eligible_for_eval": True,
                    "eligible_for_mine": True,
                }
                out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
                rows += 1
            sources.append(
                {
                    "path": str(path),
                    "label": label,
                    "semantic_bucket": semantic_bucket,
                    "duration_s": round(duration_s, 3),
                    "windows": len(windows),
                }
            )

    summary = {
        "raw_root": str(args.raw_root),
        "sources": len(files),
        "rows": rows,
        "worklist_jsonl": str(args.worklist_jsonl),
        "examples": sources[:20],
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
