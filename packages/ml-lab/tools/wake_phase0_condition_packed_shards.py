#!/usr/bin/env python
"""Condition worklist rows into packed PCM16 shard files.

This is the high-throughput alternative to wake_phase0_condition_audio.py. It
keeps the same row IDs, labels, semantic buckets, adaptive_hot_v2_5 policy, and
source-group delete-on-success behavior, but writes many clips into large shard
files instead of one tiny WAV per row.
"""

from __future__ import annotations

import argparse
import json
import time
from collections import Counter
from pathlib import Path
from typing import Iterable

import numpy as np

import wake_phase0_condition_audio as base


DEFAULT_OUTPUT_ROOT = Path(r"D:\Claude\heardright\train\conditioned\adaptive_hot_v2_5\_packed_shards")
DEFAULT_SHARD_BYTES = 1024 * 1024 * 1024


def iter_jsonl(path: Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def write_jsonl_row(handle, row: dict) -> None:
    handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
    handle.flush()


def write_progress(handle, event: str, **fields) -> None:
    if not handle:
        return
    row = {"event": event, "epoch": round(time.time(), 3)}
    row.update(fields)
    write_jsonl_row(handle, row)


def packed_done_index_path_for(manifest_path: Path) -> Path:
    return Path(str(manifest_path) + ".packed_done_ids.jsonl")


def packed_done_index_meta_path_for(index_path: Path) -> Path:
    return Path(str(index_path) + ".meta.json")


def load_ok_ids_without_artifact_checks(manifest_path: Path) -> tuple[set[str], dict]:
    done: set[str] = set()
    index_path = packed_done_index_path_for(manifest_path)
    meta_path = packed_done_index_meta_path_for(index_path)
    stat = manifest_path.stat()
    expected_meta = {
        "manifest_path": str(manifest_path),
        "manifest_size": stat.st_size,
        "manifest_mtime_ns": stat.st_mtime_ns,
    }
    info = {
        "done_index_path": str(index_path),
        "done_index_used": False,
        "done_index_rebuilt": False,
    }
    if index_path.exists() and meta_path.exists():
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            if all(meta.get(key) == value for key, value in expected_meta.items()):
                with index_path.open("r", encoding="utf-8", errors="replace") as f:
                    for line in f:
                        row_id = line.strip()
                        if row_id:
                            done.add(row_id)
                info["done_index_used"] = True
                return done, info
        except Exception:
            done.clear()

    with manifest_path.open("rb") as f:
        for raw_line in f:
            line = raw_line.strip()
            if not line:
                continue
            try:
                row = json.loads(line.decode("utf-8", errors="replace"))
            except json.JSONDecodeError:
                continue
            if row.get("ok") and row.get("id"):
                done.add(str(row["id"]))

    index_path.parent.mkdir(parents=True, exist_ok=True)
    with index_path.open("w", encoding="utf-8", newline="\n") as f:
        for row_id in sorted(done):
            f.write(row_id + "\n")
    meta = dict(expected_meta)
    meta["updated_epoch"] = time.time()
    meta_path.write_text(json.dumps(meta, indent=2, sort_keys=True), encoding="utf-8")
    info["done_index_rebuilt"] = True
    return done, info


def load_done_ids(manifest_paths: list[Path]) -> tuple[set[str], list[dict]]:
    done: set[str] = set()
    infos: list[dict] = []
    for manifest_path in manifest_paths:
        if not manifest_path.exists():
            infos.append({"manifest": str(manifest_path), "exists": False, "done_ids": 0})
            continue
        manifest_done, info = load_ok_ids_without_artifact_checks(manifest_path)
        done.update(manifest_done)
        info.update({"manifest": str(manifest_path), "exists": True, "done_ids": len(manifest_done)})
        infos.append(info)
    return done, infos


def to_pcm16_bytes(audio: np.ndarray) -> bytes:
    clipped = np.clip(np.asarray(audio, dtype="float32"), -1.0, 1.0)
    pcm = np.round(clipped * 32767.0).astype("<i2", copy=False)
    return pcm.tobytes(order="C")


class PackedShardWriter:
    def __init__(self, output_root: Path, index_path: Path, shard_prefix: str, max_shard_bytes: int):
        self.output_root = output_root
        self.index_path = index_path
        self.shard_prefix = shard_prefix
        self.max_shard_bytes = max_shard_bytes
        self.shard_index = -1
        self.handle = None
        self.path: Path | None = None
        self.offset = 0
        self.index_path.parent.mkdir(parents=True, exist_ok=True)
        self.index_handle = self.index_path.open("a", encoding="utf-8", newline="\n")

    def _open_next(self) -> None:
        if self.handle:
            self.handle.flush()
            self.handle.close()
        self.shard_index += 1
        shard_dir = self.output_root / f"{self.shard_prefix}_{self.shard_index // 1000:03d}"
        shard_dir.mkdir(parents=True, exist_ok=True)
        self.path = shard_dir / f"{self.shard_prefix}_{self.shard_index:06d}.pcm16"
        self.handle = self.path.open("ab")
        self.offset = self.path.stat().st_size if self.path.exists() else 0

    def write_clip(self, row: dict, clip: np.ndarray, source_meta: dict, scope: str) -> dict:
        payload = to_pcm16_bytes(clip)
        if self.handle is None or self.path is None or (self.offset and self.offset + len(payload) > self.max_shard_bytes):
            self._open_next()
        assert self.handle is not None and self.path is not None
        offset = self.offset
        self.handle.write(payload)
        self.offset += len(payload)
        out = dict(row)
        out.update(
            {
                "ok": True,
                "error": "",
                "conditioned_path": str(self.path),
                "conditioned_artifact": "packed_pcm16_shard",
                "conditioned_shard_path": str(self.path),
                "conditioned_shard_offset_bytes": offset,
                "conditioned_shard_nbytes": len(payload),
                "conditioned_dtype": "pcm16_le",
                "conditioned_sample_rate": base.SAMPLE_RATE,
                "conditioned_samples": int(np.asarray(clip).size),
                "conditioned_duration_s": round(float(np.asarray(clip).size) / base.SAMPLE_RATE, 6),
                "conditioned_bytes": len(payload),
                "conditioning_policy": base.POLICY,
                "conditioning_source_scope": scope,
            }
        )
        out.update(source_meta)
        return out

    def write_index_row(self, row: dict) -> None:
        write_jsonl_row(self.index_handle, row)

    def flush_data(self) -> None:
        if self.handle:
            self.handle.flush()

    def close(self) -> None:
        try:
            if self.handle:
                self.handle.flush()
                self.handle.close()
        finally:
            self.index_handle.flush()
            self.index_handle.close()


def row_error(row: dict, source_meta: dict, error: str) -> dict:
    out = base.row_error(row, source_meta, error)
    out["conditioned_artifact"] = "packed_pcm16_shard"
    out["conditioned_shard_path"] = ""
    out["conditioned_shard_offset_bytes"] = None
    out["conditioned_shard_nbytes"] = 0
    return out


def process_group(
    rows: list[dict],
    writer: PackedShardWriter,
    delete_source_on_success: bool,
    delete_root: Path,
    progress_handle=None,
) -> tuple[list[dict], Counter]:
    source_path = Path(rows[0]["source_path"])
    counters: Counter = Counter()
    out_rows: list[dict] = []
    write_progress(progress_handle, "source_start", source_path=str(source_path), rows=len(rows))
    if base.should_process_group_bounded(rows):
        for block_start_s, block_end_s, block_rows in base.iter_bounded_blocks(rows):
            block_started = time.time()
            write_progress(
                progress_handle,
                "block_start",
                source_path=str(source_path),
                block_start_s=round(float(block_start_s), 6),
                block_end_s=round(float(block_end_s), 6),
                rows=len(block_rows),
            )
            try:
                started = time.time()
                audio, sr = base.read_audio_segment(source_path, block_start_s, block_end_s)
                conditioned, source_meta = base.condition_array(audio, sr, started)
                source_meta.update(
                    {
                        "conditioning_block_start_s": round(float(block_start_s), 6),
                        "conditioning_block_end_s": round(float(block_end_s), 6),
                        "conditioning_block_duration_s": round(float(block_end_s) - float(block_start_s), 6),
                    }
                )
                for row in block_rows:
                    clip = base.slice_row_from_block(conditioned, row, block_start_s)
                    out_rows.append(writer.write_clip(row, clip, source_meta, "bounded_block_then_slice_packed"))
                    counters["written"] += 1
                    counters["bounded_block_rows"] += 1
                    counters[f"kind:{row['output_kind']}"] += 1
                counters["bounded_blocks"] += 1
            except Exception as exc:
                source_meta = {"source_path_failed": str(source_path)}
                for row in block_rows:
                    out_rows.append(row_error(row, source_meta, str(exc)))
                    counters["errors"] += 1
            writer.flush_data()
            write_progress(
                progress_handle,
                "block_done",
                source_path=str(source_path),
                block_start_s=round(float(block_start_s), 6),
                block_end_s=round(float(block_end_s), 6),
                rows=len(block_rows),
                elapsed_s=round(time.time() - block_started, 3),
                cumulative_written=int(counters.get("written", 0)),
                cumulative_errors=int(counters.get("errors", 0)),
            )
        rows_out = finalize_group_delete(source_path, out_rows, delete_source_on_success, delete_root, counters)
        write_progress(
            progress_handle,
            "source_done",
            source_path=str(source_path),
            rows=len(rows),
            written=int(counters.get("written", 0)),
            errors=int(counters.get("errors", 0)),
        )
        return rows_out

    try:
        conditioned, source_meta = base.condition_source(source_path)
    except Exception as exc:
        source_meta = {"source_path_failed": str(source_path)}
        for row in rows:
            out_rows.append(row_error(row, source_meta, str(exc)))
            counters["errors"] += 1
        return out_rows, counters

    for row in rows:
        try:
            clip = base.slice_row(conditioned, row)
            out_rows.append(writer.write_clip(row, clip, source_meta, "full_source_then_slice_packed"))
            counters["written"] += 1
            counters[f"kind:{row['output_kind']}"] += 1
        except Exception as exc:
            out_rows.append(row_error(row, source_meta, str(exc)))
            counters["errors"] += 1
    writer.flush_data()
    rows_out = finalize_group_delete(source_path, out_rows, delete_source_on_success, delete_root, counters)
    write_progress(
        progress_handle,
        "source_done",
        source_path=str(source_path),
        rows=len(rows),
        written=int(counters.get("written", 0)),
        errors=int(counters.get("errors", 0)),
    )
    return rows_out


def finalize_group_delete(source_path: Path, out_rows: list[dict], delete_source_on_success: bool, delete_root: Path, counters: Counter) -> tuple[list[dict], Counter]:
    if delete_source_on_success:
        deleted, reason = base.maybe_delete_source(source_path, out_rows, delete_root)
        if deleted:
            counters["source_deleted_after_success"] += 1
            for row in out_rows:
                row["source_delete_status"] = "deleted_after_conditioning"
        else:
            counters[f"source_delete_skipped:{reason}"] += 1
            for row in out_rows:
                row["source_delete_status"] = f"skipped:{reason}"
    return out_rows, counters


def summarize(path: Path, args: argparse.Namespace, counters: Counter, done_count: int, elapsed_s: float) -> dict:
    summary = {
        "worklist_jsonl": str(args.worklist_jsonl),
        "output_root": str(args.output_root),
        "output_manifest_jsonl": str(args.output_manifest_jsonl),
        "output_index_jsonl": str(args.output_index_jsonl),
        "conditioning_policy": base.POLICY,
        "conditioned_artifact": "packed_pcm16_shard",
        "max_shard_bytes": args.max_shard_bytes,
        "done_rows_loaded": done_count,
        "done_manifest_jsonl": [str(path) for path in getattr(args, "done_manifest_paths", [])],
        "done_manifest_load_info": getattr(args, "done_manifest_load_info", []),
        "elapsed_s": round(elapsed_s, 3),
        "counters": dict(sorted(counters.items())),
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")
    return summary


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--worklist-jsonl", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, default=DEFAULT_OUTPUT_ROOT)
    parser.add_argument("--output-manifest-jsonl", type=Path, required=True)
    parser.add_argument("--output-index-jsonl", type=Path, default=None)
    parser.add_argument("--progress-jsonl", type=Path, default=None)
    parser.add_argument("--done-manifest-jsonl", type=Path, action="append", default=[])
    parser.add_argument("--summary-json", type=Path, required=True)
    parser.add_argument("--shard-prefix", default="")
    parser.add_argument("--max-shard-bytes", type=int, default=DEFAULT_SHARD_BYTES)
    parser.add_argument("--limit-rows", type=int, default=0)
    parser.add_argument("--limit-sources", type=int, default=0)
    parser.add_argument("--delete-source-on-success", action="store_true")
    parser.add_argument("--delete-train-root", type=Path, default=Path(r"D:\Claude\heardright\train"))
    args = parser.parse_args()

    args.output_index_jsonl = args.output_index_jsonl or Path(str(args.output_manifest_jsonl) + ".index.jsonl")
    args.progress_jsonl = args.progress_jsonl or Path(str(args.summary_json) + ".progress.jsonl")
    shard_prefix = args.shard_prefix or args.output_manifest_jsonl.stem.replace("_conditioned_manifest", "")
    done_manifest_paths = list(args.done_manifest_jsonl) + [args.output_manifest_jsonl]
    done, done_manifest_load_info = load_done_ids(done_manifest_paths)
    counters: Counter = Counter()
    started = time.time()
    current_source = None
    group: list[dict] = []
    seen_sources = 0
    pending_rows = 0

    args.output_manifest_jsonl.parent.mkdir(parents=True, exist_ok=True)
    writer = PackedShardWriter(args.output_root, args.output_index_jsonl, shard_prefix, args.max_shard_bytes)
    try:
        args.progress_jsonl.parent.mkdir(parents=True, exist_ok=True)
        with args.output_manifest_jsonl.open("a", encoding="utf-8", newline="\n") as manifest, args.progress_jsonl.open(
            "a", encoding="utf-8", newline="\n"
        ) as progress:
            for row in iter_jsonl(args.worklist_jsonl):
                counters["seen_worklist_rows"] += 1
                if row["id"] in done:
                    counters["skipped_done_rows"] += 1
                    continue
                source = row["source_path"]
                if current_source is None:
                    current_source = source
                    seen_sources = 1
                elif source != current_source:
                    rows, group_counts = process_group(
                        group, writer, args.delete_source_on_success, args.delete_train_root, progress
                    )
                    counters.update(group_counts)
                    for out_row in rows:
                        write_jsonl_row(manifest, out_row)
                        writer.write_index_row(out_row)
                    group = []
                    current_source = source
                    seen_sources += 1
                    if args.limit_sources and seen_sources > args.limit_sources:
                        break
                group.append(row)
                pending_rows += 1
                if args.limit_rows and pending_rows >= args.limit_rows:
                    break
            if group:
                rows, group_counts = process_group(group, writer, args.delete_source_on_success, args.delete_train_root, progress)
                counters.update(group_counts)
                for out_row in rows:
                    write_jsonl_row(manifest, out_row)
                    writer.write_index_row(out_row)
    finally:
        writer.close()

    counters["pending_rows_considered"] = pending_rows
    counters["sources_considered"] = seen_sources if pending_rows else 0
    args.done_manifest_paths = done_manifest_paths
    args.done_manifest_load_info = done_manifest_load_info
    summary = summarize(args.summary_json, args, counters, len(done), time.time() - started)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if counters.get("errors", 0) == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
