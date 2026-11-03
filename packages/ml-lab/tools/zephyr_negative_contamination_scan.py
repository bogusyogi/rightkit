#!/usr/bin/env python
"""ASR scan negative buckets for Zephyr contamination."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import time
import wave
from collections import Counter
from pathlib import Path
from typing import Iterable


ACCEPTED_ZEPHYR_FORMS = {
    "zephyr",
    "zephir",
    "zefer",
    "zeffer",
    "zepher",
    "zephar",
    "zephyrs",
    "zefyr",
    "zefir",
    "zeffir",
    "zeffyr",
    "zephier",
    "zephyer",
    "zeppe",
    "zepper",
    "zeppa",
    "zeppi",
    "zeppy",
    "zaffer",
    "zapper",
}


def iter_jsonl(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def normalize_text(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", " ", text.lower()).strip()


def levenshtein_ascii(a: str, b: str) -> int:
    costs = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        prev = costs[0]
        costs[0] = i
        for j, cb in enumerate(b, 1):
            cur = costs[j]
            costs[j] = min(costs[j] + 1, costs[j - 1] + 1, prev + (0 if ca == cb else 1))
            prev = cur
    return costs[-1]


def is_zephyr_like_word(word: str) -> bool:
    if word in ACCEPTED_ZEPHYR_FORMS:
        return True
    return word.startswith("z") and 4 <= len(word) <= 8 and levenshtein_ascii(word, "zephyr") <= 2


def contains_zephyr_variant(text: str) -> bool:
    return any(is_zephyr_like_word(word) for word in normalize_text(text).split())


def in_scope(row: dict, scope: str) -> bool:
    if int(row.get("label") or 0) != 0:
        return False
    source_bucket = str(row.get("source_bucket") or "")
    family = str(row.get("source_family") or "")
    negative_bucket = str(row.get("negative_bucket") or "")
    if family == "near_negative":
        return False
    if scope == "speech-no-text":
        return source_bucket in {
            "audiobooks",
            "music_archive_tests",
            "youtube_negative",
            "legacy_archive_audio",
            "legacy_archive_negative",
        }
    if scope == "speech-no-text-large":
        return source_bucket in {"audiobooks", "music_archive_tests"}
    if scope == "small-nonpico":
        return source_bucket in {"youtube_negative", "legacy_archive_audio", "legacy_archive_negative"} or negative_bucket in {
            "mic_capture_negative",
            "general_negative",
            "tv_movie_negative",
        }
    if scope == "audiobooks":
        return source_bucket == "audiobooks"
    if scope == "music-archive-tests":
        return source_bucket == "music_archive_tests"
    if scope == "legacy-music":
        return source_bucket == "legacy_archive_music"
    raise ValueError(f"unknown scope: {scope}")


def resolve_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw)
    if path.is_absolute():
        return path
    return repo_root / path


def write_wav_from_packed(repo_root: Path, row: dict, wav_path: Path) -> None:
    packed_path = resolve_path(repo_root, str(row.get("packed_path") or row.get("audio_path") or ""))
    offset = int(row.get("packed_offset_bytes") or 0)
    nbytes = int(row.get("packed_nbytes") or 0)
    if not packed_path.exists():
        raise FileNotFoundError(f"packed path missing: {packed_path}")
    if nbytes <= 0 or nbytes % 2:
        raise ValueError(f"invalid packed_nbytes={nbytes}")
    with packed_path.open("rb") as f:
        f.seek(offset)
        payload = f.read(nbytes)
    if len(payload) != nbytes:
        raise IOError(f"short read from {packed_path}: wanted {nbytes}, got {len(payload)}")
    with wave.open(str(wav_path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(16_000)
        wf.writeframes(payload)


def write_json(path: Path, data: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(path)


def batch_rows(rows: list[dict], size: int) -> Iterable[tuple[int, list[dict]]]:
    for i in range(0, len(rows), size):
        yield i // size, rows[i : i + size]


def run_batch(args: argparse.Namespace, rows: list[dict], batch_id: int, tmp_root: Path) -> dict:
    batch_dir = tmp_root / f"batch_{batch_id:06d}"
    done_path = batch_dir / "done.json"
    if done_path.exists():
        cached = json.loads(done_path.read_text(encoding="utf-8"))
        if len(cached.get("rows", [])) == len(rows):
            return cached
        shutil.rmtree(batch_dir, ignore_errors=True)
    if batch_dir.exists():
        shutil.rmtree(batch_dir)
    batch_dir.mkdir(parents=True, exist_ok=True)
    manifest = []
    for idx, row in enumerate(rows):
        clip = f"{batch_id:06d}_{idx:04d}_{row.get('sample_id') or idx}.wav"
        wav_path = batch_dir / clip
        write_wav_from_packed(args.repo_root, row, wav_path)
        manifest.append({"clip": clip, "path": str(wav_path), "duration_s": float(row.get("duration_s") or 0.0)})
    manifest_path = batch_dir / "manifest.json"
    result_path = batch_dir / "result.json"
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    env = dict(os.environ)
    if args.ort_dylib_path:
        env["ORT_DYLIB_PATH"] = str(args.ort_dylib_path)
    env["RIGHTKIT_ML_ASR_COMMAND_BIAS_SCORE"] = str(args.bias_score)
    proc = subprocess.run(
        [str(args.asr_bin), str(args.model_dir), str(manifest_path), str(result_path), args.ep],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"hr_manifest_asr failed rc={proc.returncode}: {(proc.stderr or proc.stdout)[-2000:]}")
    report = json.loads(result_path.read_text(encoding="utf-8"))
    done_path.write_text(json.dumps(report), encoding="utf-8")
    if not args.keep_tmp:
        for wav in batch_dir.glob("*.wav"):
            wav.unlink(missing_ok=True)
        manifest_path.unlink(missing_ok=True)
        result_path.unlink(missing_ok=True)
    return report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument("--manifest", type=Path, default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_20260609.jsonl"))
    parser.add_argument(
        "--scope",
        choices=[
            "speech-no-text",
            "speech-no-text-large",
            "small-nonpico",
            "audiobooks",
            "music-archive-tests",
            "legacy-music",
        ],
        default="speech-no-text",
    )
    parser.add_argument("--output-dir", type=Path, default=Path(r"D:\Claude\heardright\train\reports\negative_contamination_scan_20260610"))
    parser.add_argument("--asr-bin", type=Path, default=Path(r"D:\Claude\heardright\tauri-app-next\parakeet-rs-bench\target\release\hr_manifest_asr.exe"))
    parser.add_argument("--model-dir", type=Path, default=Path(r"D:\Claude\heardright\model_registry\final\asr\rnnt_b64_sym_english_primary"))
    parser.add_argument("--ort-dylib-path", type=Path, default=Path(r"D:\Claude\heardright\tauri-app-next\src-tauri\resources\runtime\onnxruntime.dll"))
    parser.add_argument("--ep", choices=["dml", "cpu"], default="dml")
    parser.add_argument("--bias-score", type=float, default=1.0)
    parser.add_argument("--batch-size", type=int, default=500)
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--shard-index", type=int, default=0)
    parser.add_argument("--shard-count", type=int, default=1)
    parser.add_argument("--progress-every", type=int, default=500)
    parser.add_argument("--keep-tmp", action="store_true")
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    suffix = f"{args.scope}_shard{args.shard_index:02d}-of-{args.shard_count:02d}"
    tmp_root = args.output_dir / "_tmp" / suffix
    tmp_root.mkdir(parents=True, exist_ok=True)
    hit_path = args.output_dir / f"{suffix}_hits.jsonl"
    progress_path = args.output_dir / f"{suffix}_progress.json"
    summary_path = args.output_dir / f"{suffix}_summary.json"

    selected = []
    selected_before_shard = 0
    for line_no, row in enumerate(iter_jsonl(args.manifest), 1):
        if not in_scope(row, args.scope):
            continue
        if selected_before_shard % args.shard_count == args.shard_index:
            row["_line_no"] = line_no
            selected.append(row)
            if args.limit_rows and len(selected) >= args.limit_rows:
                break
        selected_before_shard += 1

    counters: Counter = Counter()
    started = time.time()
    with hit_path.open("w", encoding="utf-8", newline="\n") as hit_out:
        for batch_id, rows in batch_rows(selected, args.batch_size):
            report = run_batch(args, rows, batch_id, tmp_root)
            asr_rows = report.get("rows", [])
            if len(asr_rows) != len(rows):
                raise RuntimeError(f"row count mismatch for batch {batch_id}: {len(asr_rows)} != {len(rows)}")
            counters["processed"] += len(rows)
            for row, asr_row in zip(rows, asr_rows):
                text = str(asr_row.get("text") or "")
                error = str(asr_row.get("error") or "")
                if error:
                    counters["errors"] += 1
                    continue
                if contains_zephyr_variant(text):
                    out = {
                        **row,
                        "rnnt_transcript": text,
                        "rnnt_normalized_transcript": normalize_text(text),
                        "rnnt_decode_ms": asr_row.get("decode_ms"),
                        "scan_scope": args.scope,
                        "scan_backend": "locked_rnnt_with_command_bias",
                    }
                    hit_out.write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")
                    counters["hits"] += 1
                    counters[f"bucket:{row.get('negative_bucket')}:{row.get('source_bucket')}:hits"] += 1
            hit_out.flush()
            if counters["processed"] % args.progress_every == 0 or counters["processed"] == len(selected):
                elapsed = time.time() - started
                write_json(
                    progress_path,
                    {
                        "elapsed_s": round(elapsed, 3),
                        "processed": counters["processed"],
                        "selected_for_shard": len(selected),
                        "hits": counters["hits"],
                        "errors": counters["errors"],
                        "rows_per_minute": round(counters["processed"] / max(elapsed / 60.0, 1e-9), 3),
                        "scope": args.scope,
                        "shard_index": args.shard_index,
                        "shard_count": args.shard_count,
                    },
                )

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "scope": args.scope,
        "selected_for_shard": len(selected),
        "shard_index": args.shard_index,
        "shard_count": args.shard_count,
        "counters": dict(counters),
        "hit_path": str(hit_path),
    }
    write_json(summary_path, summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
