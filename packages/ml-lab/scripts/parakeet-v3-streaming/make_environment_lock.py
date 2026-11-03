#!/usr/bin/env python3
"""Emit the frozen environment receipt for the Parakeet v3 streaming-shape run.

Writes environment-lock.txt plus environment-lock.sha256. The receipt is the
run's dependency identity: once Stage A has loaded the model, a dependency change
means a new run id, so this must be captured before the first load.

Usage:
  python make_environment_lock.py --out <dir> [--repo-commit <sha>]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path


def run(cmd):
    try:
        return subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=120
        ).stdout.strip()
    except Exception as e:  # pragma: no cover
        return f"<error: {type(e).__name__}: {e}>"


def mod_version(name):
    try:
        m = __import__(name)
        return getattr(m, "__version__", "<no __version__>")
    except Exception as e:
        return f"<absent: {type(e).__name__}>"


def onnxruntime_provider_line(runtime) -> str:
    providers = ",".join(runtime.get_available_providers())
    return f"onnxruntime_providers: {providers}"


def write_environment_lock(out: Path, body: str) -> tuple[Path, str]:
    out.mkdir(parents=True, exist_ok=True)
    lock_path = out / "environment-lock.txt"
    with lock_path.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write(body)
        fh.flush()
        os.fsync(fh.fileno())
    digest = hashlib.sha256(lock_path.read_bytes()).hexdigest()
    sha_path = out / "environment-lock.sha256"
    with sha_path.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write(f"{digest}  environment-lock.txt\n")
        fh.flush()
        os.fsync(fh.fileno())
    return lock_path, digest


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--repo-commit", default=os.environ.get("RIGHTKIT_ML_REPO_COMMIT", ""))
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    lines = []
    add = lines.append

    add("# Parakeet v3 streaming-shape export — environment lock")
    add(f"python_executable: {sys.executable}")
    add(f"python_version: {sys.version.split()[0]}")
    add(f"platform: {platform.platform()}")
    add(f"machine: {platform.machine()}")
    add(f"repo_commit: {args.repo_commit or '<unset>'}")

    try:
        import torch

        add(f"torch: {torch.__version__}")
        add(f"torch_cuda_built: {torch.version.cuda}")
        add(f"torch_cudnn: {torch.backends.cudnn.version()}")
        add(f"torch_cuda_available: {torch.cuda.is_available()}")
        if torch.cuda.is_available():
            add(f"gpu_name: {torch.cuda.get_device_name(0)}")
            free, total = torch.cuda.mem_get_info()
            add(f"gpu_vram_free_bytes: {free}")
            add(f"gpu_vram_total_bytes: {total}")
    except Exception as e:
        add(f"torch: <absent: {type(e).__name__}: {e}>")

    for name in ("nemo", "coremltools", "onnx", "onnxruntime", "numpy", "lhotse"):
        add(f"{name}: {mod_version(name)}")
    try:
        import onnxruntime

        add(onnxruntime_provider_line(onnxruntime))
    except Exception as e:
        add(f"onnxruntime_providers: <absent: {type(e).__name__}>")

    add(f"ffmpeg: {run('ffmpeg -version 2>/dev/null | head -1') or '<absent>'}")
    add(f"libsndfile: {run('ldconfig -p 2>/dev/null | grep -c libsndfile')}")
    add(f"nvidia_driver: {run('nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1')}")

    total, used, free = shutil.disk_usage(os.path.expanduser("~"))
    add(f"disk_free_bytes: {free}")
    add(f"disk_total_bytes: {total}")

    freeze = run(f"{sys.executable} -m pip freeze --all")
    add(f"pip_freeze_sha256: {hashlib.sha256(freeze.encode()).hexdigest()}")
    add(f"pip_freeze_lines: {len(freeze.splitlines())}")
    add("")
    add("# --- pip freeze --all ---")
    lines.extend(freeze.splitlines())

    body = "\n".join(lines) + "\n"
    lock_path, digest = write_environment_lock(Path(args.out), body)

    print(
        json.dumps(
            {"lock": str(lock_path), "sha256": digest, "bytes": lock_path.stat().st_size},
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
