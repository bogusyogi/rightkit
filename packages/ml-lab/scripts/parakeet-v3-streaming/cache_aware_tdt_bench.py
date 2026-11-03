#!/usr/bin/env python3
"""Run the exported Parakeet v3 explicit-cache encoder without scorer access."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import time
from pathlib import Path
from typing import Any

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper


PREENCODE_FRAMES = 9
CURRENT_FRAMES = 400
FIXED_INPUT_FRAMES = PREENCODE_FRAMES + CURRENT_FRAMES
ALLOWED_CANDIDATE_FIELDS = {
    "id",
    "path",
    "sha256",
    "sample_rate_hz",
    "channels",
    "duration_s",
}


def build_fixed_cache_chunk(features: np.ndarray, offset: int) -> tuple[np.ndarray, int]:
    """Build NeMo's pad-and-drop export input at one 200-frame shift."""
    if features.ndim != 3 or features.shape[0] != 1:
        raise ValueError("features must have shape [1, feature_bins, frames]")
    if offset < 0:
        raise ValueError("offset must be non-negative")

    chunk = np.zeros(
        (1, features.shape[1], FIXED_INPUT_FRAMES), dtype=np.float32
    )
    pre_start = max(0, offset - PREENCODE_FRAMES)
    pre = features[:, :, pre_start:offset]
    pre_target_start = PREENCODE_FRAMES - pre.shape[2]
    chunk[:, :, pre_target_start:PREENCODE_FRAMES] = pre

    current = features[:, :, offset : offset + CURRENT_FRAMES]
    chunk[:, :, PREENCODE_FRAMES : PREENCODE_FRAMES + current.shape[2]] = current
    valid_len = PREENCODE_FRAMES + current.shape[2]
    return chunk, valid_len


def normalize_per_feature(chunk: np.ndarray, valid_len: int) -> np.ndarray:
    """Match NeMo normalize_batch(..., normalize_type='per_feature')."""
    if chunk.ndim != 3 or not 1 <= valid_len <= chunk.shape[2]:
        raise ValueError("valid_len must select at least one feature frame")
    valid = chunk[:, :, :valid_len]
    mean = valid.mean(axis=2, keepdims=True, dtype=np.float32)
    if valid_len == 1:
        std = np.zeros_like(mean)
    else:
        std = valid.std(axis=2, ddof=1, keepdims=True, dtype=np.float32)
    normalized = (chunk - mean) / (std + np.float32(1e-5))
    normalized[:, :, valid_len:] = 0.0
    return normalized.astype(np.float32, copy=False)


def feature_offsets(frame_count: int) -> list[int]:
    if frame_count < 0:
        raise ValueError("frame_count must be non-negative")
    return list(range(0, frame_count, 200))


def tokens_to_text(tokens: list[int], vocab: dict[int, str]) -> str:
    pieces = [vocab[token].replace("\u2581", " ") for token in tokens]
    text = "".join(pieces)
    return re.sub(r"\A\s|\s\B|(\s)\b", lambda match: " " if match.group(1) else "", text)


def validate_candidate_manifest(rows: list[dict[str, object]]) -> None:
    required = ALLOWED_CANDIDATE_FIELDS
    for index, row in enumerate(rows):
        extra = set(row) - ALLOWED_CANDIDATE_FIELDS
        missing = required - set(row)
        if extra:
            raise ValueError(
                f"row {index} has forbidden candidate-visible field(s): {sorted(extra)}"
            )
        if missing:
            raise ValueError(f"row {index} is missing field(s): {sorted(missing)}")
        if len(str(row["sha256"])) != 64:
            raise ValueError(f"row {index} has an invalid audio sha256")


def derive_raw_mel_model(source: Path, target: Path) -> None:
    """Create a NeMo-compatible graph exposing the pre-normalization `log` tensor."""
    model = onnx.load(source, load_external_data=True)
    for node in model.graph.node:
        if node.op_type != "Pad":
            continue
        mode = next((attr for attr in node.attribute if attr.name == "mode"), None)
        if mode is not None and mode.s == b"reflect":
            mode.s = b"constant"
    value = next(
        (item for item in [*model.graph.value_info, *model.graph.output] if item.name == "log"),
        None,
    )
    if value is None:
        value = helper.make_tensor_value_info("log", TensorProto.FLOAT, [1, 128, None])
    del model.graph.output[:]
    model.graph.output.append(value)
    target.parent.mkdir(parents=True, exist_ok=True)
    sidecar = target.with_name(target.name + ".data")
    target.unlink(missing_ok=True)
    sidecar.unlink(missing_ok=True)
    onnx.save_model(
        model,
        target,
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location=sidecar.name,
        size_threshold=1024,
    )


