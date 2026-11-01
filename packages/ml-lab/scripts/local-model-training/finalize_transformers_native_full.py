#!/usr/bin/env python3
"""Finalize a native full-weight checkpoint without entering an adapter/Unsloth merge path."""
from __future__ import annotations

import argparse
import json
import os
import shutil
from pathlib import Path

from train_transformers_native_full import sha_file, sha_tree


EXCLUDED_TRAINING_STATE = {"optimizer.pt", "scheduler.pt", "trainer_state.json", "trainer-state.json"}


def finalize(selected_manifest: Path, output_dir: Path, family: str) -> Path:
    selected_manifest = selected_manifest.resolve(); output_dir = output_dir.resolve()
    payload = json.loads(selected_manifest.read_text("utf-8"))
    source = Path(payload.get("checkpoint_dir", "")).resolve()
    if payload.get("training_backend") != "transformers_native_full":
        raise RuntimeError("native finalizer requires a transformers_native_full checkpoint manifest")
    if not source.is_dir() or payload.get("checkpoint_dir_sha256") != sha_tree(source):
        raise RuntimeError("selected native checkpoint is missing or hash-mismatched")
    output_dir.mkdir(parents=True, exist_ok=True)
    merged = output_dir / "merged"; temporary = output_dir / f"merged.partial-{os.getpid()}"
    if merged.exists():
        raise RuntimeError(f"native merged destination already exists without a stage receipt: {merged}")
    if temporary.exists():
        shutil.rmtree(temporary)
    shutil.copytree(source, temporary, ignore=lambda _directory, names: [name for name in names if name in EXCLUDED_TRAINING_STATE])
    if not any(temporary.glob("*.safetensors")) and not (temporary / "pytorch_model.bin").is_file():
        raise RuntimeError("native finalizer did not find model weights")
    temporary.replace(merged)
    manifest = output_dir / "merge.manifest.json"
    manifest.write_text(json.dumps({
        "schema_version": "heardright.l1.action_manifest.v1", "action": "merge", "family": family,
        "training_backend": "transformers_native_full", "adapter_merge_required": False,
        "selected_checkpoint_manifest": str(selected_manifest), "selected_checkpoint_manifest_sha256": sha_file(selected_manifest),
        "selected_checkpoint_dir": str(source), "selected_checkpoint_dir_sha256": sha_tree(source),
        "merged_dir": str(merged), "merged_dir_sha256": sha_tree(merged),
    }, indent=2) + "\n", "utf-8")
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--family", required=True)
    parser.add_argument("--selected-checkpoint", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    if not args.live:
        raise SystemExit("native checkpoint finalization requires --live")
    manifest = finalize(args.selected_checkpoint, args.output_dir, args.family)
    print(json.dumps({"status": "ok", "artifact": str(manifest), "merged_dir": str(args.output_dir.resolve() / "merged")}))


if __name__ == "__main__":
    main()
