#!/usr/bin/env python3
"""Validate and profile the NeMo-compatible rolling mel path on CPU or DirectML."""
from __future__ import annotations

import argparse
import hashlib
import json
import time
import wave
from pathlib import Path

import numpy as np
import onnxruntime as ort

from cache_aware_tdt_bench import (
    DurableLedger,
    finalize_nemo_raw_mel,
    provider_chain,
    sha256_file,
    validate_candidate_manifest,
)
from prepare_cache_aware_features import save_npy_atomic


SAMPLE_RATE = 16_000
HOP_SAMPLES = 160
HALF_FFT_SAMPLES = 256
SHIFT_SAMPLES = 32_000
NONFINAL_LOOKAHEAD_SAMPLES = HALF_FFT_SAMPLES - HOP_SAMPLES


def mel_streaming_plan(sample_count: int) -> list[dict[str, int | bool]]:
    """Plan exact rolling mel slices with bounded overlap and a Stop flush."""
    if sample_count <= 0:
        raise ValueError("sample_count must be positive")
    observations = list(
        range(
            SHIFT_SAMPLES + NONFINAL_LOOKAHEAD_SAMPLES,
            sample_count,
            SHIFT_SAMPLES,
        )
    )
    observations.append(sample_count)
    committed = 0
    plan: list[dict[str, int | bool]] = []
    for observed in observations:
        final = observed == sample_count
        if final:
            feature_end = observed // HOP_SAMPLES
        else:
            feature_end = max(
                0,
                (observed - HALF_FFT_SAMPLES) // HOP_SAMPLES + 1,
            )
        sample_start = max(0, (committed - 2) * HOP_SAMPLES)
        local_feature_start = committed - sample_start // HOP_SAMPLES
        plan.append(
            {
                "observed_samples": observed,
                "final": final,
                "feature_start": committed,
                "feature_end": feature_end,
                "sample_start": sample_start,
                "local_feature_start": local_feature_start,
            }
        )
        committed = feature_end
    return plan


