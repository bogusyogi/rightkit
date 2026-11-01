from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path
import re


KNOWN_TTS_PREFIXES = (
    "piper_",
    "edge_tts_",
    "parler_tts_",
    "kokoro_",
    "chatterbox_",
    "styletts",
    "coqui",
    "dia_",
    "f5_",
)


def parse_csv(value: str) -> set[str]:
    return {item.strip() for item in value.split(",") if item.strip()}


def looks_like_tts(row: dict) -> bool:
    source_bucket = str(row.get("source_bucket") or "").lower()
    semantic_bucket = str(row.get("semantic_bucket") or "").lower()
    family = str(row.get("tts_family") or row.get("engine") or "").lower()
    source_path = str(row.get("source_path") or "").lower()
    audio_path = str(row.get("audio_path") or "").lower()

    if family:
        return True
    if "tts" in source_bucket:
        return True
    if source_bucket.startswith(KNOWN_TTS_PREFIXES):
        return True
    if has_tts_path_marker(source_path) or has_tts_path_marker(audio_path):
        return True
    return semantic_bucket == "zephyr_positive" and "frozen_verified_audio" in audio_path


def has_tts_path_marker(value: str) -> bool:
    if not value:
        return False
    normalized = value.replace("\\", "/").lower()
    if "/tts/" in normalized:
        return True
    if normalized.endswith("/tts"):
        return True
    tokens = [token for token in re.split(r"[^a-z0-9]+", normalized) if token]
    return "tts" in tokens or "edge_tts" in tokens or "parler_tts" in tokens


def summarize_key(row: dict) -> tuple[str, int, str, str, str]:
    return (
        str(row.get("split") or ""),
        int(row.get("label") or 0),
        str(row.get("source_bucket") or ""),
        str(row.get("semantic_bucket") or ""),
        str(row.get("tts_family") or row.get("engine") or ""),
    )


def is_allowed_tts_row(
    row: dict,
    allowed_splits: set[str],
    allowed_labels: set[int],
    allowed_semantic_buckets: set[str],
) -> bool:
    split = str(row.get("split") or "")
    label = int(row.get("label") or 0)
    semantic_bucket = str(row.get("semantic_bucket") or "")
    return split in allowed_splits and label in allowed_labels and semantic_bucket in allowed_semantic_buckets


def verify_manifest(
    manifest: Path,
    allowed_splits: set[str],
    allowed_labels: set[int],
    allowed_semantic_buckets: set[str],
) -> dict:
    summary_counts: Counter[tuple[str, int, str, str, str]] = Counter()
    violation_counts: Counter[tuple[str, int, str, str, str]] = Counter()
    violations: list[dict] = []
    total_rows = 0
    tts_rows = 0
    allowed_tts_rows = 0

    with manifest.open("r", encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, 1):
            row = json.loads(line)
            total_rows += 1
            if not looks_like_tts(row):
                continue
            tts_rows += 1
            key = summarize_key(row)
            summary_counts[key] += 1
            if is_allowed_tts_row(row, allowed_splits, allowed_labels, allowed_semantic_buckets):
                allowed_tts_rows += 1
                continue
            violation_counts[key] += 1
            if len(violations) < 50:
                violations.append(
                    {
                        "line": line_no,
                        "split": row.get("split"),
                        "label": int(row.get("label") or 0),
                        "source_bucket": row.get("source_bucket"),
                        "semantic_bucket": row.get("semantic_bucket"),
                        "tts_family": row.get("tts_family") or row.get("engine") or "",
                        "audio_path": row.get("audio_path"),
                    }
                )

    return {
        "manifest": str(manifest),
        "total_rows": total_rows,
        "tts_rows": tts_rows,
        "allowed_tts_rows": allowed_tts_rows,
        "violation_rows": tts_rows - allowed_tts_rows,
        "tts_summary": [
            {
                "split": split,
                "label": label,
                "source_bucket": source_bucket,
                "semantic_bucket": semantic_bucket,
                "tts_family": family,
                "rows": rows,
            }
            for (split, label, source_bucket, semantic_bucket, family), rows in sorted(summary_counts.items())
        ],
        "violation_summary": [
            {
                "split": split,
                "label": label,
                "source_bucket": source_bucket,
                "semantic_bucket": semantic_bucket,
                "tts_family": family,
                "rows": rows,
            }
            for (split, label, source_bucket, semantic_bucket, family), rows in sorted(violation_counts.items())
        ],
        "violation_examples": violations,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, action="append", required=True)
    parser.add_argument("--summary-out", type=Path, default=None)
    parser.add_argument("--allowed-splits", default="train")
    parser.add_argument("--allowed-labels", default="1")
    parser.add_argument("--allowed-semantic-buckets", default="zephyr_positive")
    parser.add_argument("--no-fail-on-violation", action="store_true")
    args = parser.parse_args()

    allowed_splits = parse_csv(args.allowed_splits)
    allowed_labels = {int(value) for value in parse_csv(args.allowed_labels)}
    allowed_semantic_buckets = parse_csv(args.allowed_semantic_buckets)

    per_manifest = [
        verify_manifest(
            manifest=manifest,
            allowed_splits=allowed_splits,
            allowed_labels=allowed_labels,
            allowed_semantic_buckets=allowed_semantic_buckets,
        )
        for manifest in args.manifest
    ]
    violation_rows = sum(item["violation_rows"] for item in per_manifest)
    summary = {
        "allowed_splits": sorted(allowed_splits),
        "allowed_labels": sorted(allowed_labels),
        "allowed_semantic_buckets": sorted(allowed_semantic_buckets),
        "manifest_count": len(per_manifest),
        "violation_rows": violation_rows,
        "manifests": per_manifest,
    }

    if args.summary_out is not None:
        args.summary_out.parent.mkdir(parents=True, exist_ok=True)
        args.summary_out.write_text(json.dumps(summary, indent=2), encoding="utf-8")

    print(json.dumps(summary, indent=2))
    if violation_rows and not args.no_fail_on_violation:
        sys.exit(1)


if __name__ == "__main__":
    main()