def finalize_nemo_raw_mel(raw: np.ndarray) -> np.ndarray:
    """Convert the ONNX `[batch,time,feature]` log mel to NeMo's masked layout."""
    if raw.ndim != 3 or raw.shape[0] != 1 or raw.shape[1] < 1:
        raise ValueError("raw mel must have shape [1, time, feature]")
    finalized = raw.transpose(0, 2, 1).astype(np.float32, copy=True)
    finalized[:, :, -1] = 0.0
    return finalized


def decode_tdt(
    encoded: np.ndarray,
    decoder: Any,
    *,
    blank_idx: int,
    vocab_size: int,
    state_shape: tuple[int, int, int],
    duration_cap: int,
    max_tokens_per_step: int = 10,
) -> list[int]:
    """Greedy TDT decode matching the locked onnx-asr duration-cap behavior."""
    incremental = IncrementalTdtDecoder(
        decoder,
        blank_idx=blank_idx,
        vocab_size=vocab_size,
        state_shape=state_shape,
        duration_cap=duration_cap,
        max_tokens_per_step=max_tokens_per_step,
    )
    incremental.push(encoded)
    return incremental.tokens


class IncrementalTdtDecoder:
    """Carry TDT predictor state and duration jumps across encoder chunks."""

    def __init__(
        self,
        decoder: Any,
        *,
        blank_idx: int,
        vocab_size: int,
        state_shape: tuple[int, int, int],
        duration_cap: int,
        max_tokens_per_step: int = 10,
    ):
        self.decoder = decoder
        self.blank_idx = blank_idx
        self.vocab_size = vocab_size
        self.duration_cap = duration_cap
        self.max_tokens_per_step = max_tokens_per_step
        self.state = (
            np.zeros(state_shape, dtype=np.float32),
            np.zeros(state_shape, dtype=np.float32),
        )
        self.tokens: list[int] = []
        self.pending_frames = 0
        self.emitted_tokens = 0

    def push(self, encoded: np.ndarray) -> None:
        time_index = self.pending_frames
        self.pending_frames = 0
        while time_index < encoded.shape[0]:
            outputs, state1, state2 = self.decoder.run(
            ["outputs", "output_states_1", "output_states_2"],
            {
                "encoder_outputs": encoded[time_index][None, :, None],
                "targets": np.asarray(
                        [[self.tokens[-1] if self.tokens else self.blank_idx]],
                        dtype=np.int32,
                ),
                "target_length": np.asarray([1], dtype=np.int32),
                    "input_states_1": self.state[0],
                    "input_states_2": self.state[1],
            },
        )
            logits = np.asarray(outputs).squeeze()
            token = int(np.argmax(logits[: self.vocab_size]))
            step = min(
                int(np.argmax(logits[self.vocab_size :])), self.duration_cap
            )
            if token != self.blank_idx:
                self.state = (state1, state2)
                self.tokens.append(token)
                self.emitted_tokens += 1
            if step > 0:
                time_index += step
                self.emitted_tokens = 0
            elif token == self.blank_idx or self.emitted_tokens >= self.max_tokens_per_step:
                time_index += 1
                self.emitted_tokens = 0
        self.pending_frames = max(0, time_index - encoded.shape[0])


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def load_vocab(path: Path) -> tuple[dict[int, str], int]:
    vocab: dict[int, str] = {}
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            token, raw_index = line.rstrip("\n").rsplit(" ", 1)
            vocab[int(raw_index)] = token
    blank = next((index for index, token in vocab.items() if token == "<blk>"), None)
    if blank is None:
        raise ValueError("vocabulary is missing <blk>")
    return vocab, blank


def _session_options(*, dml: bool, intra_threads: int = 4) -> ort.SessionOptions:
    options = ort.SessionOptions()
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.enable_mem_pattern = not dml
    options.intra_op_num_threads = intra_threads
    options.inter_op_num_threads = 1
    return options


def provider_chain(name: str, available: set[str] | None = None) -> list[str]:
    requested = {
        "cpu": "CPUExecutionProvider",
        "dml": "DmlExecutionProvider",
        "cuda": "CUDAExecutionProvider",
    }.get(name)
    if requested is None:
        raise ValueError(f"unsupported provider: {name}")
    available = available if available is not None else set(ort.get_available_providers())
    if requested not in available:
        raise RuntimeError(f"provider is unavailable: {requested}")
    return [requested] if name == "cpu" else [requested, "CPUExecutionProvider"]


