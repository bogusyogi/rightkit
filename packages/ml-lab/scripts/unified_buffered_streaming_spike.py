from __future__ import annotations

import argparse
import json
import re
import sys
import time
import wave
from dataclasses import asdict, dataclass
from pathlib import Path
from statistics import median

import numpy as np


REPO = Path(__file__).resolve().parents[1]
HEARDRIGHT = REPO.parent
APP = HEARDRIGHT / "app"
if str(APP) not in sys.path:
    sys.path.insert(0, str(APP))

from heardright.asr_runtime.backends.parakeet_unified_ort import (  # noqa: E402
    ParakeetUnifiedORTBackend,
    SAMPLE_RATE,
)


DEFAULT_WAV = (
    HEARDRIGHT
    / "tests"
    / "v2_acceptance"
    / "validation_audio"
    / "jfk_us_accent.wav"
)
DEFAULT_ARTIFACT = Path(__file__).with_name("_unified_buffered_streaming_spike.json")


@dataclass(frozen=True)
class BufferedStreamingConfig:
    chunk_s: float
    left_context_s: float
    right_context_s: float
    mode: str
    encoder_provider: str
    decoder_provider: str
    max_tokens_per_step: int


def _read_wav_mono_16k(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wf:
        channels = wf.getnchannels()
        sample_rate = wf.getframerate()
        sample_width = wf.getsampwidth()
        frames = wf.readframes(wf.getnframes())

    if sample_rate != SAMPLE_RATE:
        raise ValueError(f"{path} is {sample_rate} Hz, expected {SAMPLE_RATE} Hz")
    if sample_width != 2:
        raise ValueError(f"{path} is {sample_width * 8}-bit, expected 16-bit PCM")

    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    if channels > 1:
        audio = audio.reshape(-1, channels).mean(axis=1)
    return audio.astype(np.float32, copy=False)


def _decode_encoder_frames(
    backend: ParakeetUnifiedORTBackend,
    enc_out: np.ndarray,
    *,
    start_frame: int,
    end_frame: int,
    state1: np.ndarray,
    state2: np.ndarray,
    last_token: int,
) -> tuple[str, list[int], np.ndarray, np.ndarray, int]:
    tokens: list[int] = []
    steps = min(int(end_frame), int(enc_out.shape[2]))
    for frame_idx in range(max(0, start_frame), steps):
        enc_t = enc_out[:, :, frame_idx : frame_idx + 1].astype(np.float32, copy=False)
        emitted = 0
        while emitted < backend.max_tokens_per_step:
            logits, _, next_state1, next_state2 = backend._decoder_joint.run(
                ["outputs", "prednet_lengths", "output_states_1", "output_states_2"],
                {
                    "encoder_outputs": enc_t,
                    "targets": np.array([[last_token]], dtype=np.int32),
                    "target_length": np.array([1], dtype=np.int32),
                    "input_states_1": state1,
                    "input_states_2": state2,
                },
            )
            idx = int(np.argmax(logits[0, 0, 0]))
            if idx == 1024:
                break
            tokens.append(idx)
            last_token = idx
            state1, state2 = next_state1, next_state2
            emitted += 1
    text = backend._tokenizer.decode(tokens)
    return text, tokens, state1, state2, last_token


def _words(text: str) -> list[str]:
    return re.findall(r"[a-z0-9']+", text.lower())


def _word_error_rate(reference: str, hypothesis: str) -> float:
    ref = _words(reference)
    hyp = _words(hypothesis)
    if not ref:
        return 0.0 if not hyp else 100.0

    prev = list(range(len(hyp) + 1))
    for i, ref_word in enumerate(ref, start=1):
        cur = [i] + [0] * len(hyp)
        for j, hyp_word in enumerate(hyp, start=1):
            cur[j] = min(
                prev[j] + 1,
                cur[j - 1] + 1,
                prev[j - 1] + (0 if ref_word == hyp_word else 1),
            )
        prev = cur
    return 100.0 * prev[-1] / len(ref)


def _append_by_overlap(committed: list[str], window_text: str) -> tuple[list[str], int, int]:
    incoming = window_text.split()
    if not incoming:
        return committed, 0, 0

    committed_norm = [w.lower().strip(".,!?;:\"()[]{}") for w in committed]
    incoming_norm = [w.lower().strip(".,!?;:\"()[]{}") for w in incoming]
    max_overlap = min(len(committed_norm), len(incoming_norm), 40)
    overlap = 0
    for size in range(max_overlap, 0, -1):
        if committed_norm[-size:] == incoming_norm[:size]:
            overlap = size
            break

    novel = incoming[overlap:]
    return committed + novel, overlap, len(novel)


def run_spike(
    *,
    wav_path: Path,
    artifact_path: Path,
    config: BufferedStreamingConfig,
) -> dict:
    audio = _read_wav_mono_16k(wav_path)
    duration_s = len(audio) / SAMPLE_RATE

    backend = ParakeetUnifiedORTBackend(
        encoder_provider=config.encoder_provider,
        decoder_provider=config.decoder_provider,
        max_tokens_per_step=config.max_tokens_per_step,
    )

    t0 = time.perf_counter()
    offline_text = backend._decode(audio)
    offline_decode_s = time.perf_counter() - t0

    committed_words: list[str] = []
    partials: list[dict] = []
    if config.mode == "chunked":
        live_times = []
        chunk_start_s = 0.0
        while chunk_start_s < duration_s:
            live_times.append(chunk_start_s)
            chunk_start_s += config.chunk_s
    else:
        live_times: list[float] = []
        live_t = config.chunk_s
        while live_t < duration_s:
            live_times.append(live_t)
            live_t += config.chunk_s
        if not live_times or abs(live_times[-1] - duration_s) > 1e-6:
            live_times.append(duration_s)

    state1 = np.zeros((2, 1, 640), dtype=np.float32)
    state2 = np.zeros((2, 1, 640), dtype=np.float32)
    last_token = 1024
    accumulated_tokens: list[int] = []
    accumulated_text = ""

    for live_t in live_times:
        if config.mode == "prefix":
            window_start_s = 0.0
        elif config.mode == "sliding":
            window_start_s = max(0.0, live_t - config.left_context_s)
        elif config.mode == "chunked":
            chunk_start_s = live_t
            chunk_end_s = min(duration_s, chunk_start_s + config.chunk_s)
            window_start_s = max(0.0, chunk_start_s - config.left_context_s)
            window_end_s = min(duration_s, chunk_end_s + config.right_context_s)
        else:
            raise ValueError(f"unknown buffered streaming mode {config.mode!r}")
        if config.mode != "chunked":
            window_end_s = min(duration_s, live_t + config.right_context_s)
        start_i = int(round(window_start_s * SAMPLE_RATE))
        end_i = int(round(window_end_s * SAMPLE_RATE))

        decode_start = time.perf_counter()
        if config.mode == "chunked":
            feats, feat_lens = backend._features(audio[start_i:end_i])
            enc_out, enc_len = backend._encoder.run(
                ["outputs", "encoded_lengths"],
                {"audio_signal": feats, "length": feat_lens},
            )
            left_s = chunk_start_s - window_start_s
            main_s = chunk_end_s - chunk_start_s
            left_frames = int(round(left_s * SAMPLE_RATE / 160.0)) // 8
            main_frames = int(round(main_s * SAMPLE_RATE / 160.0)) // 8
            available = min(int(enc_len[0]), int(enc_out.shape[2]))
            start_frame = min(left_frames, available)
            end_frame = min(start_frame + main_frames, available)
            window_text, tokens, state1, state2, last_token = _decode_encoder_frames(
                backend,
                enc_out,
                start_frame=start_frame,
                end_frame=end_frame,
                state1=state1,
                state2=state2,
                last_token=last_token,
            )
            accumulated_tokens.extend(tokens)
            accumulated_text = backend._tokenizer.decode(accumulated_tokens).strip()
            display_text = accumulated_text
        else:
            window_text = backend._decode(audio[start_i:end_i])
        decode_s = time.perf_counter() - decode_start
        if config.mode == "prefix":
            overlap_words = 0
            novel_words = len(window_text.split())
            display_text = window_text
        elif config.mode == "sliding":
            committed_words, overlap_words, novel_words = _append_by_overlap(
                committed_words, window_text
            )
            display_text = " ".join(committed_words).strip()
        else:
            overlap_words = 0
            novel_words = len(window_text.split())

        partials.append(
            {
                "live_t_s": round(live_t if config.mode != "chunked" else chunk_end_s, 3),
                "algorithmic_latency_s": round(
                    (
                        config.chunk_s + config.right_context_s
                        if config.mode == "chunked"
                        else config.chunk_s
                    ),
                    3,
                ),
                "window_start_s": round(window_start_s, 3),
                "window_end_s": round(window_end_s, 3),
                "window_audio_s": round((end_i - start_i) / SAMPLE_RATE, 3),
                "decode_s": round(decode_s, 4),
                "chunk_start_s": round(chunk_start_s, 3) if config.mode == "chunked" else None,
                "chunk_end_s": round(chunk_end_s, 3) if config.mode == "chunked" else None,
                "overlap_words": overlap_words,
                "novel_words": novel_words,
                "window_text": window_text,
                "display_text": display_text,
                "wer_vs_offline_pct": round(_word_error_rate(offline_text, display_text), 3),
            }
        )
    final_streaming_text = partials[-1]["display_text"] if partials else ""
    decode_times = [p["decode_s"] for p in partials]
    result = {
        "wav": str(wav_path),
        "artifact": str(artifact_path),
        "config": asdict(config),
        "duration_s": round(duration_s, 3),
        "backend": backend.name,
        "providers": {
            "encoder": backend._encoder.get_providers(),
            "decoder_joint": backend._decoder_joint.get_providers(),
        },
        "offline": {
            "decode_s": round(offline_decode_s, 4),
            "text": offline_text,
        },
        "streaming": {
            "partial_count": len(partials),
            "first_partial_live_t_s": partials[0]["live_t_s"] if partials else None,
            "first_partial_decode_s": partials[0]["decode_s"] if partials else None,
            "median_partial_decode_s": round(median(decode_times), 4) if decode_times else None,
            "max_partial_decode_s": round(max(decode_times), 4) if decode_times else None,
            "total_decode_s": round(sum(decode_times), 4),
            "final_text": final_streaming_text,
            "final_wer_vs_offline_pct": round(
                _word_error_rate(offline_text, final_streaming_text), 3
            ),
            "realtime_budget_pass": all(t <= config.chunk_s for t in decode_times),
            "partials": partials,
        },
    }
    artifact_path.write_text(json.dumps(result, indent=2), encoding="utf-8")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Spike Unified ONNX buffered streaming with sliding context windows."
    )
    parser.add_argument("--wav", type=Path, default=DEFAULT_WAV)
    parser.add_argument("--artifact", type=Path, default=DEFAULT_ARTIFACT)
    parser.add_argument("--chunk-s", type=float, default=1.12)
    parser.add_argument("--left-context-s", type=float, default=5.0)
    parser.add_argument("--right-context-s", type=float, default=0.32)
    parser.add_argument("--mode", choices=("prefix", "sliding", "chunked"), default="prefix")
    parser.add_argument("--encoder-provider", choices=("dml", "cpu"), default="dml")
    parser.add_argument("--decoder-provider", choices=("dml", "cpu"), default="cpu")
    parser.add_argument("--max-tokens-per-step", type=int, default=10)
    args = parser.parse_args()

    config = BufferedStreamingConfig(
        chunk_s=args.chunk_s,
        left_context_s=args.left_context_s,
        right_context_s=args.right_context_s,
        mode=args.mode,
        encoder_provider=args.encoder_provider,
        decoder_provider=args.decoder_provider,
        max_tokens_per_step=args.max_tokens_per_step,
    )
    result = run_spike(wav_path=args.wav, artifact_path=args.artifact, config=config)
    print(
        json.dumps(
            {
                "artifact": result["artifact"],
                "duration_s": result["duration_s"],
                "providers": result["providers"],
                "offline_decode_s": result["offline"]["decode_s"],
                "partial_count": result["streaming"]["partial_count"],
                "first_partial_live_t_s": result["streaming"]["first_partial_live_t_s"],
                "median_partial_decode_s": result["streaming"]["median_partial_decode_s"],
                "max_partial_decode_s": result["streaming"]["max_partial_decode_s"],
                "total_decode_s": result["streaming"]["total_decode_s"],
                "final_wer_vs_offline_pct": result["streaming"][
                    "final_wer_vs_offline_pct"
                ],
                "realtime_budget_pass": result["streaming"]["realtime_budget_pass"],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
