#!/usr/bin/env python3
"""Run the product-real rolling mel + explicit-cache + incremental TDT pipeline."""
from __future__ import annotations

import argparse
import hashlib
import json
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort

from cache_aware_mel_bench import mel_streaming_plan, read_pcm16_mono
from cache_aware_tdt_bench import (
    CURRENT_FRAMES,
    PREENCODE_FRAMES,
    CacheAwareTdtRuntime,
    DurableLedger,
    IncrementalTdtDecoder,
    decode_tdt,
    normalize_per_feature,
    provider_chain,
    sha256_file,
    tokens_to_text,
    validate_candidate_manifest,
)


class RollingFeatureBuffer:
    """Bounded raw-feature window keyed by absolute feature-frame index."""

    def __init__(self, *, feature_bins: int):
        self.base = 0
        self.data = np.empty((1, feature_bins, 0), dtype=np.float32)

    @property
    def frames(self) -> int:
        return int(self.data.shape[2])

    @property
    def end(self) -> int:
        return self.base + self.frames

    def append(self, feature_start: int, piece: np.ndarray) -> None:
        if piece.ndim != 3 or piece.shape[:2] != self.data.shape[:2]:
            raise ValueError("feature piece shape mismatch")
        if feature_start != self.end:
            raise ValueError(
                f"non-contiguous feature append: start={feature_start}, end={self.end}"
            )
        self.data = np.concatenate(
            [self.data, piece.astype(np.float32, copy=False)], axis=2
        )

    def build_chunk(self, offset: int) -> tuple[np.ndarray, int]:
        pre_start = max(0, offset - PREENCODE_FRAMES)
        if pre_start < self.base or offset > self.end:
            raise ValueError("required pre-encode features are not buffered")
        chunk = np.zeros((1, self.data.shape[1], PREENCODE_FRAMES + CURRENT_FRAMES), dtype=np.float32)
        pre = self.data[:, :, pre_start - self.base : offset - self.base]
        chunk[:, :, PREENCODE_FRAMES - pre.shape[2] : PREENCODE_FRAMES] = pre
        current_end = min(self.end, offset + CURRENT_FRAMES)
        current = self.data[:, :, offset - self.base : current_end - self.base]
        chunk[:, :, PREENCODE_FRAMES : PREENCODE_FRAMES + current.shape[2]] = current
        return chunk, PREENCODE_FRAMES + current.shape[2]

    def discard_for_next_offset(self, next_offset: int) -> None:
        keep_from = max(0, next_offset - PREENCODE_FRAMES)
        if keep_from <= self.base:
            return
        keep_from = min(keep_from, self.end)
        self.data = self.data[:, :, keep_from - self.base :].copy()
        self.base = keep_from


def ready_offsets(next_offset: int, available_frames: int, *, final: bool) -> list[int]:
    if next_offset < 0 or available_frames < 0:
        raise ValueError("feature offsets must be non-negative")
    if final:
        return list(range(next_offset, available_frames, 200))
    ready: list[int] = []
    while next_offset + CURRENT_FRAMES <= available_frames:
        ready.append(next_offset)
        next_offset += 200
    return ready