class CacheAwareTdtRuntime:
    """Exported explicit-cache encoder plus the locked TDT decoder/joint."""

    def __init__(self, model_dir: Path, encoder_provider: str, decoder_provider: str):
        available = set(ort.get_available_providers())
        encoder_providers = provider_chain(encoder_provider, available)
        decoder_providers = provider_chain(decoder_provider, available)
        self.encoder = ort.InferenceSession(
            str(model_dir / "encoder-model.onnx"),
            sess_options=_session_options(dml=encoder_provider == "dml"),
            providers=encoder_providers,
        )
        self.decoder = ort.InferenceSession(
            str(model_dir / "decoder_joint-model.onnx"),
            sess_options=_session_options(dml=decoder_provider == "dml", intra_threads=2),
            providers=decoder_providers,
        )
        if self.encoder.get_providers()[0] != encoder_providers[0]:
            raise RuntimeError(
                f"encoder provider fell back: requested={encoder_providers[0]} "
                f"active={self.encoder.get_providers()}"
            )
        if self.decoder.get_providers()[0] != decoder_providers[0]:
            raise RuntimeError(
                f"decoder provider fell back: requested={decoder_providers[0]} "
                f"active={self.decoder.get_providers()}"
            )
        self.vocab, self.blank_idx = load_vocab(model_dir / "vocab.txt")
        decoder_shapes = {item.name: item.shape for item in self.decoder.get_inputs()}
        state = decoder_shapes["input_states_1"]
        self.state_shape = (int(state[0]), 1, int(state[2]))
        self.encoder_provider = encoder_provider
        self.decoder_provider = decoder_provider

    @staticmethod
    def _initial_caches() -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        return (
            np.zeros((1, 24, 125, 1024), dtype=np.float32),
            np.zeros((1, 24, 1024, 4), dtype=np.float32),
            np.zeros((1,), dtype=np.int64),
        )

    def warmup(self) -> float:
        chunk = np.zeros((1, 128, FIXED_INPUT_FRAMES), dtype=np.float32)
        channel, temporal, cache_len = self._initial_caches()
        started = time.perf_counter()
        for _ in range(2):
            outputs = self.encoder.run(
                None,
                {
                    "melspectrogram_features": chunk,
                    "mel_len": np.asarray([FIXED_INPUT_FRAMES], dtype=np.int64),
                    "cache_last_channel": channel,
                    "cache_last_time": temporal,
                    "cache_last_channel_len": cache_len,
                },
            )
            channel, temporal, cache_len = outputs[2], outputs[3], outputs[4]
        decoder = IncrementalTdtDecoder(
            self.decoder,
            blank_idx=self.blank_idx,
            vocab_size=len(self.vocab),
            state_shape=self.state_shape,
            duration_cap=2,
        )
        decoder.push(np.zeros((4, 1024), dtype=np.float32))
        return (time.perf_counter() - started) * 1000.0

    def transcribe_features(
        self, features: np.ndarray, duration_s: float, *, verify_deferred: bool = False
    ) -> dict[str, object]:
        if features.ndim != 3 or features.shape[:2] != (1, 128):
            raise ValueError("features must have shape [1,128,T]")
        if not np.isfinite(features).all():
            raise ValueError("features contain non-finite values")

        channel, temporal, cache_len = self._initial_caches()
        encoded_parts: list[np.ndarray] = []
        chunk_ms: list[float] = []
        decoder_chunk_ms: list[float] = []
        lengths: list[int] = []
        schedule_finish_s = 0.0
        incremental = IncrementalTdtDecoder(
            self.decoder,
            blank_idx=self.blank_idx,
            vocab_size=len(self.vocab),
            state_shape=self.state_shape,
            duration_cap=2,
        )
        for offset in feature_offsets(features.shape[2]):
            chunk, valid_len = build_fixed_cache_chunk(features, offset)
            chunk = normalize_per_feature(chunk, valid_len)
            started = time.perf_counter()
            outputs = self.encoder.run(
                None,
                {
                    "melspectrogram_features": chunk,
                    "mel_len": np.asarray([valid_len], dtype=np.int64),
                    "cache_last_channel": channel,
                    "cache_last_time": temporal,
                    "cache_last_channel_len": cache_len,
                },
            )
            elapsed_ms = (time.perf_counter() - started) * 1000.0
            encoded, encoded_len, channel, temporal, cache_len = outputs
            out_len = int(encoded_len[0])
            if encoded.shape[1] != 1024 or not 0 <= out_len <= encoded.shape[2]:
                raise RuntimeError("encoder returned an invalid output shape or length")
            if channel.shape != (1, 24, 125, 1024) or temporal.shape != (1, 24, 1024, 4):
                raise RuntimeError("encoder cache shape changed")
            if not all(np.isfinite(item).all() for item in (encoded, channel, temporal)):
                raise RuntimeError("encoder returned non-finite values")
            current_encoded = encoded[:, :, :out_len]
            if verify_deferred:
                encoded_parts.append(current_encoded)
            decoder_started = time.perf_counter()
            incremental.push(current_encoded.transpose(0, 2, 1)[0])
            current_decoder_ms = (time.perf_counter() - decoder_started) * 1000.0
            lengths.append(out_len)
            chunk_ms.append(elapsed_ms)
            decoder_chunk_ms.append(current_decoder_ms)

            ready_s = min(duration_s, (offset + CURRENT_FRAMES) * 0.01)
            schedule_finish_s = max(schedule_finish_s, ready_s) + (
                elapsed_ms + current_decoder_ms
            ) / 1000.0

        if not lengths:
            return {
                "hypothesis": "",
                "tokens": [],
                "chunk_ms": [],
                "decoder_chunk_ms": [],
                "encoder_output_lengths": [],
                "encoder_total_ms": 0.0,
                "decoder_ms": 0.0,
                "simulated_stop_tail_ms": 0.0,
            }
        tokens = incremental.tokens
        decoder_ms = float(sum(decoder_chunk_ms))
        deferred_verify_ms = 0.0
        if verify_deferred:
            encoded_all = np.concatenate(encoded_parts, axis=2).transpose(0, 2, 1)[0]
            started = time.perf_counter()
            deferred_tokens = decode_tdt(
                encoded_all,
                self.decoder,
                blank_idx=self.blank_idx,
                vocab_size=len(self.vocab),
                state_shape=self.state_shape,
                duration_cap=2,
            )
            deferred_verify_ms = (time.perf_counter() - started) * 1000.0
            if tokens != deferred_tokens:
                raise RuntimeError("incremental TDT tokens differ from deferred decode")
        stop_tail_ms = max(0.0, schedule_finish_s - duration_s) * 1000.0
        return {
            "hypothesis": tokens_to_text(tokens, self.vocab).strip(),
            "tokens": tokens,
            "chunk_ms": chunk_ms,
            "decoder_chunk_ms": decoder_chunk_ms,
            "encoder_output_lengths": lengths,
            "encoder_total_ms": float(sum(chunk_ms)),
            "decoder_ms": decoder_ms,
            "deferred_tokens_match": True if verify_deferred else None,
            "deferred_verify_ms": deferred_verify_ms,
            "simulated_stop_tail_ms": stop_tail_ms,
        }


