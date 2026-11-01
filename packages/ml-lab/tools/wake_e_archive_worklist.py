#!/usr/bin/env python
"""Build a conditioning worklist for legacy E: wake-train archive audio.

This intentionally treats the archive as a source of raws to drain into the
current D: source-truth conditioned corpus. Most old archive audio is negative
context/eval/cache material; only explicit positive corpus paths are labeled as
Zephyr positives.
"""

from __future__ import annotations

import argparse
import json
import math
import shutil
import subprocess
from collections import Counter
from hashlib import sha256
from pathlib import Path
from typing import Iterable

try:
    import soundfile as sf
except Exception:  # pragma: no cover - runtime guard
    sf = None


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
    ".sph",
    ".wav",
    ".webm",
    ".wma",
}
WINDOW_S = 1.5
POLICY = "adaptive_hot_v2_5"
SKIP_AUDIO_PARTS = {
    "_vast_pull",
    "_wavlm_distills",
    "ambient_fp_test",
    "chunks",
    "logs",
    "logs_4090",
    "replay_dbg",
    "replay_dbg2",
    "soak_vad_dual",
    "v16_results",
    "v18_last_shot",
}
SKIP_AUDIO_SUBSTRINGS = (
    "\\cond_cache\\",
    "\\conditioned\\",
    "\\distill",
    "\\gate_epoch",
    "\\logs\\",
    "\\model",
    "\\v18_last_shot\\chunks\\",
    "\\wavlm",
)


def stable_id(parts: Iterable[object]) -> str:
    return sha256("|".join(str(part) for part in parts).encode("utf-8", errors="replace")).hexdigest()


def iter_audio(root: Path) -> Iterable[Path]:
    for path in root.rglob("*"):
        rel = str(path.relative_to(root)).replace("/", "\\").lower()
        parts = set(rel.split("\\"))
        if parts & SKIP_AUDIO_PARTS:
            continue
        if any(fragment in f"\\{rel}" for fragment in SKIP_AUDIO_SUBSTRINGS):
            continue
        if path.is_file() and path.suffix.lower() in AUDIO_EXTS:
            yield path


