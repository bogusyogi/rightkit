#!/usr/bin/env python
"""Audit and optionally merge append-only conditioned manifests.

This is intended for the Zephyr source-truth handoff after long conditioning
runs. It treats row IDs as deterministic and lets later rows supersede earlier
rows, so a failed row followed by an OK retry is not reported as unresolved.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import sqlite3
from collections import Counter
from pathlib import Path


AUDIO_EXTENSIONS = {
    ".aac",
    ".aif",
    ".aiff",
    ".avi",
    ".flac",
    ".m4a",
    ".mkv",
    ".mov",
    ".mp3",
    ".mp4",
    ".ogg",
    ".opus",
    ".wav",
    ".webm",
    ".wma",
}

OBSOLETE_WAKE_TERMS = ("shunya", "cipher", "decipher", "quasar", "equinox")


def iter_jsonl(path: Path):
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line_no, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                yield line_no, json.loads(line)
            except json.JSONDecodeError as exc:
                yield line_no, {"id": "", "ok": False, "error": f"bad_json:{exc}"}


def normalized_path(value: str) -> str:
    return str(Path(value)).replace("/", "\\").lower()


def resolve_output_path(path_value: str, repo_root: Path) -> Path:
    path = Path(path_value)
    if path.is_absolute():
        return path
    return repo_root / path


def scan_raw_roots(raw_roots: list[Path], out_csv: Path | None = None) -> dict:
    by_root = []
    total_files = 0
    total_bytes = 0
    rows = []
    obsolete_wake_rows = []
    for root in raw_roots:
        files = 0
        bytes_ = 0
        if root.exists():
            for dirpath, _, filenames in os.walk(root):
                for name in filenames:
                    if Path(name).suffix.lower() not in AUDIO_EXTENSIONS:
                        continue
                    files += 1
                    path = Path(dirpath) / name
                    try:
                        size = path.stat().st_size
                    except OSError:
                        size = 0
                    bytes_ += size
                    rows.append(
                        {
                            "root": str(root),
                            "bytes": size,
                            "gb": round(size / (1024**3), 6),
                            "path": str(path),
                            "suffix": path.suffix.lower(),
                        }
                    )
                    lowered_path = str(path).lower()
                    matched_terms = [term for term in OBSOLETE_WAKE_TERMS if term in lowered_path]
                    if matched_terms:
                        obsolete_wake_rows.append(
                            {
                                "root": str(root),
                                "bytes": size,
                                "path": str(path),
                                "matched_terms": ",".join(matched_terms),
                            }
                        )
        total_files += files
        total_bytes += bytes_
        by_root.append(
            {
                "root": str(root),
                "audio_video_files": files,
                "bytes": bytes_,
                "gb": round(bytes_ / (1024**3), 4),
            }
        )
    if out_csv is not None:
        out_csv.parent.mkdir(parents=True, exist_ok=True)
        with out_csv.open("w", encoding="utf-8", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=["root", "bytes", "gb", "suffix", "path"])
            writer.writeheader()
            writer.writerows(rows)
    return {
        "roots": by_root,
        "total_audio_video_files": total_files,
        "total_bytes": total_bytes,
        "total_gb": round(total_bytes / (1024**3), 4),
        "residual_csv": str(out_csv) if out_csv is not None else "",
        "obsolete_wake_raw_count": len(obsolete_wake_rows),
        "obsolete_wake_raw_examples": obsolete_wake_rows[:25],
    }


def path_is_under(path: Path, root: Path) -> bool:
    try:
        resolved_path = path.resolve()
        resolved_root = root.resolve()
    except OSError:
        return False
    return resolved_path == resolved_root or resolved_root in resolved_path.parents


def write_bucket_report(path: Path | None, bucket_counts: Counter) -> None:
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["partition_hint", "semantic_bucket", "output_kind", "label", "rows"])
        writer.writeheader()
        for key, rows in sorted(bucket_counts.items()):
            partition_hint, semantic_bucket, output_kind, label = key
            writer.writerow(
                {
                    "partition_hint": partition_hint,
                    "semantic_bucket": semantic_bucket,
                    "output_kind": output_kind,
                    "label": label,
                    "rows": rows,
                }
            )


def validate_expected_buckets(path: Path | None, bucket_counts: Counter) -> list[dict]:
    if path is None:
        return []
    failures = []
    with path.open("r", encoding="utf-8", newline="") as f:
        reader = csv.DictReader(f)
        for row in reader:
            partition_hint = str(row.get("partition_hint", "")).strip()
            semantic_bucket = str(row.get("semantic_bucket", "")).strip()
            output_kind = str(row.get("output_kind", "")).strip()
            label = str(row.get("label", "")).strip()
            try:
                min_rows = int(str(row.get("min_rows", "1")).strip() or "1")
            except ValueError:
                min_rows = 1
            actual = 0
            for (actual_partition, actual_semantic, actual_kind, actual_label), count in bucket_counts.items():
                if partition_hint and partition_hint != actual_partition:
                    continue
                if semantic_bucket and semantic_bucket != actual_semantic:
                    continue
                if output_kind and output_kind != actual_kind:
                    continue
                if label and label != actual_label:
                    continue
                actual += int(count)
            if actual < min_rows:
                failures.append(
                    {
                        "partition_hint": partition_hint,
                        "semantic_bucket": semantic_bucket,
                        "output_kind": output_kind,
                        "label": label,
                        "min_rows": min_rows,
                        "actual_rows": actual,
                        "description": row.get("description", ""),
                    }
                )
    return failures


def count_bucket_rows(
    bucket_counts: Counter,
    partition_hint: str = "",
    semantic_bucket: str = "",
    output_kind: str = "",
    label: str = "",
) -> int:
    total = 0
    for (actual_partition, actual_semantic, actual_kind, actual_label), count in bucket_counts.items():
        if partition_hint and partition_hint != actual_partition:
            continue
        if semantic_bucket and semantic_bucket != actual_semantic:
            continue
        if output_kind and output_kind != actual_kind:
            continue
        if label and label != actual_label:
            continue
        total += int(count)
    return total


def build_bucket_assertions(bucket_counts: Counter, allow_missing_music_split: bool = False) -> dict:
    music_lyrics_rows = count_bucket_rows(bucket_counts, semantic_bucket="music_lyrics", label="0")
    music_non_lyric_rows = count_bucket_rows(bucket_counts, semantic_bucket="music", label="0")
    zephyr_positive_label_1_rows = count_bucket_rows(bucket_counts, semantic_bucket="zephyr_positive", label="1")
    zephyr_positive_bad_label_rows = 0
    for (_, semantic_bucket, _, label), count in bucket_counts.items():
        if semantic_bucket == "zephyr_positive" and label != "1":
            zephyr_positive_bad_label_rows += int(count)

    music_split_failures = []
    if not allow_missing_music_split:
        if music_lyrics_rows <= 0:
            music_split_failures.append(
                {
                    "semantic_bucket": "music_lyrics",
                    "label": "0",
                    "reason": "missing lyric music rows",
                }
            )
        if music_non_lyric_rows <= 0:
            music_split_failures.append(
                {
                    "semantic_bucket": "music",
                    "label": "0",
                    "reason": "missing non-lyric music rows",
                }
            )

    positive_failures = []
    if zephyr_positive_label_1_rows <= 0:
        positive_failures.append(
            {
                "semantic_bucket": "zephyr_positive",
                "label": "1",
                "reason": "missing unified positive rows",
            }
        )
    if zephyr_positive_bad_label_rows > 0:
        positive_failures.append(
            {
                "semantic_bucket": "zephyr_positive",
                "label": "not_1",
                "actual_rows": zephyr_positive_bad_label_rows,
                "reason": "zephyr_positive rows must all be label 1",
            }
        )

    return {
        "music_split": {
            "music_lyrics_label_0_rows": music_lyrics_rows,
            "music_non_lyric_label_0_rows": music_non_lyric_rows,
            "allow_missing_music_split": bool(allow_missing_music_split),
            "failure_count": len(music_split_failures),
            "failures": music_split_failures,
        },
        "positive_unification": {
            "zephyr_positive_label_1_rows": zephyr_positive_label_1_rows,
            "zephyr_positive_bad_label_rows": zephyr_positive_bad_label_rows,
            "failure_count": len(positive_failures),
            "failures": positive_failures,
        },
    }


def audit_with_sqlite(args: argparse.Namespace) -> tuple[dict, int]:
    db_path = args.sqlite_db
    if db_path.exists():
        db_path.unlink()
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")
    conn.execute(
        """
        CREATE TABLE rows (
            id TEXT PRIMARY KEY,
            row_json TEXT NOT NULL,
            ok INTEGER NOT NULL,
            source TEXT NOT NULL,
            semantic_bucket TEXT,
            partition_hint TEXT,
            output_kind TEXT,
            conditioning_policy TEXT,
            conditioned_path TEXT
        )
        """
    )
    conn.execute("CREATE TABLE failed_ids (id TEXT PRIMARY KEY)")
    conn.execute("CREATE TABLE repaired_ids (id TEXT PRIMARY KEY)")
    input_rows = Counter()
    bad_json_rows = Counter()
    duplicate_ids = 0

    for manifest in args.manifests:
        with conn:
            for line_no, row in iter_jsonl(manifest):
                input_rows[str(manifest)] += 1
                row_id = str(row.get("id") or "")
                if not row_id:
                    bad_json_rows[str(manifest)] += 1
                    continue
                previous = conn.execute("SELECT ok FROM rows WHERE id = ?", (row_id,)).fetchone()
                if previous is not None:
                    duplicate_ids += 1
                ok = 1 if row.get("ok") else 0
                if not ok:
                    conn.execute("INSERT OR IGNORE INTO failed_ids(id) VALUES (?)", (row_id,))
                elif conn.execute("SELECT 1 FROM failed_ids WHERE id = ?", (row_id,)).fetchone():
                    conn.execute("INSERT OR IGNORE INTO repaired_ids(id) VALUES (?)", (row_id,))
                conn.execute(
                    """
                    INSERT OR REPLACE INTO rows
                    (id, row_json, ok, source, semantic_bucket, partition_hint, output_kind, conditioning_policy, conditioned_path)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        row_id,
                        json.dumps(row, ensure_ascii=False, sort_keys=True),
                        ok,
                        f"{manifest}:{line_no}",
                        str(row.get("semantic_bucket", "")),
                        str(row.get("partition_hint", "")),
                        str(row.get("output_kind", "")),
                        str(row.get("conditioning_policy", "")),
                        str(row.get("conditioned_path") or ""),
                    ),
                )

    counters = Counter()
    bucket_counts = Counter()
    missing_outputs = []
    bad_policy = []
    bad_conditioned_root = []
    conditioned_root = args.conditioned_root.resolve() if args.conditioned_root else None
    for (
        row_id,
        row_json,
        ok,
        source,
        semantic_bucket,
        partition_hint,
        output_kind,
        conditioning_policy,
        conditioned_path,
    ) in conn.execute(
        "SELECT id,row_json,ok,source,semantic_bucket,partition_hint,output_kind,conditioning_policy,conditioned_path FROM rows ORDER BY id"
    ):
        counters["rows"] += 1
        counters["ok_rows" if ok else "error_rows"] += 1
        counters[f"semantic:{semantic_bucket}"] += 1
        counters[f"partition:{partition_hint}"] += 1
        counters[f"kind:{output_kind}"] += 1
        row = json.loads(row_json)
        bucket_counts[(partition_hint, semantic_bucket, output_kind, str(row.get("label", "")))] += 1
        if conditioning_policy != "adaptive_hot_v2_5":
            bad_policy.append({"id": row_id, "policy": conditioning_policy, "source": source})
        if ok and conditioned_root is not None:
            conditioned_path_resolved = resolve_output_path(conditioned_path, args.repo_root)
            if not conditioned_path or not path_is_under(conditioned_path_resolved, conditioned_root):
                bad_conditioned_root.append({"id": row_id, "conditioned_path": conditioned_path, "source": source})
        if args.verify_outputs and ok:
            if not conditioned_path or not resolve_output_path(conditioned_path, args.repo_root).exists():
                missing_outputs.append({"id": row_id, "conditioned_path": conditioned_path, "source": source})

    if args.output_jsonl:
        args.output_jsonl.parent.mkdir(parents=True, exist_ok=True)
        with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as out:
            for (row_json,) in conn.execute("SELECT row_json FROM rows ORDER BY id"):
                out.write(row_json + "\n")

    raw_scan = scan_raw_roots(args.raw_root, args.raw_report_csv) if args.raw_root else None
    residual_raw_count = int(raw_scan.get("total_audio_video_files", 0)) if raw_scan else 0
    obsolete_wake_raw_count = int(raw_scan.get("obsolete_wake_raw_count", 0)) if raw_scan else 0
    write_bucket_report(args.bucket_report_csv, bucket_counts)
    expected_bucket_failures = validate_expected_buckets(args.expected_buckets_csv, bucket_counts)
    bucket_assertions = build_bucket_assertions(bucket_counts, args.allow_missing_music_split)
    unique_rows = conn.execute("SELECT COUNT(*) FROM rows").fetchone()[0]
    ever_failed = conn.execute("SELECT COUNT(*) FROM failed_ids").fetchone()[0]
    repaired = conn.execute("SELECT COUNT(*) FROM repaired_ids").fetchone()[0]
    conn.close()
    summary = {
        "manifests": [str(path) for path in args.manifests],
        "output_jsonl": str(args.output_jsonl) if args.output_jsonl else "",
        "sqlite_db": str(db_path),
        "input_rows": dict(input_rows),
        "bad_json_rows": dict(bad_json_rows),
        "unique_rows": unique_rows,
        "duplicate_id_rows": duplicate_ids,
        "ever_failed_ids": ever_failed,
        "repaired_failed_ids": repaired,
        "latest_unresolved_error_ids": int(counters["error_rows"]),
        "missing_output_count": len(missing_outputs),
        "missing_output_examples": missing_outputs[:25],
        "bad_policy_count": len(bad_policy),
        "bad_policy_examples": bad_policy[:25],
        "bad_conditioned_root_count": len(bad_conditioned_root),
        "bad_conditioned_root_examples": bad_conditioned_root[:25],
        "bucket_report_csv": str(args.bucket_report_csv) if args.bucket_report_csv else "",
        "expected_buckets_csv": str(args.expected_buckets_csv) if args.expected_buckets_csv else "",
        "expected_bucket_failure_count": len(expected_bucket_failures),
        "expected_bucket_failures": expected_bucket_failures[:25],
        "bucket_assertions": bucket_assertions,
        "music_split_failure_count": bucket_assertions["music_split"]["failure_count"],
        "positive_unification_failure_count": bucket_assertions["positive_unification"]["failure_count"],
        "residual_raw_blocking_count": 0 if args.allow_residual_raws else residual_raw_count,
        "obsolete_wake_raw_blocking_count": 0 if args.allow_obsolete_wake_raws else obsolete_wake_raw_count,
        "obsolete_wake_raw_count": obsolete_wake_raw_count,
        "obsolete_wake_raw_examples": (raw_scan or {}).get("obsolete_wake_raw_examples", []),
        "allow_residual_raws": bool(args.allow_residual_raws),
        "allow_obsolete_wake_raws": bool(args.allow_obsolete_wake_raws),
        "counters": dict(sorted(counters.items())),
        "raw_scan": raw_scan,
    }
    blocking_raws = residual_raw_count and not args.allow_residual_raws
    blocking_obsolete_wake_raws = obsolete_wake_raw_count and not args.allow_obsolete_wake_raws
    blocking_bucket_assertions = summary["music_split_failure_count"] or summary["positive_unification_failure_count"]
    return summary, 0 if not counters["error_rows"] and not missing_outputs and not bad_policy and not bad_conditioned_root and not expected_bucket_failures and not blocking_bucket_assertions and not blocking_raws and not blocking_obsolete_wake_raws else 2