class PipelineCacheState:
    def __init__(self, runtime: CacheAwareTdtRuntime, *, verify_deferred: bool):
        self.runtime = runtime
        self.channel, self.temporal, self.cache_len = runtime._initial_caches()
        self.decoder = IncrementalTdtDecoder(
            runtime.decoder,
            blank_idx=runtime.blank_idx,
            vocab_size=len(runtime.vocab),
            state_shape=runtime.state_shape,
            duration_cap=2,
        )
        self.verify_deferred = verify_deferred
        self.encoded_parts: list[np.ndarray] = []
        self.encoder_ms: list[float] = []
        self.decoder_ms: list[float] = []
        self.output_lengths: list[int] = []

    def push(self, chunk: np.ndarray, valid_len: int) -> tuple[float, float]:
        started = time.perf_counter()
        outputs = self.runtime.encoder.run(
            None,
            {
                "melspectrogram_features": chunk,
                "mel_len": np.asarray([valid_len], dtype=np.int64),
                "cache_last_channel": self.channel,
                "cache_last_time": self.temporal,
                "cache_last_channel_len": self.cache_len,
            },
        )
        encoder_ms = (time.perf_counter() - started) * 1000.0
        encoded, encoded_len, self.channel, self.temporal, self.cache_len = outputs
        out_len = int(encoded_len[0])
        if self.channel.shape != (1, 24, 125, 1024):
            raise RuntimeError("attention cache shape changed")
        if self.temporal.shape != (1, 24, 1024, 4):
            raise RuntimeError("convolution cache shape changed")
        if not 0 <= out_len <= encoded.shape[2]:
            raise RuntimeError("encoder output length is invalid")
        current = encoded[:, :, :out_len]
        if self.verify_deferred:
            self.encoded_parts.append(current)
        decoder_started = time.perf_counter()
        self.decoder.push(current.transpose(0, 2, 1)[0])
        decoder_ms = (time.perf_counter() - decoder_started) * 1000.0
        self.encoder_ms.append(encoder_ms)
        self.decoder_ms.append(decoder_ms)
        self.output_lengths.append(out_len)
        return encoder_ms, decoder_ms

    def finish(self) -> dict[str, object]:
        deferred_ms = 0.0
        deferred_match: bool | None = None
        if self.verify_deferred:
            encoded = np.concatenate(self.encoded_parts, axis=2).transpose(0, 2, 1)[0]
            started = time.perf_counter()
            deferred = decode_tdt(
                encoded,
                self.runtime.decoder,
                blank_idx=self.runtime.blank_idx,
                vocab_size=len(self.runtime.vocab),
                state_shape=self.runtime.state_shape,
                duration_cap=2,
            )
            deferred_ms = (time.perf_counter() - started) * 1000.0
            deferred_match = deferred == self.decoder.tokens
            if not deferred_match:
                raise RuntimeError("incremental TDT tokens differ from deferred decode")
        return {
            "tokens": self.decoder.tokens,
            "hypothesis": tokens_to_text(self.decoder.tokens, self.runtime.vocab).strip(),
            "encoder_chunk_ms": self.encoder_ms,
            "decoder_chunk_ms": self.decoder_ms,
            "encoder_output_lengths": self.output_lengths,
            "encoder_total_ms": float(sum(self.encoder_ms)),
            "decoder_total_ms": float(sum(self.decoder_ms)),
            "deferred_tokens_match": deferred_match,
            "deferred_verify_ms": deferred_ms,
        }


