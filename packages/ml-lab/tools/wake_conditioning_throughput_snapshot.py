#!/usr/bin/env python
"""Throttled throughput snapshot for the active corpus conditioning lanes.

The heartbeat can call this without turning into frequent polling. If the last
snapshot is newer than --min-interval-s, the script returns the cached report
and does not rescan manifests.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import time
from pathlib import Path


DEFAULT_REPO = Path(r"D:\Claude\heardright")
DEFAULT_HISTORY = Path(r"train\reports\conditioning_throughput_history.jsonl")
DEFAULT_REPORT = Path(r"train\reports\conditioning_throughput_latest.json")


def count_jsonl_bytes(path: Path) -> dict:
    lines = 0
    ok_true = 0
    ok_false = 0
    if not path.exists():
        return {"path": str(path), "missing": True, "lines": 0, "ok_true": 0, "ok_false": 0, "bytes": 0, "mtime": 0.0}
    with path.open("rb", buffering=1024 * 1024) as f:
        for chunk in iter(lambda: f.read(8 * 1024 * 1024), b""):
            lines += chunk.count(b"\n")
            ok_true += chunk.count(b'"ok": true') + chunk.count(b'"ok":true')
            ok_false += chunk.count(b'"ok": false') + chunk.count(b'"ok":false')
    stat = path.stat()
    return {
        "path": str(path),
        "missing": False,
        "lines": lines,
        "ok_true": ok_true,
        "ok_false": ok_false,
        "bytes": stat.st_size,
        "mtime": stat.st_mtime,
    }


def count_latest_manifest_state(path: Path, sqlite_db: Path | None = None) -> dict:
    if not path.exists():
        return {"path": str(path), "missing": True, "lines": 0, "ok_true": 0, "ok_false": 0, "bytes": 0, "mtime": 0.0}
    stat = path.stat()
    if sqlite_db is None:
        latest: dict[str, bool] = {}
        lines = 0
        with path.open("r", encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                lines += 1
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                row_id = str(row.get("id") or "")
                if row_id:
                    latest[row_id] = bool(row.get("ok"))
        return {
            "path": str(path),
            "missing": False,
            "lines": lines,
            "ok_true": sum(1 for value in latest.values() if value),
            "ok_false": sum(1 for value in latest.values() if not value),
            "bytes": stat.st_size,
            "mtime": stat.st_mtime,
        }

    sqlite_db.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(sqlite_db))
    try:
        conn.execute("DROP TABLE IF EXISTS latest")
        conn.execute("CREATE TABLE latest (id TEXT PRIMARY KEY, ok INTEGER NOT NULL)")
        lines = 0
        with conn:
            with path.open("r", encoding="utf-8", errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    lines += 1
                    try:
                        row = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    row_id = str(row.get("id") or "")
                    if row_id:
                        conn.execute("INSERT OR REPLACE INTO latest(id, ok) VALUES (?, ?)", (row_id, 1 if row.get("ok") else 0))
        ok_true = int(conn.execute("SELECT COUNT(*) FROM latest WHERE ok = 1").fetchone()[0])
        ok_false = int(conn.execute("SELECT COUNT(*) FROM latest WHERE ok = 0").fetchone()[0])
        return {
            "path": str(path),
            "missing": False,
            "lines": lines,
            "ok_true": ok_true,
            "ok_false": ok_false,
            "bytes": stat.st_size,
            "mtime": stat.st_mtime,
        }
    finally:
        conn.close()


def count_lines(path: Path) -> int:
    if not path.exists():
        return 0
    lines = 0
    with path.open("rb", buffering=1024 * 1024) as f:
        for chunk in iter(lambda: f.read(8 * 1024 * 1024), b""):
            lines += chunk.count(b"\n")
    return lines


def default_manifest_worklist_pairs(repo_root: Path) -> list[tuple[Path, Path]]:
    existing = repo_root / r"train\manifests\source_truth_existing_block_20260608_0138"
    blockrun = repo_root / r"train\manifests\source_truth_blockrun_20260608_0140"
    pairs: list[tuple[Path, Path]] = []
    for name in [
        "audiobooks_shard_00",
        "audiobooks_shard_01",
        "audiobooks_shard_02",
        "tv_shard_00",
        "tv_shard_01",
        "expansion_music_shard_00",
        "expansion_music_shard_01",
    ]:
        pairs.append((blockrun / f"{name}_conditioned_manifest.jsonl", existing / f"{name}_worklist.jsonl"))
    pairs.append(
        (
            repo_root / r"train\manifests\e_archive_stream_20260607_211948_conditioned_manifest.jsonl",
            repo_root / r"train\manifests\e_archive_stream_20260607_211948_conditioning_worklist.jsonl",
        )
    )
    return pairs


def load_pairs_file(pairs_file: Path, repo_root: Path) -> list[tuple[Path, Path]]:
    pairs: list[tuple[Path, Path]] = []
    with pairs_file.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "\t" in line:
                manifest_text, worklist_text = line.split("\t", 1)
            else:
                manifest_text, worklist_text = line.split(",", 1)
            manifest = Path(manifest_text.strip())
            worklist = Path(worklist_text.strip())
            if not manifest.is_absolute():
                manifest = repo_root / manifest
            if not worklist.is_absolute():
                worklist = repo_root / worklist
            pairs.append((manifest, worklist))
    return pairs


def load_last_snapshot(history_path: Path) -> dict | None:
    if not history_path.exists():
        return None
    last = None
    with history_path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                last = json.loads(line)
            except json.JSONDecodeError:
                continue
    return last


def write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=DEFAULT_REPO)
    parser.add_argument("--history-jsonl", type=Path, default=DEFAULT_HISTORY)
    parser.add_argument("--report-json", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--pairs-file", type=Path, default=None)
    parser.add_argument("--sqlite-dir", type=Path, default=None)
    parser.add_argument("--fast-append-counts", action="store_true")
    parser.add_argument("--min-interval-s", type=int, default=40 * 60)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    repo_root = args.repo_root
    history_path = args.history_jsonl if args.history_jsonl.is_absolute() else repo_root / args.history_jsonl
    report_path = args.report_json if args.report_json.is_absolute() else repo_root / args.report_json
    now = time.time()
    last = load_last_snapshot(history_path)
    if last and not args.force:
        age_s = now - float(last.get("created_epoch", 0.0))
        if age_s < args.min_interval_s:
            cached = dict(last)
            cached["skipped_due_to_min_interval"] = True
            cached["snapshot_age_s"] = round(age_s, 3)
            cached["min_interval_s"] = args.min_interval_s
            write_json(report_path, cached)
            print(json.dumps(cached, indent=2, sort_keys=True))
            return 0

    lane_reports = []
    total_done = 0
    total_worklist_rows = 0
    total_false = 0
    pairs = load_pairs_file(args.pairs_file, repo_root) if args.pairs_file else default_manifest_worklist_pairs(repo_root)
    sqlite_dir = None
    if args.sqlite_dir:
        sqlite_dir = args.sqlite_dir if args.sqlite_dir.is_absolute() else repo_root / args.sqlite_dir
    for index, (manifest, worklist) in enumerate(pairs):
        sqlite_db = (sqlite_dir / f"throughput_lane_{index:02d}.sqlite") if sqlite_dir else None
        manifest_counts = count_jsonl_bytes(manifest) if args.fast_append_counts else count_latest_manifest_state(manifest, sqlite_db)
        worklist_rows = count_lines(worklist)
        done = int(manifest_counts["ok_true"])
        total_done += done
        total_false += int(manifest_counts["ok_false"])
        total_worklist_rows += worklist_rows
        remaining = max(0, worklist_rows - done)
        lane_reports.append(
            {
                "manifest": str(manifest),
                "worklist": str(worklist),
                "worklist_rows": worklist_rows,
                "ok_true_rows": done,
                "ok_false_rows_observed": int(manifest_counts["ok_false"]),
                "manifest_lines": int(manifest_counts["lines"]),
                "remaining_rows_estimate": remaining,
                "manifest_mtime_epoch": manifest_counts["mtime"],
            }
        )

    throughput_rows_per_hour = None
    eta_hours = None
    if last:
        elapsed_h = (now - float(last.get("created_epoch", now))) / 3600.0
        delta_rows = total_done - int(last.get("total_ok_true_rows", 0))
        if elapsed_h > 0 and delta_rows >= 0:
            throughput_rows_per_hour = delta_rows / elapsed_h
            if throughput_rows_per_hour > 0:
                eta_hours = max(0, total_worklist_rows - total_done) / throughput_rows_per_hour

    snapshot = {
        "created_epoch": now,
        "created_local": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(now)),
        "total_worklist_rows": total_worklist_rows,
        "total_ok_true_rows": total_done,
        "total_ok_false_rows_observed": total_false,
        "total_remaining_rows_estimate": max(0, total_worklist_rows - total_done),
        "throughput_rows_per_hour_since_previous": round(throughput_rows_per_hour, 3) if throughput_rows_per_hour is not None else None,
        "eta_hours_at_recent_throughput": round(eta_hours, 3) if eta_hours is not None else None,
        "lanes": lane_reports,
        "skipped_due_to_min_interval": False,
        "min_interval_s": args.min_interval_s,
    }
    history_path.parent.mkdir(parents=True, exist_ok=True)
    with history_path.open("a", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(snapshot, ensure_ascii=False, sort_keys=True) + "\n")
    write_json(report_path, snapshot)
    print(json.dumps(snapshot, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
