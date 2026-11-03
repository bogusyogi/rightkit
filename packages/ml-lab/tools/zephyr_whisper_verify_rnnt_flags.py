#!/usr/bin/env python
"""Whisper-adjudicate rows flagged by locked RNNT ASR verification."""

from __future__ import annotations

import argparse
import json
import re
import shutil
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


def resolve_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw)
    if path.is_absolute():
        return path
    return repo_root / path


def write_wav_from_packed(repo_root: Path, row: dict, wav_path: Path) -> None:
    packed_path = resolve_path(repo_root, str(row.get("packed_path") or ""))
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


def classify(row: dict, whisper_text: str, error: str = "") -> tuple[str, str]:
    label = int(row.get("label") or 0)
    family = str(row.get("source_family") or "")
    has_zephyr = contains_zephyr_variant(whisper_text)
    if error:
        return "review", f"whisper_error:{error[:160]}"
    if label == 1:
        return ("rescue", "whisper_hears_zephyr_positive") if has_zephyr else ("drop_candidate", "whisper_missing_zephyr_positive")
    if family == "near_negative":
        return ("drop_candidate", "whisper_hears_zephyr_near_negative") if has_zephyr else ("clear", "whisper_no_zephyr_near_negative")
    return ("review", "unexpected_flag_family")


def write_json(path: Path, data: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(path)


def load_flagged_rows(input_dir: Path, limit: int = 0) -> list[dict]:
    rows: list[dict] = []
    seen: set[str] = set()
    for path in sorted(input_dir.glob("required_shard??-of-02_flagged.jsonl")):
        for row in iter_jsonl(path):
            key = str(row.get("uid") or row.get("sample_id") or json.dumps(row, sort_keys=True))
            if key in seen:
                continue
            seen.add(key)
            rows.append(row)
            if limit and len(rows) >= limit:
                return rows
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument(
        "--rnnt-report-dir",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\reports\asr_verify_locked_parakeet_20260609"),
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\reports\asr_verify_whisper_flags_20260609"),
    )
    parser.add_argument("--model", default="large-v3-turbo")
    parser.add_argument("--device", default="cuda")
    parser.add_argument("--compute-type", default="float16")
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--shard-index", type=int, default=0)
    parser.add_argument("--shard-count", type=int, default=1)
    parser.add_argument("--progress-every", type=int, default=50)
    parser.add_argument("--keep-tmp", action="store_true")
    args = parser.parse_args()

    if args.shard_count < 1 or args.shard_index < 0 or args.shard_index >= args.shard_count:
        raise ValueError("invalid shard index/count")

    all_rows = load_flagged_rows(args.rnnt_report_dir, args.limit_rows)
    rows = [row for i, row in enumerate(all_rows) if i % args.shard_count == args.shard_index]

    args.output_dir.mkdir(parents=True, exist_ok=True)
    suffix = f"whisper_flags_shard{args.shard_index:02d}-of-{args.shard_count:02d}"
    out_paths = {
        "rescue": args.output_dir / f"{suffix}_rescue.jsonl",
        "drop_candidate": args.output_dir / f"{suffix}_drop_candidate.jsonl",
        "clear": args.output_dir / f"{suffix}_clear.jsonl",
        "review": args.output_dir / f"{suffix}_review.jsonl",
    }
    progress_path = args.output_dir / f"{suffix}_progress.json"
    summary_path = args.output_dir / f"{suffix}_summary.json"
    tmp_root = args.output_dir / "_tmp" / suffix
    tmp_root.mkdir(parents=True, exist_ok=True)

    from faster_whisper import WhisperModel

    load_started = time.time()
    model = WhisperModel(args.model, device=args.device, compute_type=args.compute_type)
    load_s = time.time() - load_started

    counters: Counter = Counter()
    examples: dict[str, list[dict]] = {key: [] for key in out_paths}
    started = time.time()
    handles = {key: path.open("w", encoding="utf-8", newline="\n") for key, path in out_paths.items()}
    try:
        for idx, row in enumerate(rows):
            wav_path = tmp_root / f"{idx:06d}_{row.get('sample_id') or idx}.wav"
            transcript = ""
            error = ""
            decode_s = 0.0
            try:
                write_wav_from_packed(args.repo_root, row, wav_path)
                t0 = time.time()
                segments, _info = model.transcribe(
                    str(wav_path),
                    language="en",
                    beam_size=1,
                    vad_filter=False,
                    condition_on_previous_text=False,
                    word_timestamps=False,
                )
                transcript = " ".join(seg.text.strip() for seg in segments).strip()
                decode_s = time.time() - t0
            except Exception as exc:
                error = f"{type(exc).__name__}:{exc}"
            finally:
                if not args.keep_tmp:
                    wav_path.unlink(missing_ok=True)

            status, reason = classify(row, transcript, error)
            out = {
                **row,
                "whisper_backend": f"faster-whisper:{args.model}:{args.device}:{args.compute_type}",
                "whisper_transcript": transcript,
                "whisper_normalized_transcript": normalize_text(transcript),
                "whisper_status": status,
                "whisper_reason": reason,
                "whisper_decode_s": round(decode_s, 3),
            }
            handles[status].write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")
            handles[status].flush()
            counters["processed"] += 1
            counters[status] += 1
            counters[f"family:{row.get('source_family')}:{status}"] += 1
            if len(examples[status]) < 10:
                examples[status].append(out)

            if counters["processed"] % args.progress_every == 0 or counters["processed"] == len(rows):
                elapsed = time.time() - started
                write_json(
                    progress_path,
                    {
                        "elapsed_s": round(elapsed, 3),
                        "model_load_s": round(load_s, 3),
                        "processed": counters["processed"],
                        "selected_for_shard": len(rows),
                        "rows_per_minute": round(counters["processed"] / max(elapsed / 60.0, 1e-9), 3),
                        "counters": dict(counters),
                        "model": args.model,
                        "device": args.device,
                        "compute_type": args.compute_type,
                    },
                )
    finally:
        for handle in handles.values():
            handle.close()
        if tmp_root.exists() and not args.keep_tmp:
            shutil.rmtree(tmp_root, ignore_errors=True)

    elapsed = time.time() - started
    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(elapsed, 3),
        "model_load_s": round(load_s, 3),
        "selected_for_shard": len(rows),
        "shard_index": args.shard_index,
        "shard_count": args.shard_count,
        "model": args.model,
        "device": args.device,
        "compute_type": args.compute_type,
        "counters": dict(counters),
        "examples": examples,
    }
    write_json(summary_path, summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
