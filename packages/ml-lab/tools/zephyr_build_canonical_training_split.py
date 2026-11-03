#!/usr/bin/env python
"""Build the canonical Zephyr wake-word training split.

This streams the cleaned packed source-truth tag manifest plus optional pico
conditioned manifests. It keeps all positives, samples negatives to the planned
training ratio, splits selected rows with source-group leakage guards, and
writes transfer-friendly manifests without copying audio.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterable


SEED = "zephyr_canonical_training_split_20260609_v1"
TRAIN_WEIGHT = 75
EVAL_WEIGHT = 15
TARGET_TRAIN_NEG_PER_POS = 8
TARGET_EVAL_NEG_PER_POS = 8
MUSIC_NEG_CAP_FRACTION = 0.15
NEAR_NEG_CAP_FRACTION = 0.15

PICO_MANIFESTS = (
    "train_manifest_small_flac.jsonl",
)


def iter_jsonl(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def stable_float(key: str, seed: str = SEED) -> float:
    digest = hashlib.sha256(f"{seed}|{key}".encode("utf-8", errors="replace")).digest()
    return int.from_bytes(digest[:8], "big") / float(2**64)


def stable_hex(key: str, seed: str = SEED) -> str:
    return hashlib.sha256(f"{seed}|{key}".encode("utf-8", errors="replace")).hexdigest()


def rel_path(path: str | Path, root: Path) -> str:
    p = Path(path)
    try:
        return str(p.resolve().relative_to(root.resolve()))
    except Exception:
        return str(path)


def normalize_pico_local_path(wav: str, corpus: str, pico_root: Path) -> str:
    text = str(wav)
    if text.startswith("/workspace/data/conditioned/"):
        suffix = text.removeprefix("/workspace/data/conditioned/").replace("/", "\\")
        return str(pico_root / suffix)
    return text


def normalize_pico_vast_path(local_path: str, corpus: str, pico_root: Path) -> str:
    root_text = str(pico_root).replace("\\", "/").rstrip("/").lower()
    path_text = str(local_path).replace("\\", "/")
    lower_path = path_text.lower()
    marker = root_text + "/"
    if lower_path.startswith(marker):
        return "/workspace/data/conditioned/" + path_text[len(marker) :]
    marker = "train/pico/conditioned/"
    idx = lower_path.find(marker)
    if idx >= 0:
        return "/workspace/data/conditioned/" + path_text[idx + len(marker) :]
    filename = Path(local_path).name
    return f"/workspace/data/conditioned/{corpus}/{filename}"


def canonical_group_key(row: dict) -> str:
    source_path = str(row.get("source_path") or "")
    packed_path = str(row.get("packed_path") or "")
    sample_id = str(row.get("sample_id") or row.get("id") or "")
    if source_path:
        return "src:" + source_path.lower().replace("/", "\\")
    if packed_path:
        return "packed:" + packed_path.lower().replace("/", "\\")
    return "id:" + sample_id


def fixed_partition(row: dict) -> str:
    partition = str(row.get("partition_hint") or "").lower()
    split = str(row.get("split") or "").lower()
    joined = f"{partition} {split}"
    if "old_canon" in joined or "audit" in joined:
        return "old_canon_audit_eval"
    if "eval_stress" in joined or "stress" in joined:
        return "eval_stress"
    return ""


def pico_group_key(row: dict) -> str:
    corpus = str(row.get("corpus") or "pico")
    sample_id = str(row.get("id") or "")
    stem = sample_id
    parts = sample_id.split("_")
    if corpus == "librispeech" and len(parts) >= 3:
        stem = "_".join(parts[:2])
    elif corpus == "ami" and len(parts) >= 2:
        stem = "_".join(parts[:2])
    return f"pico:{corpus}:{stem}".lower()


def negative_bucket(row: dict) -> str:
    family = str(row.get("source_family") or "").lower()
    semantic = str(row.get("semantic_bucket") or "").lower()
    corpus = str(row.get("corpus") or "").lower()
    if family == "near_negative" or "near" in semantic or "phonetic" in semantic:
        return "near_negative"
    if family == "music_negative" or "music" in semantic:
        return "music_negative"
    if family == "noise_or_musan_negative" or "musan" in semantic or "silence" in semantic:
        return "noise_or_musan_negative"
    if corpus:
        return f"pico_{corpus}_speech_negative"
    if family:
        return family
    return "general_negative"


def duration_s(row: dict) -> float:
    try:
        value = float(row.get("duration_s") or 0.0)
    except Exception:
        value = 0.0
    return max(0.0, value)


def hours(seconds: float) -> float:
    return seconds / 3600.0


def target_negative_bucket_seconds(total_negative_seconds: float, available_seconds: dict[str, float]) -> dict[str, float]:
    selected: dict[str, float] = {}
    near_cap = total_negative_seconds * NEAR_NEG_CAP_FRACTION
    selected["near_negative"] = min(float(available_seconds.get("near_negative", 0.0)), near_cap)

    music_cap = total_negative_seconds * MUSIC_NEG_CAP_FRACTION
    selected["music_negative"] = min(float(available_seconds.get("music_negative", 0.0)), music_cap)

    remaining = max(0.0, total_negative_seconds - sum(selected.values()))
    other_buckets = [
        bucket
        for bucket, seconds in available_seconds.items()
        if seconds > 0 and bucket not in {"near_negative", "music_negative"}
    ]
    total_other_avail = sum(float(available_seconds[bucket]) for bucket in other_buckets)
    for bucket in sorted(other_buckets):
        if total_other_avail <= 0:
            selected[bucket] = 0.0
        else:
            selected[bucket] = min(
                float(available_seconds[bucket]),
                remaining * float(available_seconds[bucket]) / total_other_avail,
            )
    return selected


def assign_splits(rows: list[dict], eval_fraction: float, label: int, seed_suffix: str) -> dict[str, str]:
    groups: dict[str, list[dict]] = defaultdict(list)
    for row in rows:
        groups[str(row["group_key"])].append(row)
    total_seconds = sum(duration_s(row) for row in rows)
    target_eval_seconds = total_seconds * eval_fraction
    eval_seconds = 0.0
    assignments: dict[str, str] = {}
    ordered = sorted(groups.items(), key=lambda item: stable_float(f"{seed_suffix}|{label}|{item[0]}"))
    for group_key, group_rows in ordered:
        group_seconds = sum(duration_s(row) for row in group_rows)
        split = "eval_primary" if eval_seconds < target_eval_seconds else "train"
        if split == "eval_primary":
            eval_seconds += group_seconds
        for row in group_rows:
            assignments[str(row["uid"])] = split
    return assignments


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag-jsonl", type=Path, default=Path("train/manifests/zephyr_source_truth_tags_20260609.jsonl"))
    parser.add_argument("--pico-root", type=Path, default=Path("train/pico/conditioned"))
    parser.add_argument("--output-jsonl", type=Path, default=Path("train/manifests/zephyr_canonical_training_split_20260609.jsonl"))
    parser.add_argument("--vast-jsonl", type=Path, default=Path("train/manifests/zephyr_canonical_training_split_vast_20260609.jsonl"))
    parser.add_argument("--unused-jsonl", type=Path, default=Path("train/manifests/zephyr_canonical_unused_negatives_20260609.jsonl"))
    parser.add_argument("--summary-json", type=Path, default=Path("train/reports/zephyr_canonical_training_split_20260609_summary.json"))
    parser.add_argument("--report-md", type=Path, default=Path("train/reports/zephyr_canonical_training_split_20260609_report.md"))
    parser.add_argument("--progress-json", type=Path, default=Path("train/reports/zephyr_canonical_training_split_20260609_progress.json"))
    parser.add_argument("--limit-canonical", type=int, default=0)
    parser.add_argument("--limit-pico", type=int, default=0)
    parser.add_argument("--check-path-sample", type=int, default=500)
    parser.add_argument("--unused-sample-limit", type=int, default=5000)
    return parser.parse_args()


def iter_normalized_rows(args: argparse.Namespace, started: float, stage: str) -> Iterable[dict]:
    for idx, row in enumerate(iter_jsonl(args.tag_jsonl), 1):
        if args.limit_canonical and idx > args.limit_canonical:
            break
        label = int(row.get("label") or 0)
        uid = "canonical:" + str(row.get("sample_id") or stable_hex(json.dumps(row, sort_keys=True)))
        out = {
            "schema_version": 1,
            "uid": uid,
            "sample_id": row.get("sample_id"),
            "label": label,
            "source_family": row.get("source_family", ""),
            "semantic_bucket": row.get("semantic_bucket", ""),
            "source_bucket": row.get("source_bucket", ""),
            "source_path": row.get("source_path", ""),
            "audio_storage": "packed_pcm16",
            "audio_path": row.get("packed_path", ""),
            "packed_path": row.get("packed_path", ""),
            "packed_offset_bytes": row.get("packed_offset_bytes"),
            "packed_nbytes": row.get("packed_nbytes"),
            "duration_s": row.get("duration_s"),
            "group_key": canonical_group_key(row),
            "original_partition_hint": row.get("partition_hint", ""),
            "fixed_split": fixed_partition(row),
            "partition_source": "canonical_packed",
            "transfer_required_path": row.get("packed_path", ""),
        }
        if label == 1:
            out["negative_bucket"] = ""
        else:
            out["negative_bucket"] = negative_bucket(out)
        yield out
        if idx % 250_000 == 0:
            args.progress_json.write_text(
                json.dumps(
                    {
                        "stage": f"{stage}_canonical",
                        "rows": idx,
                        "elapsed_s": round(time.time() - started, 3),
                    },
                    indent=2,
                ),
                encoding="utf-8",
            )

    pico_rows = 0
    for manifest_name in PICO_MANIFESTS:
        manifest_path = args.pico_root / manifest_name
        if not manifest_path.exists():
            continue
        for idx, row in enumerate(iter_jsonl(manifest_path), 1):
            if args.limit_pico and pico_rows >= args.limit_pico:
                break
            corpus = str(row.get("corpus") or manifest_name.split(".")[0])
            local_path = normalize_pico_local_path(str(row.get("wav") or ""), corpus, args.pico_root)
            uid = "pico:" + str(row.get("id") or stable_hex(local_path))
            out = {
                "schema_version": 1,
                "uid": uid,
                "sample_id": row.get("id"),
                "label": 0,
                "source_family": "pico_speech_negative",
                "semantic_bucket": "read_speech",
                "source_bucket": f"pico_{corpus}",
                "source_path": "",
                "audio_storage": "pico_conditioned_file",
                "audio_path": local_path,
                "duration_s": row.get("dur"),
                "text": row.get("text", ""),
                "corpus": corpus,
                "group_key": pico_group_key(row),
                "partition_source": "pico_conditioned",
                "transfer_required_path": local_path,
                "vast_audio_path": normalize_pico_vast_path(local_path, corpus, args.pico_root),
            }
            out["negative_bucket"] = negative_bucket(out)
            yield out
            pico_rows += 1
            if pico_rows % 250_000 == 0:
                args.progress_json.write_text(
                    json.dumps(
                        {
                            "stage": f"{stage}_pico",
                            "pico_rows": pico_rows,
                            "elapsed_s": round(time.time() - started, 3),
                        },
                        indent=2,
                    ),
                    encoding="utf-8",
                )
        if args.limit_pico and pico_rows >= args.limit_pico:
            break


def main() -> int:
    args = parse_args()
    started = time.time()
    for path in (args.output_jsonl, args.vast_jsonl, args.unused_jsonl, args.summary_json, args.report_md, args.progress_json):
        path.parent.mkdir(parents=True, exist_ok=True)

    available_negative_buckets: Counter = Counter()
    source_counts: Counter = Counter()
    pico_manifest_counts: Counter = Counter()
    fixed_split_counts: Counter = Counter()
    positive_count = 0
    negative_count = 0
    candidate_positive_count = 0
    candidate_negative_count = 0
    positive_seconds = 0.0
    negative_seconds = 0.0
    candidate_positive_seconds = 0.0
    candidate_negative_seconds = 0.0
    available_negative_bucket_seconds: dict[str, float] = defaultdict(float)

    for row in iter_normalized_rows(args, started, "count"):
        fixed = str(row.get("fixed_split") or "")
        dur = duration_s(row)
        if int(row["label"]) == 1:
            positive_count += 1
            positive_seconds += dur
            if not fixed:
                candidate_positive_count += 1
                candidate_positive_seconds += dur
        else:
            negative_count += 1
            negative_seconds += dur
            if not fixed:
                candidate_negative_count += 1
                candidate_negative_seconds += dur
                bucket = str(row["negative_bucket"])
                available_negative_buckets[bucket] += 1
                available_negative_bucket_seconds[bucket] += dur
        if fixed:
            fixed_split_counts[fixed] += 1
        source_counts[str(row["partition_source"])] += 1
        if row.get("partition_source") == "pico_conditioned":
            pico_manifest_counts[str(row.get("corpus") or "pico")] += 1

    eval_fraction = EVAL_WEIGHT / float(TRAIN_WEIGHT + EVAL_WEIGHT)
    target_negative_seconds_total = min(
        candidate_negative_seconds,
        candidate_positive_seconds * TARGET_TRAIN_NEG_PER_POS,
    )
    target_negative_buckets = target_negative_bucket_seconds(
        target_negative_seconds_total,
        available_negative_bucket_seconds,
    )

    positive_rows: list[dict] = []
    fixed_rows: list[dict] = []
    selected_negatives: list[dict] = []
    selected_negative_buckets: Counter = Counter()
    selected_negative_bucket_seconds: dict[str, float] = defaultdict(float)
    unused_negative_count = 0
    unused_negative_seconds = 0.0

    with args.unused_jsonl.open("w", encoding="utf-8", newline="\n") as unused:
        unused_sample_written = 0
        for row in iter_normalized_rows(args, started, "select"):
            if row.get("fixed_split"):
                fixed = dict(row)
                fixed["split"] = row["fixed_split"]
                fixed["selected_for_training_corpus"] = False
                fixed["fixed_eval_partition"] = True
                fixed_rows.append(fixed)
                continue
            if int(row["label"]) == 1:
                positive_rows.append(row)
                continue
            bucket = str(row["negative_bucket"])
            available = max(float(available_negative_bucket_seconds.get(bucket, 0.0)), 1e-9)
            target = float(target_negative_buckets.get(bucket, 0.0))
            threshold = min(1.0, target / available)
            selected = stable_float(f"select_negative|{bucket}|{row['uid']}") < threshold
            if selected:
                selected_negatives.append(row)
                selected_negative_buckets[bucket] += 1
                selected_negative_bucket_seconds[bucket] += duration_s(row)
            else:
                unused_negative_count += 1
                unused_negative_seconds += duration_s(row)
                if unused_sample_written < args.unused_sample_limit:
                    unused.write(
                        json.dumps(
                            {
                                "uid": row["uid"],
                                "label": 0,
                                "negative_bucket": row.get("negative_bucket", ""),
                                "source_family": row.get("source_family", ""),
                                "source_bucket": row.get("source_bucket", ""),
                                "partition_source": row.get("partition_source", ""),
                                "reason": "random_negative_downsample_excluded_from_training_split",
                            },
                            ensure_ascii=False,
                            sort_keys=True,
                        )
                        + "\n"
                    )
                    unused_sample_written += 1

    positive_assignments = assign_splits(positive_rows, eval_fraction, 1, "positive")
    negative_assignments = assign_splits(selected_negatives, eval_fraction, 0, "negative")
    final_rows: list[dict] = []
    for row in positive_rows:
        out = dict(row)
        out["split"] = positive_assignments[row["uid"]]
        out["selected_for_training_corpus"] = True
        final_rows.append(out)
    for row in selected_negatives:
        out = dict(row)
        out["split"] = negative_assignments[row["uid"]]
        out["selected_for_training_corpus"] = True
        final_rows.append(out)
    final_rows.extend(fixed_rows)

    by_split_label: Counter = Counter()
    by_split_family: Counter = Counter()
    by_split_bucket: Counter = Counter()
    by_split_label_seconds: dict[tuple[str, str], float] = defaultdict(float)
    by_split_family_seconds: dict[tuple[str, str], float] = defaultdict(float)
    by_split_bucket_seconds: dict[tuple[str, str], float] = defaultdict(float)
    missing_path_samples: list[str] = []
    checked_paths = 0
    train_groups: set[str] = set()
    eval_groups: set[str] = set()

    with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as out, args.vast_jsonl.open(
        "w", encoding="utf-8", newline="\n"
    ) as vast:
        for row in final_rows:
            split = str(row["split"])
            label = int(row["label"])
            group = str(row["group_key"])
            dur = duration_s(row)
            if split == "train":
                train_groups.add(group)
            elif split == "eval_primary":
                eval_groups.add(group)
            by_split_label[(split, str(label))] += 1
            by_split_label_seconds[(split, str(label))] += dur
            family = str(row.get("source_family", ""))
            bucket = str(row.get("source_bucket", ""))
            by_split_family[(split, family)] += 1
            by_split_family_seconds[(split, family)] += dur
            by_split_bucket[(split, bucket)] += 1
            by_split_bucket_seconds[(split, bucket)] += dur
            if checked_paths < args.check_path_sample:
                check_path = str(row.get("audio_path") or row.get("packed_path") or "")
                if check_path and not Path(check_path).exists():
                    missing_path_samples.append(check_path)
                checked_paths += 1

            out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            vast_row = dict(row)
            if vast_row.get("audio_storage") == "packed_pcm16":
                vast_row["vast_audio_path"] = "/workspace/data/" + str(vast_row.get("packed_path", "")).replace("\\", "/")
            vast.write(json.dumps(vast_row, ensure_ascii=False, sort_keys=True) + "\n")
    leakage_groups = sorted(train_groups.intersection(eval_groups))[:20]
    train_pos = by_split_label.get(("train", "1"), 0)
    train_neg = by_split_label.get(("train", "0"), 0)
    eval_pos = by_split_label.get(("eval_primary", "1"), 0)
    eval_neg = by_split_label.get(("eval_primary", "0"), 0)
    train_pos_seconds = by_split_label_seconds.get(("train", "1"), 0.0)
    train_neg_seconds = by_split_label_seconds.get(("train", "0"), 0.0)
    eval_pos_seconds = by_split_label_seconds.get(("eval_primary", "1"), 0.0)
    eval_neg_seconds = by_split_label_seconds.get(("eval_primary", "0"), 0.0)
    selected_negative_seconds = sum(selected_negative_bucket_seconds.values())
    selected_music_seconds = selected_negative_bucket_seconds.get("music_negative", 0.0)
    selected_near_seconds = selected_negative_bucket_seconds.get("near_negative", 0.0)

    def count_hours(counter: Counter, seconds: dict[tuple[str, str], float]) -> dict[str, dict]:
        out: dict[str, dict] = {}
        for key, count in sorted(counter.items()):
            split, name = key
            secs = float(seconds.get((split, name), 0.0))
            out[f"{split}|{name}"] = {
                "rows": count,
                "hours": round(hours(secs), 6),
                "seconds": round(secs, 3),
            }
        return out
    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "elapsed_s": round(time.time() - started, 3),
        "seed": SEED,
        "policy": {
            "train_eval_weight": f"{TRAIN_WEIGHT}/{EVAL_WEIGHT}",
            "eval_fraction_among_selected": eval_fraction,
            "target_train_neg_per_pos": TARGET_TRAIN_NEG_PER_POS,
            "target_eval_neg_per_pos": TARGET_EVAL_NEG_PER_POS,
            "negative_downsampling": "stable_random_within_negative_bucket_no_positive_exclusion",
            "copy_audio": False,
            "pico_strategy": "reference_in_place_and_emit_vast_paths",
        },
        "inputs": {
            "canonical_tag_jsonl": str(args.tag_jsonl),
            "pico_root": str(args.pico_root),
            "pico_manifests": list(PICO_MANIFESTS),
        },
        "outputs": {
            "split_jsonl": str(args.output_jsonl),
            "vast_jsonl": str(args.vast_jsonl),
            "unused_negatives_jsonl": str(args.unused_jsonl),
            "summary_json": str(args.summary_json),
            "report_md": str(args.report_md),
        },
        "inventory": {
            "positive_rows_total": positive_count,
            "negative_rows_total": negative_count,
            "rows_total": positive_count + negative_count,
            "positive_hours_total": round(hours(positive_seconds), 6),
            "negative_hours_total": round(hours(negative_seconds), 6),
            "source_counts": dict(source_counts),
            "available_negative_buckets": dict(sorted(available_negative_buckets.items())),
            "available_negative_bucket_hours": {
                bucket: round(hours(seconds), 6)
                for bucket, seconds in sorted(available_negative_bucket_seconds.items())
            },
            "pico_manifest_counts": dict(sorted(pico_manifest_counts.items())),
            "fixed_split_counts": dict(sorted(fixed_split_counts.items())),
            "candidate_positive_rows": candidate_positive_count,
            "candidate_negative_rows": candidate_negative_count,
            "candidate_positive_hours": round(hours(candidate_positive_seconds), 6),
            "candidate_negative_hours": round(hours(candidate_negative_seconds), 6),
        },
        "selection": {
            "selected_rows": len(final_rows),
            "selected_positive_rows": len(positive_rows),
            "selected_negative_rows": len(selected_negatives),
            "selected_positive_hours": round(hours(candidate_positive_seconds), 6),
            "selected_negative_hours": round(hours(selected_negative_seconds), 6),
            "fixed_eval_rows": len(fixed_rows),
            "unused_negative_rows": unused_negative_count,
            "unused_negative_hours": round(hours(unused_negative_seconds), 6),
            "unused_negative_sample_rows_written": min(unused_negative_count, args.unused_sample_limit),
            "target_negative_bucket_hours": {
                bucket: round(hours(seconds), 6)
                for bucket, seconds in sorted(target_negative_buckets.items())
            },
            "selected_negative_buckets": dict(sorted(selected_negative_buckets.items())),
            "selected_negative_bucket_hours": {
                bucket: round(hours(seconds), 6)
                for bucket, seconds in sorted(selected_negative_bucket_seconds.items())
            },
        },
        "split_counts": {
            "by_split_label": {f"{split}|{label}": count for (split, label), count in sorted(by_split_label.items())},
            "train_pos_neg_ratio": (train_neg / train_pos) if train_pos else None,
            "eval_pos_neg_ratio": (eval_neg / eval_pos) if eval_pos else None,
            "train_pos_neg_hour_ratio": (train_neg_seconds / train_pos_seconds) if train_pos_seconds else None,
            "eval_pos_neg_hour_ratio": (eval_neg_seconds / eval_pos_seconds) if eval_pos_seconds else None,
            "by_split_label_hours": count_hours(by_split_label, by_split_label_seconds),
            "by_split_family": {f"{split}|{family}": count for (split, family), count in sorted(by_split_family.items())},
            "by_split_family_hours": count_hours(by_split_family, by_split_family_seconds),
            "by_split_bucket": {f"{split}|{bucket}": count for (split, bucket), count in sorted(by_split_bucket.items())},
            "by_split_bucket_hours": count_hours(by_split_bucket, by_split_bucket_seconds),
        },
        "validation": {
            "leakage_group_count": len(train_groups.intersection(eval_groups)),
            "leakage_group_examples": leakage_groups,
            "checked_path_sample": checked_paths,
            "missing_path_sample_count": len(missing_path_samples),
            "missing_path_samples": missing_path_samples[:20],
            "train_row_ratio_within_plan": bool(train_pos and 7 <= train_neg / train_pos <= 10),
            "eval_row_ratio_within_plan": bool(eval_pos and 7 <= eval_neg / eval_pos <= 10),
            "train_hour_ratio_within_plan": bool(train_pos_seconds and 7 <= train_neg_seconds / train_pos_seconds <= 10),
            "eval_hour_ratio_within_plan": bool(eval_pos_seconds and 5 <= eval_neg_seconds / eval_pos_seconds <= 10),
            "music_negative_hour_share": (selected_music_seconds / selected_negative_seconds) if selected_negative_seconds else 0.0,
            "near_negative_hour_share": (selected_near_seconds / selected_negative_seconds) if selected_negative_seconds else 0.0,
            "music_negative_hour_cap_met": bool(selected_negative_seconds and selected_music_seconds <= selected_negative_seconds * MUSIC_NEG_CAP_FRACTION + 1.0),
            "near_negative_hour_cap_met": bool(selected_negative_seconds and selected_near_seconds <= selected_negative_seconds * NEAR_NEG_CAP_FRACTION + 1.0),
            "eval_min_positives_met": eval_pos >= 3000,
            "eval_target_positives_met": eval_pos >= 5000,
            "eval_min_negatives_met": eval_neg >= eval_pos * 5,
        },
    }
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")

    report = [
        "# Zephyr Canonical Training Split 2026-06-09",
        "",
        "## Decision",
        "",
        "Pico conditioned audio is not copied into packed shards. It remains under `train/pico/conditioned` and is included by manifest with Vast-ready paths. This avoids duplicating roughly 120 GB while still making the manifest the single source of truth for transfer/training.",
        "",
        "## Inventory",
        "",
        f"- Positives: {len(positive_rows):,}",
        f"- Negatives available: {negative_count:,}",
        f"- Total available rows: {positive_count + negative_count:,}",
        f"- Positive hours available: {hours(positive_seconds):,.2f}",
        f"- Negative hours available: {hours(negative_seconds):,.2f}",
        f"- Random-split candidate positives: {candidate_positive_count:,}",
        f"- Random-split candidate negatives: {candidate_negative_count:,}",
        f"- Canonical packed rows: {source_counts.get('canonical_packed', 0):,}",
        f"- Pico conditioned rows: {source_counts.get('pico_conditioned', 0):,}",
        f"- Fixed eval/audit rows: {len(fixed_rows):,}",
        "",
        "## Too Much Data?",
        "",
        f"Yes. By rows, all available negatives would be about 1:{(negative_count / max(positive_count, 1)):.1f} positives:negatives. By audio exposure it would be about 1:{(negative_seconds / max(positive_seconds, 1e-9)):.1f} hours. This split keeps all positives and randomly down-samples negatives by duration within buckets.",
        "",
        "## Selected Split",
        "",
        f"- Train positives: {train_pos:,}",
        f"- Train negatives: {train_neg:,}",
        f"- Train row ratio: 1:{(train_neg / max(train_pos, 1)):.2f}",
        f"- Train positive hours: {hours(train_pos_seconds):,.2f}",
        f"- Train negative hours: {hours(train_neg_seconds):,.2f}",
        f"- Train hour ratio: 1:{(train_neg_seconds / max(train_pos_seconds, 1e-9)):.2f}",
        f"- Eval positives: {eval_pos:,}",
        f"- Eval negatives: {eval_neg:,}",
        f"- Eval row ratio: 1:{(eval_neg / max(eval_pos, 1)):.2f}",
        f"- Eval positive hours: {hours(eval_pos_seconds):,.2f}",
        f"- Eval negative hours: {hours(eval_neg_seconds):,.2f}",
        f"- Eval hour ratio: 1:{(eval_neg_seconds / max(eval_pos_seconds, 1e-9)):.2f}",
        f"- Unused negatives held out by random downsample: {unused_negative_count:,}",
        f"- Unused negative hours held out: {hours(unused_negative_seconds):,.2f}",
        f"- Unused negative sample rows written: {min(unused_negative_count, args.unused_sample_limit):,}",
        "",
        "## Validation",
        "",
        f"- Group leakage train/eval: {len(train_groups.intersection(eval_groups))}",
        f"- Eval positives >= 3,000: {eval_pos >= 3000}",
        f"- Eval positives target 5,000+: {eval_pos >= 5000}",
        f"- Eval negatives >= 5x positives: {eval_neg >= eval_pos * 5}",
        f"- Train hour ratio 1:7 to 1:10: {bool(train_pos_seconds and 7 <= train_neg_seconds / train_pos_seconds <= 10)}",
        f"- Eval hour ratio at least 1:5 and below 1:10: {bool(eval_pos_seconds and 5 <= eval_neg_seconds / eval_pos_seconds <= 10)}",
        f"- Music negative hour share <= 15%: {bool(selected_negative_seconds and selected_music_seconds <= selected_negative_seconds * MUSIC_NEG_CAP_FRACTION + 1.0)}",
        f"- Near negative hour share <= 15%: {bool(selected_negative_seconds and selected_near_seconds <= selected_negative_seconds * NEAR_NEG_CAP_FRACTION + 1.0)}",
        f"- Missing path samples in first {checked_paths}: {len(missing_path_samples)}",
        "",
        "## Artifacts",
        "",
        f"- Split manifest: `{args.output_jsonl}`",
        f"- Vast transfer manifest: `{args.vast_jsonl}`",
        f"- Unused negatives manifest: `{args.unused_jsonl}`",
        f"- Summary JSON: `{args.summary_json}`",
    ]
    args.report_md.write_text("\n".join(report) + "\n", encoding="utf-8")
    args.progress_json.write_text(json.dumps({**summary, "done": True}, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
