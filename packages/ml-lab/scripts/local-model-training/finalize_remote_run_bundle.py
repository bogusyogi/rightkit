#!/usr/bin/env python3
"""Create a verified off-box transfer bundle for a terminal L1 training run."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import tarfile
import time
from typing import Any


TERMINAL_STATUSES = {
    "complete",
    "complete_with_rejections",
    "completed_no_viable_candidates",
    "failed",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return f"sha256:{digest.hexdigest()}"


def transferable(path: Path, run_root: Path) -> bool:
    relative = path.relative_to(run_root)
    parts = relative.parts
    if any(part in {"unsloth_compiled_cache", "_billing-guard"} for part in parts):
        return False
    if "native-assets" in parts and path.suffix.lower() not in {".json", ".jsonl", ".log"}:
        return False
    if any(part.startswith("checkpoint-") for part in parts):
        return False
    if any(".partial-" in part or part.endswith(".tmp") for part in parts):
        return False
    if path.name in {"active-run.lock"}:
        return False
    return path.is_file()


def build_bundle(
    run_root: Path,
    archive: Path,
    manifest_path: Path,
    lane: str | None = None,
) -> dict[str, Any]:
    run_root = run_root.resolve()
    archive = archive.resolve()
    manifest_path = manifest_path.resolve()
    state_path = run_root / "state.json"
    state = json.loads(state_path.read_text("utf-8"))
    if lane is None and state.get("status") not in TERMINAL_STATUSES:
        raise RuntimeError(f"run is not terminal: {state.get('status')!r}")
    if lane is not None:
        if not re.fullmatch(r"[a-z0-9_]+", lane):
            raise RuntimeError(f"invalid lane: {lane!r}")
        completion_receipt = run_root / "stage-receipts" / f"{lane}--gguf-evaluate.json"
        if not completion_receipt.is_file():
            raise RuntimeError(f"lane has no completed GGUF evaluation receipt: {lane}")

    def selected(path: Path) -> bool:
        if not transferable(path, run_root):
            return False
        if lane is None:
            return True
        relative = path.relative_to(run_root)
        return (
            relative.as_posix() == "state.json"
            or relative.parts[0] == lane
            or (
                relative.parts[0] == "stage-receipts"
                and relative.name.startswith(f"{lane}--")
            )
        )

    files = sorted(
        (path for path in run_root.rglob("*") if selected(path)),
        key=lambda path: path.relative_to(run_root).as_posix(),
    )
    if state_path not in files:
        raise RuntimeError("terminal bundle is missing state.json")
    entries = [
        {
            "path": path.relative_to(run_root).as_posix(),
            "bytes": path.stat().st_size,
            "sha256": sha256(path),
        }
        for path in files
    ]
    archive.parent.mkdir(parents=True, exist_ok=True)
    temporary_archive = archive.with_name(f"{archive.name}.partial")
    with tarfile.open(temporary_archive, "w:gz", compresslevel=6) as bundle:
        for path in files:
            bundle.add(path, arcname=path.relative_to(run_root).as_posix(), recursive=False)
    temporary_archive.replace(archive)
    manifest = {
        "schema_version": (
            "heardright.l1.final_run_transfer.v1"
            if lane is None
            else "heardright.l1.lane_transfer.v1"
        ),
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "run_root": str(run_root),
        "terminal_status": state["status"] if lane is None else None,
        "run_status": state.get("status"),
        "lane": lane,
        "state_sha256": sha256(state_path),
        "file_count": len(entries),
        "total_uncompressed_bytes": sum(entry["bytes"] for entry in entries),
        "files": entries,
        "archive": str(archive),
        "archive_bytes": archive.stat().st_size,
        "archive_sha256": sha256(archive),
        "excluded_epoch_checkpoints": "already transferred by per-epoch Windows receipts",
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    temporary_manifest = manifest_path.with_name(f"{manifest_path.name}.partial")
    temporary_manifest.write_text(json.dumps(manifest, indent=2) + "\n", "utf-8")
    temporary_manifest.replace(manifest_path)
    return manifest


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-root", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--lane")
    return parser.parse_args()


if __name__ == "__main__":
    args = parse_args()
    result = build_bundle(
        args.run_root,
        args.archive,
        args.manifest,
        args.lane,
    )
    print(json.dumps({
        "status": "ready",
        "archive": result["archive"],
        "sha256": result["archive_sha256"],
        "files": result["file_count"],
    }))