def main() -> int:
    parser = argparse.ArgumentParser(fromfile_prefix_chars="@")
    parser.add_argument("--repo-root", type=Path, default=Path(r"D:\Claude\heardright"))
    parser.add_argument("--output-jsonl", type=Path, default=None)
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--sqlite-db", type=Path, default=None)
    parser.add_argument("--raw-root", type=Path, action="append", default=[])
    parser.add_argument("--raw-report-csv", type=Path, default=None)
    parser.add_argument("--bucket-report-csv", type=Path, default=None)
    parser.add_argument("--expected-buckets-csv", type=Path, default=None)
    parser.add_argument("--conditioned-root", type=Path, default=Path(r"D:\Claude\heardright\train\conditioned\adaptive_hot_v2_5"))
    parser.add_argument("--allow-residual-raws", action="store_true")
    parser.add_argument("--allow-obsolete-wake-raws", action="store_true")
    parser.add_argument("--allow-missing-music-split", action="store_true")
    parser.add_argument("--verify-outputs", action="store_true")
    parser.add_argument("manifests", type=Path, nargs="+")
    args = parser.parse_args()

    if args.sqlite_db:
        summary, exit_code = audit_with_sqlite(args)
        args.summary_json.parent.mkdir(parents=True, exist_ok=True)
        args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
        print(json.dumps(summary, indent=2, sort_keys=True))
        return exit_code

    rows_by_id: dict[str, dict] = {}
    latest_source: dict[str, str] = {}
    ever_failed: set[str] = set()
    repaired_ids: set[str] = set()
    input_rows = Counter()
    bad_json_rows = Counter()
    duplicate_ids = 0

    for manifest in args.manifests:
        for line_no, row in iter_jsonl(manifest):
            input_rows[str(manifest)] += 1
            row_id = str(row.get("id") or "")
            if not row_id:
                bad_json_rows[str(manifest)] += 1
                continue
            if row_id in rows_by_id:
                duplicate_ids += 1
            if not row.get("ok"):
                ever_failed.add(row_id)
            elif row_id in ever_failed:
                repaired_ids.add(row_id)
            rows_by_id[row_id] = row
            latest_source[row_id] = f"{manifest}:{line_no}"

    counters = Counter()
    bucket_counts = Counter()
    missing_outputs = []
    bad_policy = []
    bad_conditioned_root = []
    conditioned_root = args.conditioned_root.resolve() if args.conditioned_root else None
    rows = []
    for row_id in sorted(rows_by_id):
        row = rows_by_id[row_id]
        rows.append(row)
        ok = bool(row.get("ok"))
        counters["rows"] += 1
        counters["ok_rows" if ok else "error_rows"] += 1
        counters[f"semantic:{row.get('semantic_bucket', '')}"] += 1
        counters[f"partition:{row.get('partition_hint', '')}"] += 1
        counters[f"kind:{row.get('output_kind', '')}"] += 1
        bucket_counts[
            (
                str(row.get("partition_hint", "")),
                str(row.get("semantic_bucket", "")),
                str(row.get("output_kind", "")),
                str(row.get("label", "")),
            )
        ] += 1
        if row.get("conditioning_policy") != "adaptive_hot_v2_5":
            bad_policy.append({"id": row_id, "policy": row.get("conditioning_policy"), "source": latest_source[row_id]})
        conditioned_path = str(row.get("conditioned_path") or "")
        if ok and conditioned_root is not None:
            conditioned_path_resolved = resolve_output_path(conditioned_path, args.repo_root)
            if not conditioned_path or not path_is_under(conditioned_path_resolved, conditioned_root):
                bad_conditioned_root.append({"id": row_id, "conditioned_path": conditioned_path, "source": latest_source[row_id]})
        if args.verify_outputs and ok:
            if not conditioned_path or not resolve_output_path(conditioned_path, args.repo_root).exists():
                missing_outputs.append({"id": row_id, "conditioned_path": conditioned_path, "source": latest_source[row_id]})

    if args.output_jsonl:
        args.output_jsonl.parent.mkdir(parents=True, exist_ok=True)
        with args.output_jsonl.open("w", encoding="utf-8", newline="\n") as out:
            for row in rows:
                out.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")

    raw_scan = scan_raw_roots(args.raw_root, args.raw_report_csv) if args.raw_root else None
    residual_raw_count = int(raw_scan.get("total_audio_video_files", 0)) if raw_scan else 0
    obsolete_wake_raw_count = int(raw_scan.get("obsolete_wake_raw_count", 0)) if raw_scan else 0
    write_bucket_report(args.bucket_report_csv, bucket_counts)
    expected_bucket_failures = validate_expected_buckets(args.expected_buckets_csv, bucket_counts)
    bucket_assertions = build_bucket_assertions(bucket_counts, args.allow_missing_music_split)
    summary = {
        "manifests": [str(path) for path in args.manifests],
        "output_jsonl": str(args.output_jsonl) if args.output_jsonl else "",
        "input_rows": dict(input_rows),
        "bad_json_rows": dict(bad_json_rows),
        "unique_rows": len(rows_by_id),
        "duplicate_id_rows": duplicate_ids,
        "ever_failed_ids": len(ever_failed),
        "repaired_failed_ids": len(repaired_ids),
        "latest_unresolved_error_ids": int(counters["error_rows"]),
        "missing_output_count": len(missing_outputs),
        "missing_output_examples": missing_outputs[:25],
        "bad_policy_count": len(bad_policy),
        "bad_policy_examples": bad_policy[:25],
        "bad_conditioned_root_count": len(bad_conditioned_root),
        "bad_conditioned_root_examples": bad_conditioned_root[:25],
        "bucket_report_csv": str(args.bucket_report_csv) if args.bucket_report_csv else "",
        "expected_buckets_csv": str(args.expected_buckets_csv) if args.expected_buckets_csv else "",
        "expected_bucket_failure_count": len(expected_bucket_failures),
        "expected_bucket_failures": expected_bucket_failures[:25],
        "bucket_assertions": bucket_assertions,
        "music_split_failure_count": bucket_assertions["music_split"]["failure_count"],
        "positive_unification_failure_count": bucket_assertions["positive_unification"]["failure_count"],
        "residual_raw_blocking_count": 0 if args.allow_residual_raws else residual_raw_count,
        "obsolete_wake_raw_blocking_count": 0 if args.allow_obsolete_wake_raws else obsolete_wake_raw_count,
        "obsolete_wake_raw_count": obsolete_wake_raw_count,
        "obsolete_wake_raw_examples": (raw_scan or {}).get("obsolete_wake_raw_examples", []),
        "allow_residual_raws": bool(args.allow_residual_raws),
        "allow_obsolete_wake_raws": bool(args.allow_obsolete_wake_raws),
        "counters": dict(sorted(counters.items())),
        "raw_scan": raw_scan,
    }
    args.summary_json.parent.mkdir(parents=True, exist_ok=True)
    args.summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    blocking_raws = residual_raw_count and not args.allow_residual_raws
    blocking_obsolete_wake_raws = obsolete_wake_raw_count and not args.allow_obsolete_wake_raws
    blocking_bucket_assertions = summary["music_split_failure_count"] or summary["positive_unification_failure_count"]
    return 0 if not counters["error_rows"] and not missing_outputs and not bad_policy and not bad_conditioned_root and not expected_bucket_failures and not blocking_bucket_assertions and not blocking_raws and not blocking_obsolete_wake_raws else 2


if __name__ == "__main__":
    raise SystemExit(main())