def read_pcm16_mono(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as handle:
        if (
            handle.getframerate() != SAMPLE_RATE
            or handle.getnchannels() != 1
            or handle.getsampwidth() != 2
            or handle.getcomptype() != "NONE"
        ):
            raise ValueError(f"expected uncompressed 16 kHz mono PCM16 WAV: {path}")
        payload = handle.readframes(handle.getnframes())
    return np.frombuffer(payload, dtype="<i2").astype(np.float32) / np.float32(32768.0)


def run_rolling_mel(
    session: ort.InferenceSession,
    audio: np.ndarray,
) -> tuple[np.ndarray, list[dict[str, int | bool | float]]]:
    pieces: list[np.ndarray] = []
    timings: list[dict[str, int | bool | float]] = []
    for row in mel_streaming_plan(int(audio.size)):
        sample_start = int(row["sample_start"])
        observed = int(row["observed_samples"])
        local_start = int(row["local_feature_start"])
        count = int(row["feature_end"]) - int(row["feature_start"])
        segment = audio[sample_start:observed][None, :]
        started = time.perf_counter()
        raw = session.run(None, {"audio": segment})[0]
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        local = raw.transpose(0, 2, 1).astype(np.float32, copy=False)
        piece = local[:, :, local_start : local_start + count]
        if piece.shape[2] != count:
            raise RuntimeError("rolling mel graph returned too few stable frames")
        pieces.append(piece.copy())
        timings.append(
            {
                **row,
                "segment_samples": int(segment.shape[1]),
                "mel_ms": elapsed_ms,
            }
        )
    valid = np.concatenate(pieces, axis=2)
    masked_tail = np.zeros((1, valid.shape[1], 1), dtype=np.float32)
    return np.concatenate([valid, masked_tail], axis=2), timings


def error_metrics(actual: np.ndarray, reference: np.ndarray) -> dict[str, float | int]:
    if actual.shape != reference.shape:
        raise ValueError(f"feature shape mismatch: {actual.shape} != {reference.shape}")
    delta = np.abs(actual.astype(np.float32) - reference.astype(np.float32))
    return {
        "max_abs": float(delta.max(initial=0.0)),
        "mean_abs": float(delta.mean()),
        "count_over_2e_4": int(np.count_nonzero(delta > np.float32(2e-4))),
        "count_over_1e_3": int(np.count_nonzero(delta > np.float32(1e-3))),
    }


def save_derived_feature(path: Path, features: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    save_npy_atomic(path, features)


def _digest(value: object) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--audio-root", required=True, type=Path)
    parser.add_argument("--features-root", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--provider", required=True, choices=("cpu", "dml", "cuda"))
    parser.add_argument("--clip", action="append")
    parser.add_argument("--save-features-root", type=Path)
    args = parser.parse_args()

    rows = json.loads(args.manifest.read_text(encoding="utf-8"))
    validate_candidate_manifest(rows)
    if args.clip:
        wanted = set(args.clip)
        rows = [row for row in rows if row["id"] in wanted]
        missing = wanted - {str(row["id"]) for row in rows}
        if missing:
            raise ValueError(f"unknown clip id(s): {sorted(missing)}")
    providers = provider_chain(args.provider)
    options = ort.SessionOptions()
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.enable_mem_pattern = args.provider != "dml"
    options.intra_op_num_threads = 4
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(str(args.model), sess_options=options, providers=providers)
    if session.get_providers()[0] != providers[0]:
        raise RuntimeError(
            f"mel provider fell back: requested={providers[0]} active={session.get_providers()}"
        )
    started = time.perf_counter()
    session.run(None, {"audio": np.zeros((1, SHIFT_SAMPLES), dtype=np.float32)})
    warmup_ms = (time.perf_counter() - started) * 1000.0

    model_hashes = {"onnx": sha256_file(args.model)}
    sidecar = args.model.with_name(args.model.name + ".data")
    if sidecar.exists():
        model_hashes["data"] = sha256_file(sidecar)
    runner_hash = sha256_file(Path(__file__).resolve())
    ledger = DurableLedger(args.out)
    failures = 0
    for index, item in enumerate(rows, start=1):
        clip_id = str(item["id"])
        audio_path = args.audio_root / f"{clip_id}.wav"
        reference_path = args.features_root / f"{clip_id}.npy"
        audio_hash = sha256_file(audio_path)
        if audio_hash != item["sha256"]:
            raise ValueError(f"audio hash mismatch: {clip_id}")
        binding = {
            "clip_id": clip_id,
            "provider": args.provider,
            "audio_sha256": audio_hash,
            "reference_sha256": sha256_file(reference_path),
            "model_sha256s": model_hashes,
            "runner_sha256": runner_hash,
        }
        resume_key = _digest(binding)
        if resume_key in ledger.rows:
            print(f"[{index}/{len(rows)}] {clip_id} resume=skip", flush=True)
            continue
        try:
            read_started = time.perf_counter()
            audio = read_pcm16_mono(audio_path)
            wav_read_ms = (time.perf_counter() - read_started) * 1000.0
            reference = np.load(reference_path, allow_pickle=False)

            full_started = time.perf_counter()
            full = finalize_nemo_raw_mel(session.run(None, {"audio": audio[None, :]})[0])
            full_mel_ms = (time.perf_counter() - full_started) * 1000.0
            rolling, timings = run_rolling_mel(session, audio)
            full_error = error_metrics(full, reference)
            rolling_error = error_metrics(rolling, reference)
            if full_error["max_abs"] > 1e-3 or rolling_error["max_abs"] > 1e-3:
                raise RuntimeError(
                    f"mel parity failed: full={full_error['max_abs']} rolling={rolling_error['max_abs']}"
                )
            if args.save_features_root is not None:
                save_derived_feature(args.save_features_root / f"{clip_id}.npy", rolling)
            row = {
                **binding,
                "resume_key": resume_key,
                "status": "ok",
                "duration_s": float(item["duration_s"]),
                "provider_actual": session.get_providers(),
                "warmup_ms": warmup_ms,
                "wav_read_ms": wav_read_ms,
                "full_mel_ms": full_mel_ms,
                "rolling_mel_total_ms": float(sum(float(x["mel_ms"]) for x in timings)),
                "rolling_mel_max_step_ms": float(max(float(x["mel_ms"]) for x in timings)),
                "stop_mel_ms": float(timings[-1]["mel_ms"]),
                "steps": len(timings),
                "max_segment_samples": max(int(x["segment_samples"]) for x in timings),
                "processed_sample_ratio": float(
                    sum(int(x["segment_samples"]) for x in timings) / audio.size
                ),
                "feature_shape": list(rolling.shape),
                "full_error": full_error,
                "rolling_error": rolling_error,
                "timings": timings,
                "error": None,
            }
        except Exception as exc:
            failures += 1
            row = {
                **binding,
                "resume_key": resume_key,
                "status": "error",
                "error": f"{type(exc).__name__}: {exc}",
            }
        ledger.append(row)
        print(
            f"[{index}/{len(rows)}] {clip_id} status={row['status']} "
            f"full_ms={float(row.get('full_mel_ms', 0)):.2f} "
            f"rolling_ms={float(row.get('rolling_mel_total_ms', 0)):.2f} "
            f"max_abs={float(row.get('rolling_error', {}).get('max_abs', 0)):.6g}",
            flush=True,
        )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