def transcribe_audio(
    runtime: CacheAwareTdtRuntime,
    mel_session: ort.InferenceSession,
    audio: np.ndarray,
    *,
    verify_deferred: bool,
) -> dict[str, object]:
    feature_buffer = RollingFeatureBuffer(feature_bins=128)
    cache = PipelineCacheState(runtime, verify_deferred=verify_deferred)
    next_offset = 0
    schedule_finish_s = 0.0
    mel_ms: list[float] = []
    prep_ms: list[float] = []
    max_buffer_frames = 0
    for event in mel_streaming_plan(int(audio.size)):
        observed = int(event["observed_samples"])
        sample_start = int(event["sample_start"])
        local_start = int(event["local_feature_start"])
        feature_start = int(event["feature_start"])
        feature_end = int(event["feature_end"])
        segment = audio[sample_start:observed][None, :]
        started = time.perf_counter()
        raw = mel_session.run(None, {"audio": segment})[0]
        current_mel_ms = (time.perf_counter() - started) * 1000.0
        count = feature_end - feature_start
        raw_features = raw.transpose(0, 2, 1).astype(np.float32, copy=False)
        piece = raw_features[:, :, local_start : local_start + count]
        if piece.shape[2] != count:
            raise RuntimeError("mel graph returned too few stable frames")
        feature_buffer.append(feature_start, piece.copy())
        final = bool(event["final"])
        if final:
            feature_buffer.append(
                feature_end, np.zeros((1, 128, 1), dtype=np.float32)
            )
        max_buffer_frames = max(max_buffer_frames, feature_buffer.frames)
        ready_s = observed / 16_000.0
        schedule_finish_s = max(schedule_finish_s, ready_s) + current_mel_ms / 1000.0
        mel_ms.append(current_mel_ms)

        offsets = ready_offsets(next_offset, feature_buffer.end, final=final)
        for offset in offsets:
            prep_started = time.perf_counter()
            chunk, valid_len = feature_buffer.build_chunk(offset)
            chunk = normalize_per_feature(chunk, valid_len)
            current_prep_ms = (time.perf_counter() - prep_started) * 1000.0
            encoder_ms, decoder_ms = cache.push(chunk, valid_len)
            prep_ms.append(current_prep_ms)
            schedule_finish_s += (current_prep_ms + encoder_ms + decoder_ms) / 1000.0
            next_offset = offset + 200
            feature_buffer.discard_for_next_offset(next_offset)
            max_buffer_frames = max(max_buffer_frames, feature_buffer.frames)

    result = cache.finish()
    duration_s = audio.size / 16_000.0
    return {
        **result,
        "mel_chunk_ms": mel_ms,
        "mel_total_ms": float(sum(mel_ms)),
        "feature_prep_chunk_ms": prep_ms,
        "feature_prep_total_ms": float(sum(prep_ms)),
        "max_feature_buffer_frames": max_buffer_frames,
        "steps": len(prep_ms),
        "simulated_stop_tail_ms": max(0.0, schedule_finish_s - duration_s) * 1000.0,
    }


