#!/usr/bin/env python
"""Build derived tag and eval-split manifests for Zephyr source truth.

This script is non-destructive. It streams a source-truth JSONL manifest and
produces:

- a tag manifest with normalized metadata needed for training QA
- deterministic source-level eval assignments to avoid source leakage
- a summary JSON with counts by label/family/split

Run this as a background job for full manifests; it can scan multi-GB JSONL.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from collections import Counter
from pathlib import Path


NEAR_NEGATIVE_TERMS = ("near", "phonetic", "confuser", "heifer", "safer", "suffer", "buffer", "differ")


def stable_float(key: str, seed: str) -> float:
    digest = hashlib.sha256(f"{seed}|{key}".encode("utf-8", errors="replace")).digest()
    return int.from_bytes(digest[:8], "big") / float(2**64)


def infer_source_family(row: dict) -> str:
    semantic = str(row.get("semantic_bucket", "")).lower()
    partition = str(row.get("partition_hint", "")).lower()
    source_bucket = str(row.get("source_bucket", "")).lower()
    source_path = str(row.get("source_path", "")).lower().replace("/", "\\")

    if semantic == "zephyr_positive" or int(row.get("label") or 0) == 1:
        return "zephyr_positive"
    joined = " ".join([semantic, partition, source_bucket, source_path])
    if any(term in joined for term in NEAR_NEGATIVE_TERMS):
        return "near_negative"
    if "musan" in joined or "noise" in joined or semantic in {"silence_ambient"}:
        return "noise_or_musan_negative"
    if "music" in joined:
        return "music_negative"
    if "movie" in joined or "tv" in joined:
        return "tv_movie_negative"
    if "audiobook" in joined or "librispeech" in joined or "speech" in joined:
        return "speech_negative"
    if "mic_capture" in joined:
        return "mic_capture_negative"
    return "general_negative"


def infer_license_bucket(row: dict) -> str:
    source_path = str(row.get("source_path", "")).lower().replace("/", "\\")
    source_bucket = str(row.get("source_bucket", "")).lower()
    joined = f"{source_path} {source_bucket}"
    if "youtube" in joined:
        return "youtube_research_review"
    if "musan" in joined:
        return "cc_by_musan"
    if "common_voice" in joined or "\\cv" in joined:
        return "cc0_common_voice"
    if "librispeech" in joined:
        return "cc_by_librispeech"
    if "librivox" in joined or "audiobook" in joined:
        return "public_domain_or_review"
    if "legacy_archive" in joined:
        return "legacy_archive_review"
    return "unknown_review"


def eval_probability(row: dict, source_family: str) -> float:
    label = int(row.get("label") or 0)
    if label == 1:
        return 0.10
    if source_family == "near_negative":
        return 0.10
    if source_family in {"noise_or_musan_negative", "music_negative", "speech_negative", "tv_movie_negative"}:
        return 0.01
    return 0.005


def asr_required(row: dict, source_family: str) -> bool:
    label = int(row.get("label") or 0)
    return label == 1 or source_family == "near_negative"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-jsonl", type=Path, required=True)
    parser.add_argument("--tag-jsonl", type=Path, required=True)
    parser.add_argument("--eval-jsonl", type=Path, required=True)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--progress-json", type=Path, required=True)
    parser.add_argument("--seed", default="zephyr_source_truth_20260609")
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--progress-every", type=int, default=100_000)
    args = parser.parse_args()

    started = time.time()
    args.tag_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.eval_jsonl.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.progress_json.parent.mkdir(parents=True, exist_ok=True)

    counters: Counter = Counter()
    split_counts: Counter = Counter()
    family_counts: Counter = Counter()
    label_counts: Counter = Counter()
    asr_counts: Counter = Counter()

    with args.input_jsonl.open("r", encoding="utf-8", errors="replace") as src, args.tag_jsonl.open(
        "w", encoding="utf-8", newline="\n"
    ) as tags, args.eval_jsonl.open("w", encoding="utf-8", newline="\n") as evals:
        for line_no, line in enumerate(src, 1):
            if args.limit_rows and counters["rows_seen"] >= args.limit_rows:
                break
            counters["rows_seen"] += 1
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                counters["bad_json"] += 1
                continue
            if not row.get("ok"):
                counters["skipped_not_ok"] += 1
                continue

            source_path = str(row.get("source_path") or "")
            source_key = source_path or str(row.get("id") or "")
            family = infer_source_family(row)
            license_bucket = infer_license_bucket(row)
            eval_score = stable_float(source_key, args.seed)
            split = "eval" if eval_score < eval_probability(row, family) else "train"
            asr_verify_required = asr_required(row, family)
            packed_path = row.get("conditioned_shard_path") or row.get("conditioned_path") or ""

            out = {
                "sample_id": row.get("id"),
                "label": int(row.get("label") or 0),
                "semantic_bucket": row.get("semantic_bucket", ""),
                "partition_hint": row.get("partition_hint", ""),
                "source_bucket": row.get("source_bucket", ""),
                "source_path": source_path,
                "packed_path": packed_path,
                "packed_offset_bytes": row.get("conditioned_shard_offset_bytes"),
                "packed_nbytes": row.get("conditioned_shard_nbytes") or row.get("conditioned_bytes"),
                "duration_s": row.get("conditioned_duration_s") or row.get("source_duration_s"),
                "source_family": family,
                "license_bucket": license_bucket,
                "split": split,
                "eval_eligible": split == "eval",
                "asr_verify_required": asr_verify_required,
                "qa_status": "pending_asr" if asr_verify_required else "pending_sample_qa",
            }
            tags.write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")
            if split == "eval":
                evals.write(json.dumps(out, ensure_ascii=False, sort_keys=True) + "\n")

            counters["tag_rows"] += 1
            split_counts[split] += 1
            family_counts[family] += 1
            label_counts[str(out["label"])] += 1
            asr_counts["required" if asr_verify_required else "sample_only"] += 1

            if counters["rows_seen"] % args.progress_every == 0:
                args.progress_json.write_text(
                    json.dumps(
                        {
                            "elapsed_s": round(time.time() - started, 3),
                            "line_no": line_no,
                            "rows_seen": counters["rows_seen"],
                            "tag_rows": counters["tag_rows"],
                            "split_counts": dict(split_counts),
                        },
                        indent=2,
                        sort_keys=True,
                    ),
                    encoding="utf-8",
                )

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "input_jsonl": str(args.input_jsonl),
        "tag_jsonl": str(args.tag_jsonl),
        "eval_jsonl": str(args.eval_jsonl),
        "seed": args.seed,
        "counters": dict(counters),
        "split_counts": dict(split_counts),
        "family_counts": dict(family_counts),
        "label_counts": dict(label_counts),
        "asr_counts": dict(asr_counts),
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    args.progress_json.write_text(json.dumps({**summary, "done": True}, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
