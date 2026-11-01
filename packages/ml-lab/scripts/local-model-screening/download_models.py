#!/usr/bin/env python3
"""Resolve, download, and verify the HeardRight GGUF screening roster.

The downloader resolves each requested Hub branch to an immutable commit before
starting transfers. Hugging Face's local-dir cache supplies interrupted-download
resume; completed payloads are independently checked against LFS SHA-256.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from huggingface_hub import HfApi, hf_hub_download


SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_MANIFEST = SCRIPT_DIR / "manifest.json"
DEFAULT_WINDOWS_ROOT = Path("E:/models/heardright-local-screening")
PRINT_LOCK = threading.Lock()


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def say(message: str) -> None:
    with PRINT_LOCK:
        print(message, flush=True)


def remote_sha256(sibling: Any) -> str | None:
    lfs = getattr(sibling, "lfs", None)
    if isinstance(lfs, dict):
        value = lfs.get("sha256") or lfs.get("oid")
    else:
        value = getattr(lfs, "sha256", None) if lfs else None
    if not value or set(str(value)) == {"*"}:
        return None
    return str(value).lower()


def resolve_model(api: HfApi, model: dict[str, Any]) -> dict[str, Any]:
    info = api.model_info(
        model["repo"],
        revision=model.get("revision", "main"),
        files_metadata=True,
    )
    sibling = next(
        (item for item in info.siblings or [] if item.rfilename == model["filename"]),
        None,
    )
    if sibling is None:
        raise RuntimeError(f"{model['repo']} has no file {model['filename']}")

    resolved_bytes = getattr(sibling, "size", None)
    resolved_sha = remote_sha256(sibling)
    expected_bytes = model.get("expected_bytes")
    expected_sha = model.get("expected_sha256")
    if expected_bytes and resolved_bytes and expected_bytes != resolved_bytes:
        raise RuntimeError(
            f"{model['id']} remote size drift: expected {expected_bytes}, got {resolved_bytes}"
        )
    if expected_sha and resolved_sha and expected_sha.lower() != resolved_sha:
        raise RuntimeError(
            f"{model['id']} remote SHA drift: expected {expected_sha}, got {resolved_sha}"
        )

    tags = list(info.tags or [])
    return {
        **model,
        "resolved_revision": info.sha,
        "resolved_bytes": resolved_bytes or expected_bytes,
        "resolved_sha256": resolved_sha or expected_sha,
        "gated": info.gated,
        "license_tags": sorted(tag for tag in tags if tag.startswith("license:")),
        "resolved_at": now_iso(),
    }


def quarantine_invalid(path: Path, reason: str) -> None:
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    backup = path.with_name(f"{path.name}.invalid-{stamp}")
    path.replace(backup)
    say(f"[RETAINED] {path.name} -> {backup.name} ({reason})")


def verify_local(path: Path, resolved: dict[str, Any]) -> tuple[bool, str | None]:
    expected_bytes = resolved.get("resolved_bytes")
    if expected_bytes and path.stat().st_size != expected_bytes:
        return False, f"size {path.stat().st_size} != {expected_bytes}"
    actual_sha = sha256_file(path)
    expected_sha = resolved.get("resolved_sha256")
    if expected_sha and actual_sha != expected_sha.lower():
        return False, f"sha256 {actual_sha} != {expected_sha}"
    resolved["verified_bytes"] = path.stat().st_size
    resolved["verified_sha256"] = actual_sha
    resolved["verified_at"] = now_iso()
    return True, None


def fetch_one(root: Path, resolved: dict[str, Any], verify_only: bool) -> dict[str, Any]:
    model_dir = root / "models" / resolved["id"]
    model_dir.mkdir(parents=True, exist_ok=True)
    target = model_dir / resolved["filename"]

    if target.exists():
        valid, reason = verify_local(target, resolved)
        if valid:
            say(f"[CACHED] {resolved['id']} ({target.stat().st_size / 1e6:.1f} MB)")
            return resolved
        if verify_only:
            raise RuntimeError(f"{resolved['id']} failed verification: {reason}")
        quarantine_invalid(target, reason or "verification failure")
    elif verify_only:
        raise RuntimeError(f"{resolved['id']} is missing: {target}")

    say(f"[START]  {resolved['id']} <- {resolved['repo']}@{resolved['resolved_revision'][:12]}")
    downloaded = Path(
        hf_hub_download(
            repo_id=resolved["repo"],
            filename=resolved["filename"],
            revision=resolved["resolved_revision"],
            local_dir=model_dir,
        )
    )
    valid, reason = verify_local(downloaded, resolved)
    if not valid:
        raise RuntimeError(f"{resolved['id']} failed post-download verification: {reason}")
    say(f"[OK]     {resolved['id']} ({downloaded.stat().st_size / 1e6:.1f} MB)")
    return resolved


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(os.environ.get("RIGHTKIT_ML_MODEL_ROOT", ""))
        if os.environ.get("RIGHTKIT_ML_MODEL_ROOT")
        else (DEFAULT_WINDOWS_ROOT if os.name == "nt" else Path.home() / "heardright-local-screening"),
    )
    parser.add_argument("--only", action="append", default=[], help="Candidate id; repeat as needed")
    parser.add_argument("--workers", type=int, default=3)
    parser.add_argument("--preflight", action="store_true", help="Resolve and lock metadata without downloading")
    parser.add_argument("--verify-only", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.workers < 1 or args.workers > 6:
        raise SystemExit("--workers must be between 1 and 6")

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    selected = [
        model for model in manifest["models"] if not args.only or model["id"] in set(args.only)
    ]
    unknown = sorted(set(args.only) - {model["id"] for model in manifest["models"]})
    if unknown:
        raise SystemExit(f"Unknown candidate id(s): {', '.join(unknown)}")
    if not selected:
        raise SystemExit("No models selected")

    args.root.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
    api = HfApi()
    resolved: list[dict[str, Any]] = []
    errors: list[str] = []
    say(f"Root: {args.root.resolve()}")
    say(f"Candidates: {len(selected)} | workers: {args.workers}")

    with ThreadPoolExecutor(max_workers=min(args.workers, len(selected))) as pool:
        futures = {pool.submit(resolve_model, api, model): model for model in selected}
        for future in as_completed(futures):
            model = futures[future]
            try:
                item = future.result()
                resolved.append(item)
                say(f"[RESOLVED] {model['id']} -> {item['resolved_revision'][:12]}")
            except Exception as exc:  # noqa: BLE001 - aggregate all candidate failures
                errors.append(f"{model['id']}: {exc}")
                say(f"[FAIL] {model['id']}: {exc}")

    resolved.sort(key=lambda item: [m["id"] for m in selected].index(item["id"]))
    lock = {
        "schema": "heardright.local_model_screening_lock.v1",
        "manifest": str(args.manifest.resolve()),
        "root": str(args.root.resolve()),
        "created_at": now_iso(),
        "models": resolved,
        "errors": errors,
    }
    lock_path = args.root / "manifest.lock.json"
    lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
    if errors:
        return 1

    total = sum(int(item.get("resolved_bytes") or 0) for item in resolved)
    say(f"Resolved total: {total / 1e9:.2f} GB | lock: {lock_path}")
    if args.preflight:
        return 0

    completed: list[dict[str, Any]] = []
    with ThreadPoolExecutor(max_workers=min(args.workers, len(resolved))) as pool:
        futures = {
            pool.submit(fetch_one, args.root, item, args.verify_only): item for item in resolved
        }
        for future in as_completed(futures):
            item = futures[future]
            try:
                completed.append(future.result())
            except Exception as exc:  # noqa: BLE001 - aggregate all candidate failures
                errors.append(f"{item['id']}: {exc}")
                say(f"[FAIL] {item['id']}: {exc}")

    completed.sort(key=lambda item: [m["id"] for m in selected].index(item["id"]))
    results = {
        "schema": "heardright.local_model_screening_download_results.v1",
        "completed_at": now_iso(),
        "root": str(args.root.resolve()),
        "requested": len(selected),
        "completed": len(completed),
        "errors": errors,
        "models": completed,
    }
    results_path = args.root / "download-results.json"
    results_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    say(f"Completed: {len(completed)}/{len(selected)} | results: {results_path}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