def _digest(value: object) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--mel-model", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--audio-root", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--packet", required=True, type=Path)
    parser.add_argument("--definition", required=True, type=Path)
    parser.add_argument("--environment-lock", required=True, type=Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--candidate-id", default="windows-cache-explicit-rolling-mel")
    parser.add_argument("--mel-provider", choices=("cpu", "dml", "cuda"), default="cpu")
    parser.add_argument("--encoder-provider", choices=("cpu", "dml", "cuda"), default="dml")
    parser.add_argument("--decoder-provider", choices=("cpu", "dml", "cuda"), default="dml")
    parser.add_argument("--verify-deferred", action="store_true")
    parser.add_argument("--clip", action="append")
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()

    rows = json.loads(args.manifest.read_text(encoding="utf-8"))
    validate_candidate_manifest(rows)
    if args.clip:
        wanted = set(args.clip)
        rows = [row for row in rows if row["id"] in wanted]
        missing = wanted - {str(row["id"]) for row in rows}
        if missing:
            raise ValueError(f"unknown clip id(s): {sorted(missing)}")
    if args.limit is not None:
        rows = rows[: args.limit]
    if not rows:
        raise ValueError("no clips selected")

    model_hashes = {
        name: sha256_file(args.model_dir / name)
        for name in (
            "encoder-model.onnx",
            "decoder_joint-model.onnx",
            "vocab.txt",
            "config.json",
        )
    }
    mel_hashes = {"onnx": sha256_file(args.mel_model)}
    mel_sidecar = args.mel_model.with_name(args.mel_model.name + ".data")
    if mel_sidecar.exists():
        mel_hashes["data"] = sha256_file(mel_sidecar)
    bindings = {
        "run_id": args.run_id,
        "candidate_id": args.candidate_id,
        "packet_sha256": sha256_file(args.packet),
        "definition_sha256": sha256_file(args.definition),
        "runner_sha256": sha256_file(Path(__file__).resolve()),
        "environment_lock_sha256": sha256_file(args.environment_lock),
        "candidate_config_sha256": _digest(
            {
                "mel_provider": args.mel_provider,
                "encoder_provider": args.encoder_provider,
                "decoder_provider": args.decoder_provider,
                "mel_padding": "constant",
                "mel_final_frame": "masked_zero",
                "mel_shift_samples": 32_000,
                "mel_nonfinal_lookahead_samples": 96,
                "online_normalization": "per_feature_sample_std_plus_1e-5",
                "fixed_input_frames": 409,
                "shift_frames": 200,
                "duration_cap": 2,
                "verify_deferred": args.verify_deferred,
            }
        ),
        "model_component_sha256s": model_hashes,
        "mel_model_sha256s": mel_hashes,
    }
    ledger = DurableLedger(args.out)
    work_items: list[
        tuple[int, dict[str, object], Path, dict[str, object], str]
    ] = []
    for index, item in enumerate(rows, start=1):
        clip_id = str(item["id"])
        audio_path = args.audio_root / f"{clip_id}.wav"
        audio_hash = sha256_file(audio_path)
        if audio_hash != item["sha256"]:
            raise ValueError(f"audio hash mismatch: {clip_id}")
        row_binding = {**bindings, "clip_id": clip_id, "audio_sha256": audio_hash}
        resume_key = _digest(row_binding)
        if resume_key in ledger.rows:
            print(f"[{index}/{len(rows)}] {clip_id} resume=skip", flush=True)
            continue
        work_items.append((index, item, audio_path, row_binding, resume_key))
    if not work_items:
        print("resume_complete: no model load required", flush=True)
        return 0

    runtime = CacheAwareTdtRuntime(
        args.model_dir, args.encoder_provider, args.decoder_provider
    )
    mel_providers = provider_chain(args.mel_provider)
    options = ort.SessionOptions()
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.enable_mem_pattern = args.mel_provider != "dml"
    options.intra_op_num_threads = 4
    options.inter_op_num_threads = 1
    mel_session = ort.InferenceSession(
        str(args.mel_model), sess_options=options, providers=mel_providers
    )
    if mel_session.get_providers()[0] != mel_providers[0]:
        raise RuntimeError("mel provider silently fell back")
    warmup_ms = runtime.warmup()
    mel_warmup_started = time.perf_counter()
    mel_session.run(None, {"audio": np.zeros((1, 32_096), dtype=np.float32)})
    mel_warmup_ms = (time.perf_counter() - mel_warmup_started) * 1000.0

    failures = 0
    for index, item, audio_path, row_binding, resume_key in work_items:
        clip_id = str(item["id"])
        try:
            read_started = time.perf_counter()
            audio = read_pcm16_mono(audio_path)
            wav_read_ms = (time.perf_counter() - read_started) * 1000.0
            result = transcribe_audio(
                runtime, mel_session, audio, verify_deferred=args.verify_deferred
            )
            row = {
                **row_binding,
                "resume_key": resume_key,
                "status": "ok",
                "duration_s": float(item["duration_s"]),
                "wav_read_ms": wav_read_ms,
                "warmup_ms": warmup_ms,
                "mel_warmup_ms": mel_warmup_ms,
                "mel_provider": mel_session.get_providers(),
                "encoder_provider": runtime.encoder.get_providers(),
                "decoder_provider": runtime.decoder.get_providers(),
                **result,
                "error": None,
            }
        except Exception as exc:
            failures += 1
            row = {
                **row_binding,
                "resume_key": resume_key,
                "status": "error",
                "error": f"{type(exc).__name__}: {exc}",
            }
        ledger.append(row)
        print(
            f"[{index}/{len(rows)}] {clip_id} status={row['status']} "
            f"steps={row.get('steps', 0)} tail_ms={float(row.get('simulated_stop_tail_ms', 0)):.1f} "
            f"text={str(row.get('hypothesis', ''))[:100]!r}",
            flush=True,
        )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
