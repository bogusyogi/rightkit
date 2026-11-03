#!/usr/bin/env python3
"""Stress explicit cache state, reset behavior, and process memory."""
from __future__ import annotations

import argparse
import ctypes
import gc
import hashlib
import json
import os
import time
from pathlib import Path

import numpy as np

from cache_aware_tdt_bench import CacheAwareTdtRuntime, sha256_file


def build_stress_features(
    sources: list[np.ndarray], *, target_frames: int, silence_frames: int
) -> np.ndarray:
    if not sources or target_frames <= 0 or silence_frames < 0:
        raise ValueError("invalid stress feature request")
    shape = sources[0].shape[:2]
    if shape[0] != 1 or any(item.ndim != 3 or item.shape[:2] != shape for item in sources):
        raise ValueError("all sources must have shape [1, feature_bins, frames]")
    silence = np.zeros((*shape, silence_frames), dtype=np.float32)
    cycle: list[np.ndarray] = []
    for source in sources:
        cycle.extend([source.astype(np.float32, copy=False), silence])
    cycle_frames = sum(item.shape[2] for item in cycle)
    repeats = (target_frames + cycle_frames - 1) // cycle_frames
    return np.concatenate(cycle * repeats, axis=2)[:, :, :target_frames].copy()


def process_rss_bytes() -> int:
    if os.name != "nt":
        import resource

        return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024)

    class ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.c_ulong),
            ("PageFaultCount", ctypes.c_ulong),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    counters = ProcessMemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel32.GetCurrentProcess.restype = ctypes.c_void_p
    psapi.GetProcessMemoryInfo.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ProcessMemoryCounters),
        ctypes.c_ulong,
    ]
    psapi.GetProcessMemoryInfo.restype = ctypes.c_int
    handle = kernel32.GetCurrentProcess()
    if not psapi.GetProcessMemoryInfo(
        handle, ctypes.byref(counters), counters.cb
    ):
        raise OSError("GetProcessMemoryInfo failed")
    return int(counters.WorkingSetSize)


def token_sha256(tokens: list[int]) -> str:
    body = json.dumps(tokens, separators=(",", ":"))
    return hashlib.sha256(body.encode("ascii")).hexdigest()


def write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8", newline="\n") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--feature", required=True, action="append", type=Path)
    parser.add_argument("--short-feature", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--encoder-provider", choices=("dml", "cpu", "cuda"), default="dml")
    parser.add_argument("--decoder-provider", choices=("dml", "cpu", "cuda"), default="dml")
    parser.add_argument("--minutes", action="append", type=int, default=[])
    parser.add_argument("--include-tokens", action="store_true")
    args = parser.parse_args()
    minutes = args.minutes or [10, 30]

    rss_before_load = process_rss_bytes()
    load_started = time.perf_counter()
    runtime = CacheAwareTdtRuntime(
        args.model_dir, args.encoder_provider, args.decoder_provider
    )
    load_ms = (time.perf_counter() - load_started) * 1000.0
    rss_after_load = process_rss_bytes()
    warmup_ms = runtime.warmup()
    rss_after_warmup = process_rss_bytes()

    short = np.load(args.short_feature, allow_pickle=False)
    short_duration_s = max(0.0, (short.shape[2] - 1) * 0.01)
    isolated = runtime.transcribe_features(short, short_duration_s)
    isolated_sha = token_sha256(isolated["tokens"])

    sources = [np.load(path, allow_pickle=False) for path in args.feature]
    stress_rows: list[dict[str, object]] = []
    for target_minutes in minutes:
        target_frames = target_minutes * 60 * 100 + 1
        rss_before_build = process_rss_bytes()
        features = build_stress_features(
            sources, target_frames=target_frames, silence_frames=50
        )
        rss_after_build = process_rss_bytes()
        started = time.perf_counter()
        result = runtime.transcribe_features(features, target_minutes * 60.0)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        rss_after_decode = process_rss_bytes()
        stress_row: dict[str, object] = {
                "minutes": target_minutes,
                "feature_frames": target_frames,
                "steps": len(result["chunk_ms"]),
                "token_count": len(result["tokens"]),
                "token_sha256": token_sha256(result["tokens"]),
                "elapsed_ms": elapsed_ms,
                "encoder_ms": result["encoder_total_ms"],
                "decoder_ms": result["decoder_ms"],
                "max_step_ms": max(result["chunk_ms"], default=0.0),
                "simulated_stop_tail_ms": result["simulated_stop_tail_ms"],
                "rss_before_build": rss_before_build,
                "rss_after_build": rss_after_build,
                "rss_after_decode": rss_after_decode,
            }
        if args.include_tokens:
            stress_row["tokens"] = result["tokens"]
        stress_rows.append(stress_row)
        del features, result
        gc.collect()
        stress_rows[-1]["rss_after_gc"] = process_rss_bytes()

    after_stress = runtime.transcribe_features(short, short_duration_s)
    after_sha = token_sha256(after_stress["tokens"])
    reset_exact = isolated["tokens"] == after_stress["tokens"]
    receipt = {
        "status": "ok" if reset_exact else "error",
        "model_component_sha256s": {
            name: sha256_file(args.model_dir / name)
            for name in (
                "encoder-model.onnx",
                "decoder_joint-model.onnx",
                "vocab.txt",
                "config.json",
            )
        },
        "encoder_provider": runtime.encoder.get_providers(),
        "decoder_provider": runtime.decoder.get_providers(),
        "load_ms": load_ms,
        "warmup_ms": warmup_ms,
        "rss_before_load": rss_before_load,
        "rss_after_load": rss_after_load,
        "rss_after_warmup": rss_after_warmup,
        "cache_tensor_bytes": 12_681_224,
        "isolated_short_token_sha256": isolated_sha,
        "after_stress_short_token_sha256": after_sha,
        "reset_tokens_exact": reset_exact,
        "stress": stress_rows,
    }
    write_json_atomic(args.out, receipt)
    print(
        f"status={receipt['status']} reset_exact={reset_exact} "
        f"load_ms={load_ms:.1f} warmup_ms={warmup_ms:.1f}",
        flush=True,
    )
    for row in stress_rows:
        print(
            f"{row['minutes']}m steps={row['steps']} elapsed_ms={row['elapsed_ms']:.1f} "
            f"rss_after_gc={row['rss_after_gc']}",
            flush=True,
        )
    return 0 if reset_exact else 1


if __name__ == "__main__":
    raise SystemExit(main())
