#!/usr/bin/env python
"""ASR-verify canonical Zephyr positives and near-negatives with locked app models.

This uses the Rust parakeet-rs batch verifier in tauri-app-next/parakeet-rs-bench
and the finalized ASR registry weights. It does not use the retired Python
Parakeet path under app/heardright/models.
"""

from __future__ import annotations

import argparse
import json
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


def contains_zephyr_variant(text: str) -> bool:
    return any(is_zephyr_like_word(word) for word in normalize_text(text).split())


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


def resolve_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw)
    if path.is_absolute():
        return path
    return repo_root / path


def row_in_scope(row: dict, mode: str) -> bool:
    family = str(row.get("source_family") or "")
    label = int(row.get("label") or 0)
    if mode == "required":
        return label == 1 or family == "near_negative"
    if mode == "positive":
        return label == 1
    if mode == "near-negative":
        return family == "near_negative"
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
    return ("flag", "negative_contains_zephyr") if has_zephyr else ("pass", "negative_no_zephyr")


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


def batch_rows(rows: list[dict], size: int) -> Iterable[list[dict]]:
    for i in range(0, len(rows), size):
        yield rows[i : i + size]


def run_batch(args: argparse.Namespace, rows: list[dict], batch_id: int, tmp_root: Path) -> dict:
    batch_dir = tmp_root / f"batch_{batch_id:06d}"
    if batch_dir.exists():
        shutil.rmtree(batch_dir)
    batch_dir.mkdir(parents=True, exist_ok=True)
    manifest = []
    for idx, row in enumerate(rows):
        clip = f"{batch_id:06d}_{idx:04d}_{row.get('sample_id') or idx}.wav"
        wav_path = batch_dir / clip
        write_wav_from_packed(args.repo_root, row, wav_path)
        manifest.append(
            {
                "clip": clip,
                "path": str(wav_path),
                "duration_s": float(row.get("duration_s") or 0.0),
            }
        )

    manifest_path = batch_dir / "manifest.json"
    result_path = batch_dir / "result.json"
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    env = dict(**{k: v for k, v in __import__("os").environ.items()})
    if args.ort_dylib_path:
        env["ORT_DYLIB_PATH"] = str(args.ort_dylib_path)
    cmd = [
        str(args.asr_bin),
        str(args.model_dir),
        str(manifest_path),
        str(result_path),
        args.ep,
    ]
    started = time.time()
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env)
    elapsed = time.time() - started
    if proc.returncode != 0:
        raise RuntimeError(f"hr_manifest_asr failed rc={proc.returncode}: {(proc.stderr or proc.stdout)[-2000:]}")
    report = json.loads(result_path.read_text(encoding="utf-8"))
    report["wall_s"] = elapsed
    if not args.keep_tmp:
        shutil.rmtree(batch_dir, ignore_errors=True)
    return report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\manifests\zephyr_canonical_training_split_20260609.jsonl"),
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(r"D:\Claude\heardright\train\reports\asr_verify_locked_parakeet_20260609"),
    )
    parser.add_argument(
        "--asr-bin",
        type=Path,
        default=Path(r"D:\Claude\heardright\tauri-app-next\parakeet-rs-bench\target\release\hr_manifest_asr.exe"),
    )
    parser.add_argument(
        "--model-dir",
        type=Path,
        default=Path(r"D:\Claude\heardright\model_registry\final\asr\rnnt_b64_sym_english_primary"),
    )
    parser.add_argument(
        "--ort-dylib-path",
        type=Path,
        default=Path(r"D:\Claude\heardright\tauri-app-next\src-tauri\resources\runtime\onnxruntime.dll"),
    )
    parser.add_argument("--mode", choices=["required", "positive", "near-negative"], default="required")
    parser.add_argument("--ep", choices=["dml", "cpu"], default="dml")
    parser.add_argument("--batch-size", type=int, default=250)
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--shard-index", type=int, default=0)
    parser.add_argument("--shard-count", type=int, default=1)
    parser.add_argument("--progress-every", type=int, default=250)
    parser.add_argument("--keep-tmp", action="store_true")
    args = parser.parse_args()

    if args.shard_count < 1 or args.shard_index < 0 or args.shard_index >= args.shard_count:
        raise ValueError("invalid shard index/count")
    if not args.asr_bin.exists():
        raise FileNotFoundError(f"missing ASR binary: {args.asr_bin}")
    if not args.model_dir.exists():
        raise FileNotFoundError(f"missing model dir: {args.model_dir}")

    output_dir = args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    tmp_root = output_dir / "_tmp" / f"{args.mode}_shard{args.shard_index:02d}-of-{args.shard_count:02d}"
    tmp_root.mkdir(parents=True, exist_ok=True)
    suffix = f"{args.mode}_shard{args.shard_index:02d}-of-{args.shard_count:02d}"
    pass_path = output_dir / f"{suffix}_pass.jsonl"
    flag_path = output_dir / f"{suffix}_flagged.jsonl"
    progress_path = output_dir / f"{suffix}_progress.json"
    summary_path = output_dir / f"{suffix}_summary.json"

    selected = []
    selected_before_shard = 0
    for line_no, row in enumerate(iter_jsonl(args.manifest), 1):
        if not row_in_scope(row, args.mode):
            continue
        if selected_before_shard % args.shard_count == args.shard_index:
            row["_line_no"] = line_no
            selected.append(row)
            if args.limit_rows and len(selected) >= args.limit_rows:
                break
        selected_before_shard += 1

    counters: Counter = Counter()
    examples_flagged: list[dict] = []
    started = time.time()
    with pass_path.open("w", encoding="utf-8", newline="\n") as pass_out, flag_path.open(
        "w", encoding="utf-8", newline="\n"
    ) as flag_out:
        for batch_id, rows in enumerate(batch_rows(selected, args.batch_size)):
            report = run_batch(args, rows, batch_id, tmp_root)
            asr_rows = report.get("rows", [])
            if len(asr_rows) != len(rows):
                raise RuntimeError(f"ASR row count mismatch: expected {len(rows)}, got {len(asr_rows)}")
            counters["model_load_ms_total"] += int(report.get("load_ms") or 0)
            counters["batch_wall_s_total_x1000"] += int(float(report.get("wall_s") or 0.0) * 1000)
            for row, asr_row in zip(rows, asr_rows):
                transcript = str(asr_row.get("text") or "")
                error = str(asr_row.get("error") or "")
                status, reason = classify(row, transcript, error)
                out = {
                    "sample_id": row.get("sample_id"),
                    "uid": row.get("uid"),
                    "split": row.get("split"),
                    "label": row.get("label"),
                    "source_family": row.get("source_family"),
                    "semantic_bucket": row.get("semantic_bucket"),
                    "source_bucket": row.get("source_bucket"),
                    "source_path": row.get("source_path"),
                    "packed_path": row.get("packed_path"),
                    "packed_offset_bytes": row.get("packed_offset_bytes"),
                    "packed_nbytes": row.get("packed_nbytes"),
                    "duration_s": row.get("duration_s"),
                    "transcript": transcript,
                    "normalized_transcript": normalize_text(transcript),
                    "status": status,
                    "reason": reason,
                    "backend": "locked_parakeet_rnnt",
                    "model_dir": str(args.model_dir),
                    "ep": args.ep,
                    "decode_ms": asr_row.get("decode_ms"),
                    "line_no": row.get("_line_no"),
                }
                target = pass_out if status == "pass" else flag_out
                target.write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")
                counters["processed"] += 1
                counters[status] += 1
                counters[f"family:{row.get('source_family')}:{status}"] += 1
                if status == "flag" and len(examples_flagged) < 30:
                    examples_flagged.append(out)
            pass_out.flush()
            flag_out.flush()
            if counters["processed"] % args.progress_every == 0 or counters["processed"] == len(selected):
                elapsed = time.time() - started
                write_json(
                    progress_path,
                    {
                        "elapsed_s": round(elapsed, 3),
                        "processed": counters["processed"],
                        "selected_for_shard": len(selected),
                        "pass": counters["pass"],
                        "flag": counters["flag"],
                        "rows_per_minute": round(counters["processed"] / max(elapsed / 60.0, 1e-9), 3),
                        "mode": args.mode,
                        "shard_index": args.shard_index,
                        "shard_count": args.shard_count,
                        "model_dir": str(args.model_dir),
                        "ep": args.ep,
                    },
                )

    elapsed = time.time() - started
    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(elapsed, 3),
        "mode": args.mode,
        "selected_for_shard": len(selected),
        "shard_index": args.shard_index,
        "shard_count": args.shard_count,
        "model_dir": str(args.model_dir),
        "asr_bin": str(args.asr_bin),
        "ep": args.ep,
        "counters": dict(counters),
        "examples_flagged": examples_flagged,
    }
    write_json(summary_path, summary)
    if tmp_root.exists() and not args.keep_tmp:
        shutil.rmtree(tmp_root, ignore_errors=True)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
