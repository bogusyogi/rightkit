"""Probe parakeet-unified-en-0.6b ONNX as a Heard Right CPU+DML hybrid.

This is intentionally background-friendly:
- prints one line per variant;
- writes JSON after every variant;
- downloads into .asr_candidates instead of the user HF cache.

The preferred shipping shape is encoder on DML and decoder/joint on CPU,
ideally mixed/FP16 encoder plus INT8 decoder. This script also tests the
nearby combinations so the final choice is evidence-led.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable

from unified_parakeet_variants import Variant, variants

import numpy as np
from scipy.io import wavfile

ROOT = Path("D:/Claude/heardright")
CAND = ROOT / ".asr_candidates" / "parakeet-unified-en-0.6b-onnx"
OUT = ROOT / "tauri-app-next" / "scripts" / "_unified_parakeet_hybrid_matrix.json"
REPO = "eschmidbauer/parakeet-unified-en-0.6b-onnx"
SR = 16_000
BLANK = 1024
DOWNLOAD_FILES = [
    "README.md",
    "onnx_export.py",
    "requirements.txt",
    "onnx_fp32/encoder.onnx",
    "onnx_fp32/encoder.onnx.data",
    "onnx_fp32/decoder_joint.onnx",
    "onnx_fp32/decoder_joint.onnx.data",
    "onnx_fp32/tokenizer.model",
    "onnx_int8/encoder.int8.onnx",
    "onnx_int8/decoder_joint.int8.onnx",
    "onnx_int8/tokenizer.model",
]


@dataclass
class Result:
    variant: str
    clip: str
    duration_s: float
    status: str
    latency_s: float | None = None
    text: str = ""
    error: str = ""
    encoder_providers: list[str] | None = None
    decoder_providers: list[str] | None = None
    disk_mb: float | None = None


def _ensure_sentencepiece():
    try:
        import sentencepiece as spm  # type: ignore

        return spm
    except ImportError:
        fallback = ROOT / ".venv-fp16-build" / "Lib" / "site-packages"
        if fallback.exists():
            sys.path.insert(0, str(fallback))
        import sentencepiece as spm  # type: ignore

        return spm


def _provider(name: str) -> list[str]:
    if name == "dml":
        return ["DmlExecutionProvider", "CPUExecutionProvider"]
    if name == "cpu":
        return ["CPUExecutionProvider"]
    raise ValueError(f"unknown provider {name!r}")


def _session_options(provider: str, static_window: int = 0):
    import onnxruntime as rt

    so = rt.SessionOptions()
    so.graph_optimization_level = rt.GraphOptimizationLevel.ORT_ENABLE_ALL
    so.execution_mode = rt.ExecutionMode.ORT_SEQUENTIAL
    so.enable_mem_pattern = provider == "cpu"
    if provider == "cpu":
        so.intra_op_num_threads = 4
        so.inter_op_num_threads = 1
    else:
        so.intra_op_num_threads = 4
        so.inter_op_num_threads = 2
        if static_window:
            so.add_session_config_entry("ep.dml.disable_graph_fusion", "1")
    return so


def download(which: str) -> None:
    from huggingface_hub import snapshot_download

    if which == "int8":
        allow = [f for f in DOWNLOAD_FILES if f.startswith("onnx_int8/") or "/" not in f]
    elif which == "fp32":
        allow = [f for f in DOWNLOAD_FILES if f.startswith("onnx_fp32/") or "/" not in f]
    elif which == "all":
        allow = DOWNLOAD_FILES
    else:
        raise ValueError("--download must be int8, fp32, or all")

    CAND.mkdir(parents=True, exist_ok=True)
    print(f"downloading {which} from {REPO} -> {CAND}", flush=True)
    snapshot_download(REPO, local_dir=CAND, allow_patterns=allow)
    print("download complete", flush=True)


def load_clip(path: Path) -> np.ndarray:
    sr, data = wavfile.read(str(path))
    if sr != SR:
        raise ValueError(f"{path} sample rate {sr}, expected {SR}")
    if data.dtype == np.int16:
        return data.astype(np.float32) / 32768.0
    return data.astype(np.float32)


def preprocess(audio: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    from onnx_asr.preprocessors.numpy_preprocessor import NemoPreprocessorNumpy

    pre = NemoPreprocessorNumpy("nemo128")
    wave = audio.astype(np.float32, copy=False)[None, :]
    lens = np.array([audio.shape[0]], dtype=np.int64)
    return pre(wave, lens)


def model_disk_mb(paths: Iterable[Path]) -> float:
    seen: set[Path] = set()
    total = 0
    for path in paths:
        p = path.resolve()
        if p in seen:
            continue
        seen.add(p)
        if p.exists():
            total += p.stat().st_size
        data = Path(str(p) + ".data")
        if data.exists() and data.resolve() not in seen:
            seen.add(data.resolve())
            total += data.stat().st_size
    return round(total / 1_000_000, 1)


def decode_tokens(dec, enc_out: np.ndarray, enc_len: np.ndarray, max_tokens: int) -> list[int]:
    state1 = np.zeros((2, 1, 640), dtype=np.float32)
    state2 = np.zeros((2, 1, 640), dtype=np.float32)
    last_token = BLANK
    tokens: list[int] = []
    steps = min(int(enc_len[0]), int(enc_out.shape[2]))

    for t in range(steps):
        enc_t = enc_out[:, :, t : t + 1].astype(np.float32, copy=False)
        emitted = 0
        while emitted < max_tokens:
            outputs = dec.run(
                ["outputs", "prednet_lengths", "output_states_1", "output_states_2"],
                {
                    "encoder_outputs": enc_t,
                    "targets": np.array([[last_token]], dtype=np.int32),
                    "target_length": np.array([1], dtype=np.int32),
                    "input_states_1": state1,
                    "input_states_2": state2,
                },
            )
            logits, _, next_state1, next_state2 = outputs
            idx = int(np.argmax(logits[0, 0, 0]))
            if idx == BLANK:
                break
            tokens.append(idx)
            last_token = idx
            state1, state2 = next_state1, next_state2
            emitted += 1

    return tokens


def _static_chunks(feats: np.ndarray, feat_lens: np.ndarray, window: int):
    total = min(int(feat_lens[0]), int(feats.shape[2]))
    for start in range(0, max(total, 1), window):
        end = min(start + window, total)
        real = max(0, end - start)
        chunk = feats[:, :, start:end]
        if real < window:
            chunk = np.pad(chunk, ((0, 0), (0, 0), (0, window - real)), mode="constant")
        yield chunk.astype(feats.dtype, copy=False), np.array([real], dtype=np.int64)


def transcribe(variant: Variant, audio: np.ndarray) -> tuple[str, float, list[str], list[str]]:
    import onnxruntime as rt

    spm = _ensure_sentencepiece()
    feats, feat_lens = preprocess(audio)

    enc = rt.InferenceSession(
        str(variant.encoder),
        sess_options=_session_options(variant.encoder_provider, variant.static_window),
        providers=_provider(variant.encoder_provider),
    )
    dec = rt.InferenceSession(
        str(variant.decoder),
        sess_options=_session_options(variant.decoder_provider),
        providers=_provider(variant.decoder_provider),
    )
    sp = spm.SentencePieceProcessor(model_file=str(variant.tokenizer))

    t0 = time.perf_counter()
    texts: list[str] = []
    chunks = _static_chunks(feats, feat_lens, variant.static_window) if variant.static_window else [(feats, feat_lens)]
    for chunk_feats, chunk_lens in chunks:
        enc_out, enc_len = enc.run(
            ["outputs", "encoded_lengths"],
            {"audio_signal": chunk_feats, "length": chunk_lens},
        )
        if not np.isfinite(enc_out).all():
            raise ValueError("encoder output contains NaN/Inf")
        tokens = decode_tokens(dec, enc_out, enc_len, variant.max_tokens)
        text = sp.decode(tokens).strip()
        if text:
            texts.append(text)

    text = " ".join(texts).strip()
    return text, time.perf_counter() - t0, enc.get_providers(), dec.get_providers()


def smoke(runs: int, include_mixed: bool, clip_paths: list[Path] | None = None) -> list[Result]:
    clips = clip_paths or [
        ROOT / "tests" / "asr_global_eval" / "clips" / "libri_short_1272_1272-128104-0000.wav",
        ROOT / "tests" / "asr_global_eval" / "clips" / "libri_med_2086_149214.wav",
    ]
    results: list[Result] = []
    OUT.parent.mkdir(parents=True, exist_ok=True)

    for variant in variants(CAND, include_mixed=include_mixed):
        disk = model_disk_mb((variant.encoder, variant.decoder, variant.tokenizer))
        missing = [str(p) for p in (variant.encoder, variant.decoder, variant.tokenizer) if not p.exists()]
        if missing:
            for clip in clips:
                results.append(
                    Result(
                        variant=variant.name,
                        clip=clip.name,
                        duration_s=0.0,
                        status="missing",
                        error="; ".join(missing),
                        disk_mb=disk,
                    )
                )
            continue

        for clip in clips:
            audio = load_clip(clip)
            latencies: list[float] = []
            text = ""
            enc_providers: list[str] | None = None
            dec_providers: list[str] | None = None
            status = "ok"
            error = ""
            try:
                for _ in range(runs):
                    text, latency, enc_providers, dec_providers = transcribe(variant, audio)
                    latencies.append(latency)
            except Exception as exc:
                status = "error"
                error = f"{type(exc).__name__}: {exc}"

            result = Result(
                variant=variant.name,
                clip=clip.name,
                duration_s=round(audio.shape[0] / SR, 3),
                status=status,
                latency_s=round(statistics.median(latencies), 4) if latencies else None,
                text=text[:220],
                error=error,
                encoder_providers=enc_providers,
                decoder_providers=dec_providers,
                disk_mb=disk,
            )
            results.append(result)
            OUT.write_text(json.dumps([asdict(r) for r in results], indent=2), encoding="utf-8")
            msg = f"{result.status:>7} {result.variant:<42} {clip.name:<28}"
            if result.latency_s is not None:
                msg += f" {result.latency_s:.3f}s"
            if result.error:
                msg += f" {result.error[:120]}"
            print(msg, flush=True)

    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--download", choices=("int8", "fp32", "all"))
    ap.add_argument("--mode", choices=("smoke",), default="smoke")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--include-mixed", action="store_true")
    ap.add_argument("--clips", nargs="*", type=Path)
    args = ap.parse_args()

    if args.download:
        download(args.download)
        return 0

    smoke(runs=args.runs, include_mixed=args.include_mixed, clip_paths=args.clips or None)
    print(f"wrote {OUT}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
