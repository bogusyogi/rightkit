#!/usr/bin/env python
"""Build manifests for raw corpus expansion sources staged under train/.

The script does not condition or split audio. It records raw files that are
eligible to enter the next Phase 0 rebuild after adaptive conditioning.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from collections import Counter
from pathlib import Path
from typing import Iterable


DEFAULT_ROOT = Path(r"D:\Claude\heardright\train\corpus_expansion_20260607")
AUDIO_EXTS = {".wav", ".mp3", ".flac", ".m4a", ".aa", ".ogg"}


def sha256_file(path: Path, chunk_size: int = 1024 * 1024) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(chunk_size), b""):
            h.update(chunk)
    return h.hexdigest()


def stable_id(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()


def iter_audio(root: Path) -> Iterable[Path]:
    if not root.exists():
        return
    for path in root.rglob("*"):
        if path.is_file() and path.suffix.lower() in AUDIO_EXTS:
            yield path


def classify(path: Path, expansion_root: Path) -> tuple[str, str, str, str]:
    rel = path.relative_to(expansion_root)
    parts = rel.parts
    raw_kind = parts[0] if parts else "unknown"
    lower = str(rel).lower()

    if raw_kind == "music_archive":
        source_bucket = "music_archive_tests"
        if any(tok in lower for tok in ("instrumental", "ambient", "karaoke", "score", "ost", "soundtrack")):
            semantic = "music_instrumental"
        else:
            semantic = "music_lyrics"
        provenance = "local_archive_tests_music"
    elif raw_kind == "librispeech":
        subset = next((p for p in parts if p in {"dev-clean", "dev-other", "test-clean", "test-other", "train-clean-100", "train-clean-360", "train-other-500"}), "unknown")
        source_bucket = f"librispeech_{subset}"
        semantic = "read_speech"
        provenance = "openslr_librispeech"
    elif raw_kind == "common_voice":
        source_bucket = "common_voice_v24_en_au"
        semantic = "read_speech"
        provenance = "mozilla_common_voice_v24_en_au"
    elif raw_kind == "google_speech_commands":
        source_bucket = "google_speech_commands"
        semantic = "read_speech"
        provenance = "google_speech_commands"
    elif raw_kind == "youtube":
        source_bucket = "youtube_expansion"
        semantic = "unknown_negative"
        provenance = "youtube_downloaded_for_internal_training"
    elif raw_kind == "near_phonetic_tts":
        source_bucket = "sapi_tts_near_phonetic"
        semantic = "near_phonetic"
        provenance = "local_sapi_tts_near_phonetic"
    elif raw_kind == "near_phonetic_real":
        source_bucket = "real_near_phonetic"
        semantic = "near_phonetic"
        provenance = "local_common_voice_near_phonetic_stage"
    elif raw_kind == "mic_soak_musan_aug":
        source_bucket = "mic_soak_musan_aug"
        semantic = "zephyr_positive_musan_aug" if "zephyr_positive" in lower else "near_phonetic_musan_aug"
        provenance = "internal_mic_soak_plus_local_musan_aug"
    else:
        source_bucket = f"expansion_{raw_kind}"
        semantic = "unknown_negative"
        provenance = "local_expansion"
    return raw_kind, source_bucket, semantic, provenance


def build_rows(expansion_root: Path, compute_sha256: bool) -> Iterable[dict]:
    raw_root = expansion_root / "raw"
    for path in iter_audio(raw_root):
        raw_kind, source_bucket, semantic, provenance = classify(path, raw_root)
        rel = path.relative_to(expansion_root)
        row_id = stable_id(str(rel).replace("\\", "/"))
        stat = path.stat()
        label = 1 if semantic == "zephyr_positive_musan_aug" else 0
        yield {
            "schema_version": 1,
            "id": row_id,
            "path": str(path),
            "rel_path": str(rel),
            "label": label,
            "word": "zephyr" if label == 1 else "",
            "source_bucket": source_bucket,
            "semantic_bucket": semantic,
            "source_kind": raw_kind,
            "old_split": "new_expansion",
            "eligible_for_train": True,
            "eligible_for_eval": True,
            "eligible_for_mine": True,
            "partition_hint": "unified_candidate",
            "false_fire_type": "real_wake" if label == 1 else "not_applicable",
            "license_or_provenance": provenance,
            "sha256": sha256_file(path) if compute_sha256 else "",
            "bytes": stat.st_size,
            "extension": path.suffix.lower(),
        }


def write_jsonl(path: Path, rows: Iterable[dict]) -> int:
    n = 0
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            n += 1
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
    return n


def summarize(manifest_path: Path) -> dict:
    counters = Counter()
    bytes_by_kind = Counter()
    with manifest_path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            counters["rows"] += 1
            counters[f"source_kind:{row['source_kind']}"] += 1
            counters[f"semantic_bucket:{row['semantic_bucket']}"] += 1
            counters[f"source_bucket:{row['source_bucket']}"] += 1
            bytes_by_kind[row["source_kind"]] += int(row.get("bytes") or 0)
    return {
        "manifest_jsonl": str(manifest_path),
        "counters": dict(sorted(counters.items())),
        "bytes_by_source_kind": dict(sorted(bytes_by_kind.items())),
    }


def write_coverage_csv(path: Path, summary: dict) -> None:
    rows = []
    for key, count in summary["counters"].items():
        if ":" not in key:
            continue
        group, value = key.split(":", 1)
        rows.append({"group": group, "value": value, "rows": count})
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["group", "value", "rows"])
        writer.writeheader()
        writer.writerows(sorted(rows, key=lambda r: (r["group"], r["value"])))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expansion-root", type=Path, default=DEFAULT_ROOT)
    parser.add_argument("--compute-sha256", action="store_true")
    args = parser.parse_args()

    manifest = args.expansion_root / "manifests" / "raw_expansion_manifest.jsonl"
    report_json = args.expansion_root / "reports" / "raw_expansion_summary.json"
    coverage_csv = args.expansion_root / "reports" / "raw_expansion_coverage.csv"
    count = write_jsonl(manifest, build_rows(args.expansion_root, args.compute_sha256))
    summary = summarize(manifest)
    summary["rows_written"] = count
    summary["compute_sha256"] = bool(args.compute_sha256)
    report_json.parent.mkdir(parents=True, exist_ok=True)
    report_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    write_coverage_csv(coverage_csv, summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
