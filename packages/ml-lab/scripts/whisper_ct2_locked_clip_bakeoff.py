"""Whisper CT2 bakeoff against the locked Parakeet FP16-DML baseline.

This is a contained research harness. It does not modify production defaults.

Default matrix:
    large-v3-turbo, distil-large-v3, medium
    x float16, int8_float16

It runs the same four clips used by the Parakeet DML end-to-end bench and
compares transcript char similarity plus wall-clock decode time against
``_dml_e2e_bench_fp16_warmup_7shape_baseline.json``.
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import statistics
import sys
import time
import wave
from pathlib import Path
from typing import Any

import numpy as np


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_DIR = Path(__file__).resolve().parent
BASELINE = SCRIPT_DIR / "_dml_e2e_bench_fp16_warmup_7shape_baseline.json"
REPORT = SCRIPT_DIR / "_whisper_ct2_locked_clip_bakeoff.json"
CLIPS = [
    REPO_ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "long_paragraph_first2s.wav",
    REPO_ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_10s.wav",
    REPO_ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_60s.wav",
    REPO_ROOT / "tests" / "asr_bakeoff_2026_05_10" / "clips" / "yt_150s.wav",
]
SAMPLE_RATE = 16_000
_LIVE_MODELS: list[Any] = []


def sha256_short(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()[:32]


def load_pcm16(path: Path) -> tuple[np.ndarray, float]:
    with wave.open(str(path), "rb") as wav:
        if wav.getframerate() != SAMPLE_RATE:
            raise ValueError(f"{path} is not {SAMPLE_RATE} Hz")
        if wav.getnchannels() != 1:
            raise ValueError(f"{path} is not mono")
        if wav.getsampwidth() != 2:
            raise ValueError(f"{path} is not PCM16")
        frames = wav.getnframes()
        raw = wav.readframes(frames)
    return np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0, frames / SAMPLE_RATE


def normalize_text(text: str) -> str:
    return " ".join((text or "").lower().strip().split())


def char_similarity(reference: str, hypothesis: str) -> float:
    return difflib.SequenceMatcher(None, normalize_text(reference), normalize_text(hypothesis)).ratio()


def write_report(report: dict[str, Any], path: Path) -> None:
    path.write_text(json.dumps(report, indent=2), encoding="utf-8")


def transcribe_once(model, audio: np.ndarray, *, beam_size: int) -> str:
    segments_iter, _info = model.transcribe(
        audio,
        language="en",
        beam_size=beam_size,
        condition_on_previous_text=True,
        hallucination_silence_threshold=2.0,
        no_repeat_ngram_size=3,
        repetition_penalty=1.05,
        vad_filter=True,
        vad_parameters={
            "threshold": 0.35,
            "min_silence_duration_ms": 400,
            "speech_pad_ms": 600,
        },
    )
    return " ".join((segment.text or "").strip() for segment in segments_iter).strip()


def run_config(
    *,
    model_name: str,
    compute_type: str,
    device: str,
    beam_size: int,
    runs: int,
    clips: list[dict[str, Any]],
    baseline_by_clip: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    from faster_whisper import WhisperModel

    print(f"\nLoading WhisperModel({model_name}, device={device}, compute_type={compute_type})", flush=True)
    t0 = time.perf_counter()
    model = WhisperModel(model_name, device=device, compute_type=compute_type)
    _LIVE_MODELS.append(model)
    load_s = time.perf_counter() - t0
    print(f"  loaded in {load_s:.2f}s", flush=True)

    try:
        # Spin up CUDA/CT2 kernels outside measured clip timings.
        _ = transcribe_once(model, np.zeros(int(0.5 * SAMPLE_RATE), dtype=np.float32), beam_size=1)
    except Exception as exc:
        print(f"  warmup skipped/failed: {exc}", flush=True)

    per_clip: list[dict[str, Any]] = []
    for clip in clips:
        audio = clip["audio"]
        times: list[float] = []
        transcripts: list[str] = []
        for _ in range(runs):
            t1 = time.perf_counter()
            text = transcribe_once(model, audio, beam_size=beam_size)
            times.append(time.perf_counter() - t1)
            transcripts.append(text)

        baseline_item = baseline_by_clip[clip["name"]]
        median = statistics.median(times)
        sim = char_similarity(baseline_item["transcript"], transcripts[0])
        per_clip.append(
            {
                "clip": clip["name"],
                "clip_sha256": clip["sha256"],
                "duration_s": clip["duration_s"],
                "decode_times_s": [round(t, 4) for t in times],
                "decode_median_s": round(median, 4),
                "rtf": round(median / clip["duration_s"], 4),
                "baseline_parakeet_decode_median_s": baseline_item["decode_median_s"],
                "latency_ratio_vs_parakeet": round(median / baseline_item["decode_median_s"], 4),
                "transcript": transcripts[0],
                "baseline_parakeet_transcript": baseline_item["transcript"],
                "char_similarity_vs_parakeet": round(sim, 6),
                "transcripts_agree": len(set(transcripts)) == 1,
                "non_empty": bool(transcripts[0].strip()),
            }
        )
        print(
            f"  {clip['name']:<35} med={median:.3f}s "
            f"rtf={median / clip['duration_s']:.3f} "
            f"sim={sim:.4f}",
            flush=True,
        )

    return {
        "model_name": model_name,
        "compute_type": compute_type,
        "device": device,
        "beam_size": beam_size,
        "runs_per_clip": runs,
        "load_s": round(load_s, 4),
        "per_clip": per_clip,
        "acceptance": {
            "all_non_empty": all(item["non_empty"] for item in per_clip),
            "all_transcripts_agree": all(item["transcripts_agree"] for item in per_clip),
            "all_similarity_gte_0_99_vs_parakeet": all(
                item["char_similarity_vs_parakeet"] >= 0.99 for item in per_clip
            ),
            "beats_parakeet_majority_latency": (
                sum(item["latency_ratio_vs_parakeet"] < 1.0 for item in per_clip) >= 3
            ),
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", default="large-v3-turbo,distil-large-v3,medium")
    parser.add_argument("--compute-types", default="float16,int8_float16")
    parser.add_argument("--device", default="cuda")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--beam-size", type=int, default=5)
    parser.add_argument("--report", type=Path, default=REPORT)
    parser.add_argument("--hard-exit-after-write", action="store_true")
    args = parser.parse_args()

    if not BASELINE.exists():
        raise SystemExit(f"missing locked baseline: {BASELINE}")

    try:
        import ctranslate2
        import faster_whisper
    except ImportError as exc:
        raise SystemExit(f"missing faster-whisper/CTranslate2 in {sys.executable}: {exc}") from exc

    models = [m.strip() for m in args.models.split(",") if m.strip()]
    compute_types = [c.strip() for c in args.compute_types.split(",") if c.strip()]
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    baseline_by_clip = {item["clip"]: item for item in baseline["per_clip"]}
    clips = []
    for path in CLIPS:
        audio, duration_s = load_pcm16(path)
        clips.append(
            {
                "name": path.name,
                "path": str(path),
                "audio": audio,
                "duration_s": round(duration_s, 3),
                "sha256": sha256_short(path),
            }
        )

    report: dict[str, Any] = {
        "created_at_unix": time.time(),
        "python": sys.executable,
        "faster_whisper_version": getattr(faster_whisper, "__version__", "unknown"),
        "ctranslate2_version": ctranslate2.__version__,
        "ct2_cuda_device_count": ctranslate2.get_cuda_device_count(),
        "baseline": {
            "path": str(BASELINE),
            "mode": baseline.get("mode"),
            "backend_name": baseline.get("backend_name"),
            "cold_load_s": baseline.get("cold_load_s"),
        },
        "matrix": {
            "models": models,
            "compute_types": compute_types,
            "device": args.device,
            "runs_per_clip": args.runs,
            "beam_size": args.beam_size,
        },
        "clips": [{k: v for k, v in clip.items() if k != "audio"} for clip in clips],
        "results": [],
    }
    write_report(report, args.report)

    if args.device == "cuda" and ctranslate2.get_cuda_device_count() < 1:
        raise SystemExit("requested cuda but CTranslate2 reports zero CUDA devices")

    for model_name in models:
        for compute_type in compute_types:
            try:
                result = run_config(
                    model_name=model_name,
                    compute_type=compute_type,
                    device=args.device,
                    beam_size=args.beam_size,
                    runs=args.runs,
                    clips=clips,
                    baseline_by_clip=baseline_by_clip,
                )
            except Exception as exc:
                result = {
                    "model_name": model_name,
                    "compute_type": compute_type,
                    "device": args.device,
                    "status": "failed",
                    "error": repr(exc),
                }
                print(f"FAILED {model_name} {compute_type}: {exc}", flush=True)
            report["results"].append(result)
            write_report(report, args.report)

    print(f"\nWrote {args.report}", flush=True)
    if args.hard_exit_after_write:
        sys.stdout.flush()
        sys.stderr.flush()
        os._exit(0)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
