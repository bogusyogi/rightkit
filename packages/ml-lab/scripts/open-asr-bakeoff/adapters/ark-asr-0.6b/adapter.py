#!/usr/bin/env python3
"""Bakeoff inference adapter -- ARK-ASR-0.6B (AutoArk-AI, transformers-family).

Official inference reference (fetched 2026-07-17, ground-truthed against the
locally downloaded snapshot 2026-07-17):
  https://huggingface.co/AutoArk-AI/ARK-ASR-0.6B
  AutoProcessor.apply_chat_template(...) + AutoModelForCausalLM.generate(...),
  both with trust_remote_code=True (custom `arkasr` remote code).
See README.md in this directory for the exact source excerpt and the
architectural 30-second audio-length cap caveat.

Contract: ../_contract.md (SHARED I/O CONTRACT). Shared loader/runner logic
lives in ./ark_common.py (also imported by ../ark-asr-3b/adapter.py -- both
checkpoints ship the identical custom loader/API, see ark_common.py's
docstring for the verification).

This file authors code only. It does NOT install packages, download
weights, or run inference by itself -- execution is gated on the frozen run
packet's approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) and on the
acquisition lane having populated --model-dir.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ark_common  # noqa: E402

SPEC = ark_common.ArkModelSpec(
    model_key="ark-asr-0.6b",
    repo_id="AutoArk-AI/ARK-ASR-0.6B",
    pinned_revision="45776b56d58cdfb2e2eb632f7e110f38684633e0",
    default_model_dir=Path(__file__).resolve().parents[5] / ".cache" / "asr-models" / "ark-asr-0.6b",
    default_dtype_cuda="float16",  # official README: "torch.float16 if device == 'cuda' else torch.float32"
    hidden_size_fallback=ark_common.FALLBACK_HIDDEN_SIZE_0_6B,
    num_layers_fallback=ark_common.FALLBACK_NUM_LAYERS_0_6B,
    whisper_dmodel_fallback=ark_common.FALLBACK_WHISPER_DMODEL_0_6B,
    whisper_encoder_layers_fallback=ark_common.FALLBACK_WHISPER_ENCODER_LAYERS_0_6B,
    param_count_fallback=ark_common.FALLBACK_PARAM_COUNT_0_6B,
)


if __name__ == "__main__":
    raise SystemExit(ark_common.run_cli(SPEC))
