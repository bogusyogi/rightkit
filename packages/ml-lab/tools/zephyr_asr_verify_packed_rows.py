#!/usr/bin/env python
"""ASR-verify Zephyr source-truth packed rows.

This script is designed to be launched in hidden background shards. It does not
mutate labels or manifests; it only writes pass/flag review artifacts.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from collections import Counter
from pathlib import Path
from typing import Iterable

import numpy as np


ACCEPTED_ZEPHYR_FORMS = {
    "zephyr",
    "zephir",
    "zefer",
    "zeffer",
    "zepher",
    "zephar",
    "zeppe",
    "zepper",
    "zeppa",
}

GENERAL_SAMPLE_LIMITS = {
    "general_negative": 500,
    "music_negative": 500,
    "speech_negative": 500,
    "noise_or_musan_negative": 500,
    "mic_capture_negative": 500,
    "tv_movie_negative": 10_000,
}


def add_repo_imports(repo_root: Path) -> None:
    app_root = repo_root / "app"
    if str(app_root) not in sys.path:
        sys.path.insert(0, str(app_root))


def iter_jsonl(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def normalize_text(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", " ", text.lower()).strip()


def contains_zephyr_variant(text: str) -> bool:
    words = set(normalize_text(text).split())
    return bool(words & ACCEPTED_ZEPHYR_FORMS)


def resolve_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw)
    if path.is_absolute():
        return path
    return repo_root / path


def load_packed_audio(repo_root: Path, row: dict) -> np.ndarray:
    packed_path = resolve_path(repo_root, str(row.get("packed_path") or ""))
    offset = int(row.get("packed_offset_bytes") or 0)
    nbytes = int(row.get("packed_nbytes") or 0)
    if not packed_path.exists():
        raise FileNotFoundError(f"packed path missing: {packed_path}")
    if nbytes <= 0:
        raise ValueError(f"invalid packed_nbytes={nbytes}")
    if nbytes % 2:
        raise ValueError(f"packed_nbytes must be int16 aligned: {nbytes}")
    with packed_path.open("rb") as f:
        f.seek(offset)
        payload = f.read(nbytes)
    if len(payload) != nbytes:
        raise IOError(f"short read from {packed_path}: wanted {nbytes}, got {len(payload)}")
    pcm = np.frombuffer(payload, dtype="<i2")
    return (pcm.astype("float32") / 32768.0).copy()


def row_in_scope(row: dict, mode: str, family_counts: Counter) -> bool:
    family = str(row.get("source_family") or "")
    if mode == "required":
        return bool(row.get("asr_verify_required")) or int(row.get("label") or 0) == 1 or family == "near_negative"
    if mode == "positive":
        return int(row.get("label") or 0) == 1
    if mode == "near-negative":
        return family == "near_negative"
    if mode == "general-sample":
        if bool(row.get("asr_verify_required")):
            return False
        limit = GENERAL_SAMPLE_LIMITS.get(family, 0)
        if limit <= 0 or family_counts[family] >= limit:
            return False
        family_counts[family] += 1
        return True
    raise ValueError(f"unknown mode: {mode}")


def classify(row: dict, transcript: str, error: str = "") -> tuple[str, str]:
    label = int(row.get("label") or 0)
    family = str(row.get("source_family") or "")
    has_zephyr = contains_zephyr_variant(transcript)
    if error:
        return "flag", f"asr_error:{error[:160]}"
    if label == 1:
        return ("pass", "positive_contains_zephyr") if has_zephyr else ("flag", "positive_missing_zephyr")
    if family == "near_negative":
        return ("flag", "near_negative_contains_zephyr") if has_zephyr else ("pass", "near_negative_no_zephyr")
    return ("flag", "general_negative_contains_zephyr") if has_zephyr else ("pass", "general_negative_no_zephyr")


def write_json(path: Path, data: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument("--tag-jsonl", type=Path, default=Path(r"D:\Claude\heardright\train\manifests\zephyr_source_truth_tags_20260609.jsonl"))
    parser.add_argument("--output-dir", type=Path, default=Path(r"D:\Claude\heardright\train\reports\asr_verify_20260609"))
    parser.add_argument("--backend", default="parakeet", help="ASR backend family or canonical backend name.")
    parser.add_argument("--mode", choices=["required", "positive", "near-negative", "general-sample"], default="required")
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--shard-index", type=int, default=0)
    parser.add_argument("--shard-count", type=int, default=1)
    parser.add_argument("--progress-every", type=int, default=50)
    parser.add_argument("--dry-run", action="store_true", help="Select rows and write summary without loading ASR.")
    args = parser.parse_args()

    if args.shard_count < 1 or args.shard_index < 0 or args.shard_index >= args.shard_count:
        raise ValueError("invalid shard index/count")

    repo_root = args.repo_root.resolve()
    output_dir = args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    suffix = f"{args.mode}_shard{args.shard_index:02d}-of-{args.shard_count:02d}"
    pass_path = output_dir / f"{suffix}_pass.jsonl"
    flag_path = output_dir / f"{suffix}_flagged.jsonl"
    progress_path = output_dir / f"{suffix}_progress.json"
    summary_path = output_dir / f"{suffix}_summary.json"

    started = time.time()
    counters: Counter = Counter()
    family_select_counts: Counter = Counter()
    examples_flagged: list[dict] = []

    backend = None
    if not args.dry_run:
        add_repo_imports(repo_root)
        from heardright.asr_runtime import HotwordSet, route
        from heardright.asr_runtime.backends.base import TranscribeContext

        backend = route(args.backend)
        try:
            backend.warmup()
        except Exception:
            pass
        hotwords = HotwordSet(words=("Zephyr", "zephyr"))
        ctx = TranscribeContext(hotwords=hotwords, audio_conditioned=True, deterministic=True)
    else:
        hotwords = None
        ctx = None

    with pass_path.open("w", encoding="utf-8", newline="\n") as pass_out, flag_path.open(
        "w", encoding="utf-8", newline="\n"
    ) as flag_out:
        for line_no, row in enumerate(iter_jsonl(args.tag_jsonl), 1):
            counters["rows_seen"] += 1
            if not row_in_scope(row, args.mode, family_select_counts):
                continue
            if counters["selected_before_shard"] % args.shard_count != args.shard_index:
                counters["selected_before_shard"] += 1
                continue
            counters["selected_before_shard"] += 1
            if args.limit_rows and counters["processed"] >= args.limit_rows:
                break

            transcript = ""
            error = ""
            decode_s = 0.0
            backend_name = args.backend
            if not args.dry_run:
                try:
                    audio = load_packed_audio(repo_root, row)
                    t0 = time.time()
                    result = backend.transcribe(audio, ctx=ctx)
                    decode_s = round(time.time() - t0, 3)
                    transcript = str(result.text or "")
                    backend_name = str(getattr(result, "backend", args.backend))
                except Exception as exc:
                    error = f"{type(exc).__name__}:{exc}"

            if args.dry_run:
                status, reason = "selected", "dry_run_no_asr"
            else:
                status, reason = classify(row, transcript, error)
            out = {
                "sample_id": row.get("sample_id"),
                "label": row.get("label"),
                "source_family": row.get("source_family"),
                "semantic_bucket": row.get("semantic_bucket"),
                "source_path": row.get("source_path"),
                "packed_path": row.get("packed_path"),
                "packed_offset_bytes": row.get("packed_offset_bytes"),
                "packed_nbytes": row.get("packed_nbytes"),
                "transcript": transcript,
                "normalized_transcript": normalize_text(transcript),
                "status": status,
                "reason": reason,
                "backend": backend_name,
                "decode_s": decode_s,
                "line_no": line_no,
            }
            target = pass_out if status in {"pass", "selected"} else flag_out
            target.write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")
            target.flush()
            counters["processed"] += 1
            counters[status] += 1
            counters[f"family:{row.get('source_family')}:{status}"] += 1
            if status == "flag" and len(examples_flagged) < 20:
                examples_flagged.append(out)

            if counters["processed"] % args.progress_every == 0:
                elapsed = time.time() - started
                rpm = counters["processed"] / max(elapsed / 60.0, 1e-9)
                write_json(
                    progress_path,
                    {
                        "elapsed_s": round(elapsed, 3),
                        "rows_seen": counters["rows_seen"],
                        "processed": counters["processed"],
                        "pass": counters["pass"],
                        "flag": counters["flag"],
                        "rows_per_minute": round(rpm, 3),
                        "mode": args.mode,
                        "shard_index": args.shard_index,
                        "shard_count": args.shard_count,
                    },
                )

    elapsed = time.time() - started
    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(elapsed, 3),
        "mode": args.mode,
        "backend": args.backend,
        "dry_run": args.dry_run,
        "shard_index": args.shard_index,
        "shard_count": args.shard_count,
        "counters": dict(counters),
        "pass_jsonl": str(pass_path),
        "flagged_jsonl": str(flag_path),
        "examples_flagged": examples_flagged,
    }
    write_json(summary_path, summary)
    write_json(progress_path, {**summary, "done": True})
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
