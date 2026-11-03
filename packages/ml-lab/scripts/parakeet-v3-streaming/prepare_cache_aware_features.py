#!/usr/bin/env python3
"""Prepare candidate-visible NeMo mel tensors from hash-locked audio."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path

import numpy as np

from cache_aware_tdt_bench import DurableLedger, sha256_file, validate_candidate_manifest
from nemo_cache_export import configure_cache_export


def digest_json(value: object) -> str:
    body = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def save_npy_atomic(path: Path, value: np.ndarray) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("wb") as handle:
        np.save(handle, value, allow_pickle=False)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--model-sha256", required=True)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--audio-root", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
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

    model_hash = sha256_file(args.model)
    if model_hash != args.model_sha256:
        raise ValueError("source model hash mismatch")
    config = {
        "model_sha256": model_hash,
        "online_normalization": True,
        "pad_and_drop_preencoded": True,
        "stored_tensor": "unnormalized_preprocessor_output",
        "dtype": "float32",
    }
    config_hash = digest_json(config)
    args.out.mkdir(parents=True, exist_ok=True)
    ledger = DurableLedger(args.out / "feature-manifest.jsonl")
    pending: list[tuple[int, dict[str, object], Path, str]] = []
    for index, row in enumerate(rows, start=1):
        clip_id = str(row["id"])
        audio_path = args.audio_root / f"{clip_id}.wav"
        if sha256_file(audio_path) != row["sha256"]:
            raise ValueError(f"audio hash mismatch: {clip_id}")
        resume_key = digest_json(
            {
                "id": clip_id,
                "audio_sha256": row["sha256"],
                "config_sha256": config_hash,
            }
        )
        prior = ledger.rows.get(resume_key)
        feature_path = args.out / f"{clip_id}.npy"
        if prior is not None:
            if not feature_path.is_file() or sha256_file(feature_path) != prior["feature_sha256"]:
                raise ValueError(f"completed feature changed: {clip_id}")
            print(f"[{index}/{len(rows)}] {clip_id} resume=skip", flush=True)
            continue
        pending.append((index, row, audio_path, resume_key))
    if not pending:
        print("resume_complete: no model load required", flush=True)
        return 0

    from nemo.collections.asr.models import EncDecRNNTBPEModel
    from nemo.collections.asr.parts.utils.streaming_utils import CacheAwareStreamingAudioBuffer

    model = EncDecRNNTBPEModel.restore_from(str(args.model), map_location="cpu")
    configure_cache_export(model)
    for index, row, audio_path, resume_key in pending:
        clip_id = str(row["id"])
        buffer = CacheAwareStreamingAudioBuffer(
            model, online_normalization=True, pad_and_drop_preencoded=True
        )
        buffer.append_audio_file(str(audio_path))
        features = buffer.buffer.detach().cpu().numpy().astype(np.float32, copy=False)
        feature_path = args.out / f"{clip_id}.npy"
        save_npy_atomic(feature_path, features)
        receipt = {
            "resume_key": resume_key,
            "status": "ok",
            "id": clip_id,
            "audio_sha256": row["sha256"],
            "duration_s": row["duration_s"],
            "config_sha256": config_hash,
            "feature_file": feature_path.name,
            "feature_sha256": sha256_file(feature_path),
            "shape": list(features.shape),
            "dtype": str(features.dtype),
        }
        ledger.append(receipt)
        print(f"[{index}/{len(rows)}] {clip_id} shape={list(features.shape)}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
