#!/usr/bin/env python3
"""Bakeoff inference adapter -- ARK-ASR-3B (AutoArk-AI, transformers-family).

Official inference reference (fetched 2026-07-17):
  https://huggingface.co/AutoArk-AI/ARK-ASR-3B
  AutoProcessor.apply_chat_template(...) + AutoModelForCausalLM.generate(...),
  both with trust_remote_code=True (custom `arkasr` remote code) -- IDENTICAL
  loader/API to the ARK-ASR-0.6B sibling per both model cards and per a diff
  of the two repos' sibling-file manifests (both list the same
  processing_arkasr.py / modeling_arkasr.py / modeling_audio.py /
  configuration_arkasr.py). See ../ark-asr-0.6b/ark_common.py's docstring
  for the verification and ../ark-asr-0.6b/README.md for the ground-truthed
  source excerpt (this repo's own 3B snapshot was not locally downloaded at
  authoring time -- see README.md here for what that means for the
  preflight estimate).

This adapter is a thin CLI shim: all loader/runner logic is shared with the
0.6B sibling via ../ark-asr-0.6b/ark_common.py (per the task's explicit
instruction to factor shared code into ark_common.py in the 0.6B dir and
import it from 3B, rather than duplicating ~500 lines of identical logic).

Contract: ../_contract.md (SHARED I/O CONTRACT).

This file authors code only. It does NOT install packages, download
weights, or run inference by itself -- execution is gated on the frozen run
packet's approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) and on the
acquisition lane having populated --model-dir. Per the run packet's dispatch
schedule this lane is Batch C / "Deferred; requires a separate larger-host
packet" for a full local 8 GB Windows run -- see README.md.
"""
from __future__ import annotations

import sys
from pathlib import Path

_ARK_0_6B_DIR = Path(__file__).resolve().parent.parent / "ark-asr-0.6b"
sys.path.insert(0, str(_ARK_0_6B_DIR))
import ark_common  # noqa: E402

SPEC = ark_common.ArkModelSpec(
    model_key="ark-asr-3b",
    repo_id="AutoArk-AI/ARK-ASR-3B",
    pinned_revision="1e28271b79edc97635783bea65abc89195a09ed3",
    default_model_dir=Path(__file__).resolve().parents[5] / ".cache" / "asr-models" / "ark-asr-3b",
    default_dtype_cuda="bfloat16",  # official card: "Dtype support: bfloat16 (GPU) / float32 (CPU)"
    hidden_size_fallback=ark_common.FALLBACK_HIDDEN_SIZE_3B,
    num_layers_fallback=ark_common.FALLBACK_NUM_LAYERS_3B,
    whisper_dmodel_fallback=ark_common.FALLBACK_WHISPER_DMODEL_3B,
    whisper_encoder_layers_fallback=ark_common.FALLBACK_WHISPER_ENCODER_LAYERS_3B,
    param_count_fallback=ark_common.FALLBACK_PARAM_COUNT_3B,
)


if __name__ == "__main__":
    raise SystemExit(ark_common.run_cli(SPEC))