def probe_duration(path: Path) -> tuple[bool, float | None, int | None, str]:
    if sf is not None:
        try:
            with sf.SoundFile(str(path)) as f:
                return True, float(len(f)) / float(f.samplerate), int(f.samplerate), "soundfile"
        except Exception:
            pass
    ffprobe = shutil.which("ffprobe")
    if not ffprobe:
        return False, None, None, "ffprobe_missing"
    cmd = [
        ffprobe,
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=sample_rate:format=duration",
        "-of",
        "json",
        str(path),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        return False, None, None, (proc.stderr or proc.stdout or "ffprobe_failed").strip()[:300]
    data = json.loads(proc.stdout or "{}")
    duration = data.get("format", {}).get("duration")
    streams = data.get("streams") or []
    sample_rate = streams[0].get("sample_rate") if streams else None
    return (
        duration not in (None, "N/A", ""),
        float(duration) if duration not in (None, "N/A", "") else None,
        int(sample_rate) if sample_rate not in (None, "N/A", "") else None,
        "ffprobe",
    )


def classify(root: Path, path: Path) -> tuple[int, str, str]:
    rel = str(path.relative_to(root)).replace("/", "\\").lower()
    if (
        "\\corpus\\_pos_augmented\\" in f"\\{rel}"
        or "\\_corpus_rebuild\\_pos_final\\" in f"\\{rel}"
        or "\\_corpus_rebuild\\pos_keep_base\\" in f"\\{rel}"
        or "\\hr_zephyr_corpus_v1\\audio\\" in f"\\{rel}" and "\\pos" in f"\\{rel}"
    ):
        return 1, "zephyr_positive", "legacy_archive_positive"
    if "\\_corpus_rebuild\\_musan_subset\\" in f"\\{rel}" or "\\musan" in f"\\{rel}":
        return 0, "silence_ambient", "legacy_archive_musan"
    if "\\_corpus_rebuild\\_prod_negs\\" in f"\\{rel}" or "\\_corpus_rebuild\\_mic_negs\\" in f"\\{rel}":
        return 0, "natural_conversation", "legacy_archive_negative"
    if "_yt_music" in rel or "music" in rel:
        return 0, "music_lyrics", "legacy_archive_music"
    if "tedlium" in rel or "librispeech" in rel or "voxconverse" in rel or "speech" in rel:
        return 0, "read_speech", "legacy_archive_speech"
    if "ambient" in rel or "fire_clips" in rel or "noise" in rel:
        return 0, "silence_ambient", "legacy_archive_ambient_false_fire"
    if "youtube" in rel or "_yt_" in rel or "yt_" in rel:
        return 0, "near_phonetic", "legacy_archive_youtube"
    return 0, "legacy_archive_negative", "legacy_archive_audio"


def source_manifest_row(root: Path, path: Path, ok: bool, duration_s: float | None, sample_rate: int | None, probe: str) -> dict:
    label, semantic, bucket = classify(root, path)
    return {
        "path": str(path),
        "exists": path.exists(),
        "bytes": path.stat().st_size if path.exists() else 0,
        "ext": path.suffix.lower(),
        "top_folder": "e_archive",
        "source_kind": "e_archive",
        "source_bucket": bucket,
        "semantic_bucket": semantic,
        "label": label,
        "ok": ok,
        "duration_s": duration_s,
        "sample_rate": sample_rate,
        "probe_method": probe,
        "license_or_provenance": "legacy_e_wake_train_a5_2026_05_11",
        "old_split": "e_archive_raw_or_cache",
    }


def worklist_rows(meta: dict) -> list[dict]:
    if not meta["ok"] or not meta["duration_s"]:
        return []
    duration = float(meta["duration_s"])
    label = int(meta["label"])
    source_path = meta["path"]
    source_id = stable_id([source_path])
    if label == 1:
        if duration > 3.0:
            return []
        offsets = [(0, 0.0, duration, "positive_full_clip")]
    else:
        count = max(1, int(math.floor(duration / WINDOW_S)))
        offsets = [(i, i * WINDOW_S, (i * WINDOW_S) + WINDOW_S, "negative_1p5s_window") for i in range(count)]
    rows: list[dict] = []
    for chunk_index, start_s, end_s, kind in offsets:
        rows.append(
            {
                "schema_version": 1,
                "id": stable_id([source_path.lower(), f"{start_s:.6f}", f"{end_s:.6f}", label, kind]),
                "source_id": source_id,
                "source_path": source_path,
                "source_top_folder": "e_archive",
                "source_bucket": meta["source_bucket"],
                "partition_hint": "legacy_archive",
                "semantic_bucket": meta["semantic_bucket"],
                "label": label,
                "chunk_index": chunk_index,
                "start_offset_s": round(start_s, 6),
                "end_offset_s": round(end_s, 6),
                "source_duration_s": duration,
                "source_sample_rate": meta["sample_rate"],
                "conditioning_policy": POLICY,
                "conditioned_path": "",
                "output_kind": kind,
                "source_plan_status": "legacy_archive_positive" if label == 1 else "segment_negative",
                "target_window_policy": "",
                "label_sealed": "",
            }
        )
    return rows


def write_jsonl(path: Path, rows: Iterable[dict]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    count = 0
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            count += 1
    return count


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive-root", type=Path, required=True)
    parser.add_argument("--raw-metadata-jsonl", type=Path, required=True)
    parser.add_argument("--worklist-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    args = parser.parse_args()

    counters: Counter = Counter()
    args.raw_metadata_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.worklist_jsonl.parent.mkdir(parents=True, exist_ok=True)
    metadata_count = 0
    worklist_count = 0
    with args.raw_metadata_jsonl.open("w", encoding="utf-8", newline="\n") as meta_f, args.worklist_jsonl.open(
        "w", encoding="utf-8", newline="\n"
    ) as work_f:
        for path in iter_audio(args.archive_root):
            ok, duration_s, sample_rate, probe = probe_duration(path)
            row = source_manifest_row(args.archive_root, path, ok, duration_s, sample_rate, probe)
            meta_f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            metadata_count += 1
            counters["audio_files"] += 1
            counters[f"ext:{path.suffix.lower()}"] += 1
            counters[f"label:{row['label']}"] += 1
            counters[f"semantic:{row['semantic_bucket']}"] += 1
            counters[f"probe:{probe}"] += 1
            if not ok:
                counters["probe_errors"] += 1
            rows = worklist_rows(row)
            if not rows and row.get("label") == 1:
                counters["positive_skipped_needs_centering"] += 1
            for work_row in rows:
                work_f.write(json.dumps(work_row, ensure_ascii=False, sort_keys=True) + "\n")
                worklist_count += 1
    summary = {
        "archive_root": str(args.archive_root),
        "raw_metadata_jsonl": str(args.raw_metadata_jsonl),
        "worklist_jsonl": str(args.worklist_jsonl),
        "metadata_rows": metadata_count,
        "worklist_rows": worklist_count,
        "counters": dict(sorted(counters.items())),
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if counters["probe_errors"] == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