def _canonical_row(row: dict[str, object]) -> str:
    return json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


class DurableLedger:
    """Append-only JSONL ledger with idempotent, conflict-detecting resume."""

    def __init__(self, path: Path):
        self.path = path
        self.rows: dict[str, dict[str, object]] = {}
        if path.exists():
            lines = path.read_text(encoding="utf-8").splitlines()
            for line_number, line in enumerate(lines, start=1):
                if not line.strip():
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError as exc:
                    if line_number == len(lines):
                        break
                    raise ValueError(f"invalid JSONL row {line_number}") from exc
                self._remember(row)

    def _remember(self, row: dict[str, object]) -> None:
        key = str(row.get("resume_key", ""))
        if not key:
            raise ValueError("row is missing resume_key")
        prior = self.rows.get(key)
        if prior is not None and _canonical_row(prior) != _canonical_row(row):
            raise ValueError(f"conflicting duplicate resume_key: {key}")
        self.rows[key] = row

    def append(self, row: dict[str, object]) -> bool:
        key = str(row.get("resume_key", ""))
        prior = self.rows.get(key)
        if prior is not None:
            if _canonical_row(prior) != _canonical_row(row):
                raise ValueError(f"conflicting duplicate resume_key: {key}")
            return False
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8", newline="\n") as handle:
            handle.write(_canonical_row(row) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        self.rows[key] = row
        return True


def _digest_json(value: object) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--audio-root", required=True, type=Path)
    parser.add_argument("--features-root", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--packet", required=True, type=Path)
    parser.add_argument("--definition", required=True, type=Path)
    parser.add_argument("--environment-lock", required=True, type=Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--candidate-id", default="windows-cache-explicit")
    parser.add_argument("--provider", choices=("dml", "cpu", "cuda"), help=argparse.SUPPRESS)
    parser.add_argument("--encoder-provider", choices=("dml", "cpu", "cuda"))
    parser.add_argument("--decoder-provider", choices=("dml", "cpu", "cuda"), default="cpu")
    parser.add_argument("--verify-deferred", action="store_true")
    parser.add_argument("--clip", action="append")
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()
    encoder_provider = args.encoder_provider or args.provider or "dml"

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

    runner_path = Path(__file__).resolve()
    model_files = [
        "encoder-model.onnx",
        "decoder_joint-model.onnx",
        "vocab.txt",
        "config.json",
    ]
    model_hashes = {
        name: sha256_file(args.model_dir / name)
        for name in model_files
    }
    bindings = {
        "run_id": args.run_id,
        "candidate_id": args.candidate_id,
        "packet_sha256": sha256_file(args.packet),
        "definition_sha256": sha256_file(args.definition),
        "runner_sha256": sha256_file(runner_path),
        "environment_lock_sha256": sha256_file(args.environment_lock),
        "candidate_config_sha256": _digest_json(
            {
                "encoder_provider": encoder_provider,
                "decoder_provider": args.decoder_provider,
                "feature_source": "nemo_preprocessor_unnormalized",
                "online_normalization": True,
                "pad_and_drop_preencoded": True,
                "preencode_frames": PREENCODE_FRAMES,
                "current_frames": CURRENT_FRAMES,
                "shift_frames": 200,
                "duration_cap": 2,
                "incremental_decoder_state": True,
                "verify_deferred": args.verify_deferred,
            }
        ),
        "model_component_sha256s": model_hashes,
    }
    ledger = DurableLedger(args.out)
    work_items: list[tuple[int, dict[str, object], Path, dict[str, object], str]] = []
    for index, item in enumerate(rows, start=1):
        clip_id = str(item["id"])
        audio_path = args.audio_root / f"{clip_id}.wav"
        feature_path = args.features_root / f"{clip_id}.npy"
        audio_hash = sha256_file(audio_path)
        if audio_hash != item["sha256"]:
            raise ValueError(f"audio hash mismatch: {clip_id}")
        row_binding = {
            **bindings,
            "clip_id": clip_id,
            "audio_sha256": audio_hash,
            "feature_sha256": sha256_file(feature_path),
        }
        resume_key = _digest_json(row_binding)
        if resume_key in ledger.rows:
            print(f"[{index}/{len(rows)}] {clip_id} resume=skip", flush=True)
            continue
        work_items.append((index, item, feature_path, row_binding, resume_key))
    if not work_items:
        print("resume_complete: no model load required", flush=True)
        return 0

    runtime = CacheAwareTdtRuntime(args.model_dir, encoder_provider, args.decoder_provider)
    warmup_ms = runtime.warmup()
    print(
        f"warmup_ms={warmup_ms:.1f} encoder={runtime.encoder.get_providers()} "
        f"decoder={runtime.decoder.get_providers()}",
        flush=True,
    )

    failures = 0
    for index, item, feature_path, row_binding, resume_key in work_items:
        clip_id = str(item["id"])
        try:
            features = np.load(feature_path, allow_pickle=False)
            result = runtime.transcribe_features(
                features,
                float(item["duration_s"]),
                verify_deferred=args.verify_deferred,
            )
            row = {
                **row_binding,
                "resume_key": resume_key,
                "status": "ok",
                "duration_s": float(item["duration_s"]),
                "feature_shape": list(features.shape),
                "encoder_provider": runtime.encoder.get_providers(),
                "decoder_provider": runtime.decoder.get_providers(),
                "warmup_ms": warmup_ms,
                **result,
                "error": None,
            }
        except Exception as exc:
            failures += 1
            row = {
                **row_binding,
                "resume_key": resume_key,
                "status": "error",
                "duration_s": float(item["duration_s"]),
                "error": f"{type(exc).__name__}: {exc}",
            }
        ledger.append(row)
        print(
            f"[{index}/{len(rows)}] {clip_id} status={row['status']} "
            f"chunks={len(row.get('chunk_ms', []))} tail_ms={float(row.get('simulated_stop_tail_ms', 0)):.1f} "
            f"text={str(row.get('hypothesis', ''))[:120]!r}",
            flush=True,
        )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
